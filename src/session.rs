//! The loop: fold → request → stream → log → tools → boundaries → repeat.
//!
//! One session is one process and one append-only log. Every verb that arrives
//! on the session socket is logged and then applied at exactly one
//! [`Boundary`], never between a tool call and its result.

use std::collections::BTreeSet;
use std::io::IsTerminal as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::AsyncReadExt as _;
use tokio::sync::{mpsc, oneshot};

use crate::VERSION;
use crate::chat::ToolCall;
use crate::cli::{AskArgs, CommonArgs, ResumeArgs, RunArgs};
use crate::compact;
use crate::config::{self, Config};
use crate::credentials;
use crate::event::{
    Blobs, EventBody, EventLog, LogEvent, LoggedToolCall, Payload, SystemInfo, read_events,
    spill_payloads,
};
use crate::herdr::{Herdr, State as HerdrState};
use crate::ipc::{self, Incoming, Request, Response};
use crate::provider::{self, Client};
use crate::resume::{self, UnsettledCall};
use crate::tools::{self, Ctx, ToolOutcome};
use crate::tui::{self, UiEvent};
use crate::verbs::{self, Boundary, Pending, Scheduler, Verb};

/// How long a stopped call may take to report back before the session settles
/// it on the user's behalf. The kill is immediate; this is for the process tree
/// to unwind. The `say --now` requirement is "within 2 s".
///
/// It is armed **per call**: a `stop --call` never bounds a sibling's run
/// (M1 review F1).
const STOP_GRACE: Duration = Duration::from_millis(1500);
/// While a stop is settling, look for calls that started just after the kill
/// and take them down too.
const STOP_POLL: Duration = Duration::from_millis(25);
/// How often a request waiting for a concurrency slot looks again. The slot
/// appears when another process closes its handle; there is nothing to be told
/// about, and a person watching sees a shorter delay rather than a poll rate.
const SLOT_POLL: Duration = Duration::from_millis(100);
/// How long an idle session sleeps between looks for a metadata fetch that
/// started with the session, so `state` carries the provider's window as soon
/// as it is there instead of at the next turn (STEER-1 review S2). Short,
/// because a session with no turn running has nothing better to do.
const IDLE_MODELS_WAIT: Duration = Duration::from_millis(100);

/// What a resume says to a log whose last turn ended cleanly but was never
/// closed: the model's last message is its own answer, and a request may not
/// end on one (M3 review S4).
const CONTINUE_NOTE: &str = "[kelir] the session was interrupted between turns; continue the task \
                             from where it stopped.";

type Inbox = mpsc::UnboundedReceiver<Incoming>;

pub async fn run(args: RunArgs) -> u8 {
    drive(Options {
        json: args.json,
        ask: false,
        keep: args.keep,
        tui: false,
        common: args.common,
        task: args.task,
        resume: args.resume,
    })
    .await
}

/// `kelir resume <id> [task]`: continue a session from its log.
pub async fn resume(args: ResumeArgs) -> u8 {
    drive(Options {
        json: args.json,
        ask: false,
        keep: args.keep,
        tui: false,
        common: args.common,
        task: args.task,
        resume: Some(args.id),
    })
    .await
}

pub async fn ask(args: AskArgs) -> u8 {
    drive(Options {
        json: false,
        ask: true,
        keep: false,
        tui: false,
        common: args.common,
        task: args.question,
        resume: None,
    })
    .await
}

/// `kelir` with no subcommand, or `kelir tui`: the interactive session.
pub async fn tui(args: crate::cli::TuiArgs) -> u8 {
    if !std::io::stdout().is_terminal() {
        eprintln!(
            "kelir: no terminal: `kelir` is the interactive session and needs a TTY \
             (use `kelir run <task>` or `kelir ask <question>` in a script)"
        );
        return 2;
    }
    drive(Options {
        json: false,
        ask: false,
        // The TUI stays up between turns, like `run --keep`, and a failed turn
        // never ends it.
        keep: true,
        tui: true,
        common: args.common,
        // The TUI has no task: the first turn starts when a human sends a
        // message. `TuiArgs` has no positional, so there is none to reject.
        task: Vec::new(),
        resume: args.resume,
    })
    .await
}

/// `kelir view <path>`: queue an image for the model's next message.
///
/// The model reaches this through `bash` (so it costs no tool-schema text). It
/// sends the `view` verb to the live session over the socket the bash call
/// already carries (`KELIR_SOCK`); the session checks the model's vision
/// metadata and records a `source` event (kind `attachment`) at the tail. That
/// is what keeps the provider cache prefix byte-stable: earlier messages are
/// never edited.
pub async fn view(args: crate::cli::ViewArgs) -> u8 {
    let Some(address) =
        crate::envs::var_os("KELIR_SOCK").map(|value| value.to_string_lossy().into_owned())
    else {
        eprintln!("kelir: view: no session: run it from a kelir tool call (KELIR_SOCK is not set)");
        return 1;
    };
    let request = Request::new("view", json!({"path": args.path}), "tool");
    let response = match ipc::request_value(&address, &request).await {
        Ok(response) => response,
        Err(error) => {
            eprintln!("kelir: view: cannot reach the session: {error}");
            return 1;
        }
    };
    if response.ok {
        println!("image queued: appears in your next message");
        0
    } else {
        let message = response
            .error
            .unwrap_or_else(|| "the session refused the image".into());
        eprintln!("kelir: {message}");
        1
    }
}

struct Options {
    json: bool,
    ask: bool,
    keep: bool,
    /// Interactive: an inline viewport owns the terminal and every turn is
    /// rendered through `tui::UiEvent`s instead of stdout.
    tui: bool,
    common: CommonArgs,
    /// The task words from the command line (`run`, `ask`); empty for the TUI.
    task: Vec<String>,
    /// Continue this session (id or unique prefix) from its log.
    resume: Option<String>,
}

enum Failure {
    /// Exit 2: bad invocation or configuration.
    Usage(String),
    /// Exit 1: the run itself failed.
    Run(String),
}

impl Failure {
    fn code(&self) -> u8 {
        match self {
            Failure::Usage(_) => 2,
            Failure::Run(_) => 1,
        }
    }

    fn message(&self) -> &str {
        match self {
            Failure::Usage(message) | Failure::Run(message) => message,
        }
    }
}

struct Prepared {
    cfg: Config,
    provider_name: String,
    model: String,
    client: Client,
    /// The provider config the client was built from, its key included: the
    /// session keeps it as `active_provider`.
    provider_cfg: crate::config::ProviderConfig,
    /// Where this session's key came from (`env OPENROUTER_API_KEY`,
    /// `opencode auth export opencode`). Recorded in `session_started`.
    key_origin: Option<String>,
    id: String,
    dir: PathBuf,
    task: String,
    cwd: PathBuf,
    /// The frozen system prompt: the text, its hash, and the sources that went
    /// in (`session_started.system` and `sources[]`).
    system: crate::sources::Rendered,
    sock: String,
    json: bool,
    ask: bool,
    keep: bool,
    tui: bool,
    herdr: Option<Herdr>,
    resume: Option<Reopened>,
    /// The window compaction measures against, and where it came from
    /// (`config`, `api`, `cache`, `default`).
    window: u64,
    window_source: &'static str,
    /// Why the window is what it is, when that needs saying: the short reason the
    /// provider's list did not answer, in words (STEER-1 review N4). `None` for
    /// a configured window and for a list that answered.
    window_note: Option<String>,
    /// The provider's model list as this process learned it, so a `model` swap
    /// resolves without the network.
    models: Option<crate::models::Cache>,
    /// A metadata fetch still running (or its answer, waiting to be taken):
    /// resolved at the next `before_step`, so a slow list never holds the first
    /// request (STEER-1 review S2).
    models_fetch: Option<Fetch>,
    /// The opt-in concurrency governor for the provider this session starts
    /// with (`max_concurrent_requests`); `None` when the provider config has no
    /// limit.
    governor: Option<crate::governor::Governor>,
}

/// What a resume reads out of the log before the session continues it.
struct Reopened {
    events: Vec<LogEvent>,
    /// The log was not closed cleanly (a turn was running, or the process was
    /// killed between turns): resume continues from the model's next step.
    in_flight: bool,
    /// Calls the log shows as requested but never settled.
    unsettled: Vec<UnsettledCall>,
    /// Torn or unreadable log lines, reported as `error` events.
    damage: Vec<String>,
    step: u32,
    turns: u32,
    /// The tool names frozen at `session_started`, so a resumed session sends
    /// the same tool list (and keeps its cache prefix).
    tools: Vec<String>,
    /// The frozen system text; kept so the log is never rewritten.
    system_text: String,
    /// The sources the frozen system was rendered from, as `session_started`
    /// recorded them: the watcher re-checks exactly these.
    sources: Vec<crate::event::SourceInfo>,
    cwd: PathBuf,
}

struct Session {
    id: String,
    dir: PathBuf,
    writer: EventLog,
    blobs: Blobs,
    events: Vec<LogEvent>,
    cfg: Config,
    provider_name: String,
    model: String,
    client: Client,
    /// The config of the provider the client is talking to, with its key: the
    /// one a re-read or a `model` verb starts from. Held here (not only in
    /// `cfg.providers`, whose entry has no key) because the key lives in memory
    /// and nowhere else.
    active_provider: crate::config::ProviderConfig,
    ctx: Ctx,
    json: bool,
    echo_assistant: bool,
    keep: bool,
    /// Interactive: output goes to the inline viewport, not to stdout.
    tui: bool,
    /// The UI, when one is attached. Every event is best-effort: a dead UI
    /// never fails a turn.
    ui: Option<mpsc::UnboundedSender<UiEvent>>,
    /// Herdr's OSC 7501 status, when this session reports one.
    herdr: Option<Herdr>,
    /// A task was given on the command line (the TUI starts with none).
    has_task: bool,
    final_text: String,
    /// Session-wide step counter: every `step_started` in a `--keep` session
    /// keeps counting, so step numbers stay unique in one log.
    step: u32,
    /// The turn being run (0 before the first one).
    turn: u32,
    /// Steps taken in the turn now running, reset at every turn start: an
    /// explicit `max_steps` cap counts these, not the session-wide `step`.
    turn_steps: u32,
    /// Tools turned off for the rest of the session. The list sent to the
    /// provider never changes.
    disabled: BTreeSet<String>,
    /// The provider call ids of this batch that have **not** settled yet, so a
    /// `stop --call` for a call that is done is refused (M1 review F2).
    live_calls: Vec<String>,
    /// Verbs waiting for their boundary.
    scheduler: Scheduler,
    /// A compaction waiting for its boundary (`before_step`).
    compact_request: Option<CompactRequest>,
    /// The last `usage.prompt_tokens` for the current model: the honest size
    /// of the context the auto trigger compares to the window. Reset after a
    /// compaction, so the next trigger uses the estimate of the smaller fold.
    last_prompt_tokens: Option<u64>,
    /// The size at which an automatic compaction was declined because it would
    /// not shrink the context (M3 review N1). The trigger stays quiet until the
    /// context has grown past it, instead of paying for a summary every step.
    compact_floor: Option<u64>,
    /// The tool names frozen at `session_started`; a resumed session sends the
    /// same list, so its cache prefix is unchanged.
    tool_names: Vec<String>,
    /// This process reopened the log instead of writing `session_started`.
    resumed: bool,
    /// The log was not closed cleanly: a turn is continued on resume.
    in_flight: bool,
    /// The last streamed step failed with an error the provider called
    /// retryable: a resumed turn may try the turn again (see [`Session::run`]).
    retryable_failure: bool,
    /// The task words from the command line, for the `user` event a resume
    /// adds when it is given a new message.
    pending_task: String,
    /// The window compaction measures the context against, and where it came
    /// from (`config`, `api`, `cache`, `default`).
    window: u64,
    window_source: &'static str,
    /// The model the window was last applied for: the startup answer is
    /// silent, a later model's answer is announced once.
    window_model: Option<String>,
    /// See [`Prepared::window_note`].
    window_note: Option<String>,
    /// The provider's model list as this process learned it.
    models: Option<crate::models::Cache>,
    /// A metadata fetch still running (or its answer, waiting to be taken);
    /// applied at the next `before_step`.
    models_fetch: Option<Fetch>,
    /// Body fields a provider refused with a 400 during this session, by the
    /// field name: the retry leaves them out and the session remembers, so the
    /// same 400 is not paid twice (STEER-6 review S3). A provider that rejects
    /// the field used to make every step a hard error until the person found
    /// the config key.
    refused_fields: BTreeSet<String>,
    /// The sources in this context, re-checked at every boundary: an edit on
    /// disk becomes a `source` message, never a system rewrite.
    watch: crate::sources::Watch,
    /// The client-side concurrency governor, when the provider config asks for
    /// one (`max_concurrent_requests`). `None` is off, the default.
    governor: Option<crate::governor::Governor>,
    /// This session's socket trust boundary: its token and the jobs it owns
    /// (STEER-7). The `bg`, `cancel` and `bg-spawn` verbs are refused here
    /// when they name a job this session did not start.
    trust: std::sync::Arc<crate::trust::Trust>,
    /// Job ids whose status file was refused (unsigned or badly signed): the log
    /// records each once, so a child rewriting a file cannot grow it.
    refused_status: std::collections::HashSet<String>,
    /// The highest signed sequence number believed per job.
    seen_seq: std::collections::HashMap<String, u64>,
}

/// A compaction asked for by someone; it applies at the next `before_step`.
struct CompactRequest {
    by: &'static str,
    focus: Option<String>,
}

/// What a compaction did, for the UI line.
struct CompactedInfo {
    through_seq: u64,
    tokens_before: u64,
    tokens_after: u64,
    pinned: usize,
}

/// What one compaction attempt ended as.
enum CompactRun {
    /// Nothing old enough to summarise.
    Nothing,
    /// A `compacted` event was written.
    Applied(CompactedInfo),
    /// The context would not shrink, so nothing was written (N1); `would_be` is
    /// the size the fold would have had.
    Skipped { before: u64, would_be: u64 },
    /// A verb interrupted the summarising request.
    Stopped(String),
    /// The `end` verb arrived during the summarising request.
    End,
}

/// The result the `compact` tool call itself gets: the work happens at the next
/// boundary, not inside the call.
fn compact_queued(focus: &Option<String>) -> String {
    match focus {
        Some(focus) => format!(
            "compaction queued (focus: {focus}): the earlier conversation is summarised at the \
             next step; the task and the messages a human typed stay verbatim"
        ),
        None => "compaction queued: the earlier conversation is summarised at the next step; the \
                 task and the messages a human typed stay verbatim"
            .to_string(),
    }
}

/// Is this process armed to die at `point`? The documented test hook
/// (`KELIR_CRASH_AT=<point>`) is a no-op unless it names this point exactly.
fn crash_armed(point: &str) -> bool {
    crate::envs::var_os("KELIR_CRASH_AT")
        .map(|value| value.to_string_lossy().trim() == point)
        .unwrap_or(false)
}

/// Die the way a crash does: abort, no unwinding, no `session_ended`.
fn crash_now(point: &str) -> ! {
    let _ = writeln!(
        std::io::stderr(),
        "kelir: KELIR_CRASH_AT={point}: aborting (test hook)"
    );
    let _ = std::io::stderr().flush();
    std::process::abort()
}

fn crash_at(point: &str) {
    if crash_armed(point) {
        crash_now(point);
    }
}

/// What a turn ended as.
enum TurnEnd {
    Done,
    Stopped(String),
    /// The turn paused and waits for a human. Not a failure: the session
    /// stays alive (`--keep`) and the next message opens a new turn. The log
    /// says `turn_ended` outcome `paused` with the report's `reason`
    /// (`steps`: the explicit `max_steps` cap; `provider`: the provider stayed
    /// unavailable past the waiting budget, so the message says how to go on).
    Paused(PauseReport),
    /// The `end` verb arrived.
    End,
}

/// What the next turn should be.
enum TurnStart {
    Messages(Vec<Message>),
    End,
}

enum Idle {
    Turn,
    End,
}

/// A `user` event to inject at a boundary.
struct Message {
    text: String,
    delivery: &'static str,
    from: String,
}

/// What a boundary did.
#[derive(Default)]
struct Applied {
    /// The `end` verb was applied.
    end: bool,
    /// The `stop` verb ended the turn, with this reason.
    stopped: Option<String>,
    /// How many messages were injected (a `say` means another step).
    messages: usize,
}

/// What the provider stream ended as.
enum StreamEnd {
    Assembled(crate::stream::Assembler),
    Stopped(String),
    End,
    /// The provider stayed unavailable past the request's waiting budget: the
    /// turn pauses (it does not fail) and the next `say` retries from the same
    /// fold.
    Paused(PauseReport),
}

/// What a pause says happened, for `turn_ended.final_text` (and its `reason`).
struct PauseReport {
    /// `paused` because of this: an explicit `max_steps` cap (`steps`) or an
    /// unavailable provider (`provider`).
    reason: PauseReason,
    /// Provider requests made inside the budget (`provider` only).
    attempts: u32,
    /// How long the waits inside the budget took (`provider` only).
    waited: Duration,
    /// The last failure's short reason ("rate limited (429)"), `provider` only.
    detail: String,
}

/// The two ways a turn pauses; both log `outcome: "paused"`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PauseReason {
    /// An explicit `max_steps` cap was reached.
    Steps,
    /// The provider stayed unavailable past the waiting budget.
    Provider,
}

impl PauseReason {
    fn as_str(self) -> &'static str {
        match self {
            PauseReason::Steps => "steps",
            PauseReason::Provider => "provider",
        }
    }
}

impl PauseReport {
    /// The step cap: "turn paused after 40 steps; say continue to resume".
    fn steps(steps: u32) -> PauseReport {
        PauseReport {
            reason: PauseReason::Steps,
            attempts: steps,
            waited: Duration::ZERO,
            detail: String::new(),
        }
    }

    /// "provider unavailable after 7 attempts / 30 min: rate limited (429);
    /// say continue to retry".
    fn message(&self) -> String {
        if self.reason == PauseReason::Steps {
            return format!(
                "turn paused after {} {}; say continue to resume",
                self.attempts,
                if self.attempts == 1 { "step" } else { "steps" }
            );
        }
        let mut text = format!(
            "provider unavailable after {} attempt{}, {}: ",
            self.attempts,
            if self.attempts == 1 { "" } else { "s" },
            crate::retry::seconds_text(self.waited)
        );
        text.push_str(&self.detail);
        text.push_str("; say continue to retry");
        text
    }
}

/// Where a request comes from, so it can be rebuilt after a wait: a `model`
/// verb during the wait changes the model (and the provider), and the next
/// attempt has to use it.
enum RequestFor<'a> {
    /// A step: the fold, built from the session's current model.
    Step,
    /// The compaction's own request: it is built once, from the model it was
    /// made with.
    Built(&'a provider::BuiltRequest),
}

impl RequestFor<'_> {
    fn build(&self, session: &Session) -> provider::BuiltRequest {
        match self {
            RequestFor::Built(request) => (*request).clone(),
            RequestFor::Step => {
                // The fold cannot fail here for a reason a rebuild would fix;
                // an unreadable blob is the parser's error to report.
                let messages = crate::fold::fold(&session.events, &session.dir)
                    .map(|folded| folded.request_messages())
                    .unwrap_or_default();
                // The same builder `drive_turn` used for the first attempt, so
                // a retry keeps STEER-6's capability decisions (`parallel_calls`,
                // the `reasoning_effort` gate and the fields this session already
                // saw a 400 for) as well as STEER-5's rebuild after a wait.
                session.step_request(&messages)
            }
        }
    }
}

/// What [`Session::accept_at_once`] did with a verb.
enum AtOnce {
    /// Accepted and applied now; the turn may have ended because of it.
    Applied(Option<Box<StreamEnd>>),
    /// Refused (the client has its error); nothing was applied, so nothing
    /// about the running turn changed.
    Rejected,
}

/// Does this request ask for a verb that is applied the moment it arrives, and
/// so is answered only after it landed? `say --now` and `model` (STEER-5 review
/// N1). Read from the raw request, before it is parsed: the answer needs to know
/// which path to take.
fn at_once_verb(request: &Request) -> bool {
    match request.verb.as_str() {
        "model" => true,
        "say" => request
            .args
            .get("now")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => false,
    }
}

/// What one provider attempt did.
enum Attempt {
    Ok(crate::stream::Assembler),
    Failed(provider::ProviderError),
    /// A verb ended the turn while the request was in flight.
    End(StreamEnd),
}

/// What a retry or slot wait ended as.
enum Waited {
    /// The wait ran out: try again.
    Timeout,
    /// A verb applied at once ends the wait and the attempt is re-sent right
    /// away (`say --now`, `model`): the request is a new one, so it is rebuilt
    /// from the fold the verb just changed.
    Retry,
    /// A verb (or a signal) ended the turn.
    End(Box<StreamEnd>),
}

/// What waiting for a governor slot ended as: a slot (or none, when the
/// governor is off), or a verb ending the turn.
enum SlotWait {
    Taken(Option<crate::governor::Slot>),
    Interrupted(StreamEnd),
}

/// What a tool batch ended as.
#[derive(Default)]
struct Batch {
    stopped: Option<String>,
    end: bool,
}

async fn drive(opts: Options) -> u8 {
    // One interrupt listener for the process, installed before *anything*
    // observable: before `prepare` (which records `session_started`) and so
    // before any client can see the session and signal it. Tokio's `signal`
    // keeps Ctrl-C captured for the whole process once it exists, so this is
    // also the last safe moment to install it; the flag it sets is
    // level-triggered, so a signal that lands before the first wait is still
    // there when the wait begins (STEER-5 fixes round 2).
    crate::signals::install();
    let mut prepared = match prepare(&opts).await {
        Ok(prepared) => prepared,
        Err(failure) => {
            eprintln!("kelir: {}", failure.message());
            // A scripted caller reads the result line, even for a usage error.
            if opts.json {
                print_result_event(
                    "error",
                    failure.code(),
                    "",
                    0,
                    None,
                    Some(failure.message()),
                );
            }
            return failure.code();
        }
    };

    // One session is one writer. Take the log's lock before anything is
    // appended: on Unix the "already running" connect check and the bind are
    // two steps, and two resumes started together can both pass the first one
    // (M3 review N3). The guard lives until `drive` returns.
    let _lock = match ipc::lock(&prepared.dir) {
        Ok(lock) => lock,
        Err(message) => {
            let message = format!("session {} is already running ({message})", prepared.id);
            eprintln!("kelir: {message}");
            if opts.json {
                print_result_event("error", 1, "", 0, None, Some(&message));
            }
            return 1;
        }
    };

    // Bind before `session_started` is written, so a client that sees the event
    // can connect.
    let listener = match ipc::bind(&prepared.dir, &prepared.id) {
        Ok(listener) => listener,
        Err(message) => {
            eprintln!("kelir: {message}");
            if opts.json {
                print_result_event("error", 1, "", 0, None, Some(&message));
            }
            return 1;
        }
    };
    // On Unix a session dir whose path does not fit in `sun_path` binds a
    // shorter socket and records it (M1 Linux round 1); take the address that
    // was really bound for `KELIR_SOCK`.
    prepared.sock = ipc::value(&prepared.dir, &prepared.id);

    // The trust boundary (STEER-7): a token for this session, and the registry
    // of the jobs it may be told about. Made before anything can connect, so
    // there is no window in which the socket answers without one.
    // The session holds every job's signing key for as long as it lives, so it
    // is closed to its own user's memory reads before the first key exists
    // (Windows: a DACL that keeps terminate/limited-query for the owner;
    // Linux: not dumpable). Best effort, said on stderr, never fatal
    // (STEER-7 review round 4, S2).
    if let Err(error) = crate::bg::harden_process() {
        eprintln!("kelir: warning: {error}");
    }
    let trust = match crate::trust::Trust::create(prepared.dir.clone()) {
        Ok(trust) => std::sync::Arc::new(trust),
        Err(message) => {
            eprintln!("kelir: {message}");
            if opts.json {
                print_result_event("error", 1, "", 0, None, Some(&message));
            }
            return 1;
        }
    };

    let mut session = match Session::open(prepared, trust.clone()) {
        Ok(session) => session,
        Err(message) => {
            eprintln!("kelir: {message}");
            if opts.json {
                print_result_event("error", 1, "", 0, None, Some(&message));
            }
            return 1;
        }
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<Incoming>();
    ipc::serve(listener, trust.clone(), tx.clone());
    // Background jobs: `kelir bg run` reports itself over the socket, and this
    // watcher is the safety net that also catches a promoted call settling
    // while the session runs. Both arrive as one `bg` verb (see [`Session::note_bg`]).
    watch_bg(session.dir.join("bg"), tx.clone());

    // The interactive session: the TUI runs as a task beside the loop, and all
    // it can do is send verbs into the same channel the socket feeds.
    let mut tui = None;
    if opts.tui {
        let (to_tui, from_session) = mpsc::unbounded_channel::<UiEvent>();
        let mut banner = vec![
            tui::BannerLine::plain(format!(
                "kelir · {} · {}",
                session.provider_name, session.model
            )),
            tui::BannerLine::plain(format!("session {}", session.id)),
            tui::BannerLine::plain(format!(
                "cwd {}",
                crate::util::path_string(&session.ctx.cwd)
            )),
        ];
        // The window is not ambient: the number is for `state`, not the banner
        // (tui3). What does belong here is a window that is *not* the
        // provider's, with the reason: a 401, an offline endpoint or a default
        // fallback silently changes the compaction threshold, and a person who
        // cannot see that is being misled (F-window, STEER-1 review N4). One
        // dim line, only when the start actually fell back.
        if let Some(line) = window_banner_line(&session) {
            banner.push(tui::BannerLine::dim(line));
        }
        banner.push(tui::BannerLine::plain(
            "Enter sends · Alt-Enter adds a line · /help shows the commands",
        ));
        // The cap is worth a banner line only when one is set: the status line
        // carries the count ("step 3/5") while a turn runs.
        if let Some(cap) = session.cfg.max_steps {
            banner.push(tui::BannerLine::plain(format!(
                "step cap {cap} per turn · reaching it pauses the turn (say continue)"
            )));
        }
        if session.resumed {
            banner.push(tui::BannerLine::plain(format!(
                "resumed from the event log · {} turn(s) recorded{}",
                session.turn,
                if session.in_flight {
                    " · continuing the turn that was cut off"
                } else {
                    ""
                }
            )));
        }
        banner.push(tui::BannerLine::plain(String::new()));
        let token = session.ctx.token.clone();
        match tui::start(
            from_session,
            to_tui.clone(),
            tx.clone(),
            &banner,
            session.cfg.max_steps,
            token.clone(),
            crate::theme::Theme::from_env(),
        ) {
            Ok(mut handle) => {
                // The session dir is where a pasted image is saved, and where
                // `attachments/…` in a prompt resolves.
                handle.app.set_session_dir(session.dir.clone());
                session.ui = Some(to_tui.clone());
                let task = tokio::spawn(handle.run());
                // If the UI dies (a panic, a terminal error) the session ends
                // the same way `/quit` ends it, instead of running forever with
                // nobody watching.
                let verbs = tx.clone();
                let watchdog = tokio::spawn(async move {
                    let _ = task.await;
                    let (reply, _answer) = oneshot::channel();
                    let _ = verbs.send(Incoming {
                        // The UI runs inside the session, so its verb carries
                        // the token from memory like the TUI's own dispatch.
                        request: Request::new("end", json!({}), "tui")
                            .with_token(Some(token.clone())),
                        reply,
                        action: None,
                    });
                });
                tui = Some((watchdog, to_tui));
            }
            Err(error) => {
                // Raw mode may already be on; leave the terminal usable.
                let _ = ratatui::try_restore();
                eprintln!("kelir: cannot start the terminal UI: {error}");
                return 1;
            }
        }
    }

    let code = session.run(&mut rx).await;
    if let Some((watchdog, to_tui)) = tui {
        // One last paint, then the terminal goes back to the shell.
        let _ = to_tui.send(UiEvent::Quit);
        let _ = watchdog.await;
    }
    if opts.ask && !session.final_text.is_empty() {
        println!("{}", session.final_text);
    }
    session.print_result(code);
    code
}

/// Watch this session's `bg/` for jobs that start or settle, and push each
/// change into the same channel the socket feeds. The session stays the only
/// writer of the log: the watcher only says what happened, and the session
/// records it at the next boundary (or immediately when it is idle).
fn watch_bg(dir: PathBuf, tx: mpsc::UnboundedSender<Incoming>) {
    tokio::spawn(async move {
        let mut seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        loop {
            tokio::time::sleep(crate::bg::WATCH).await;
            for status in crate::bg::list(&dir) {
                if !status.is_job() {
                    continue;
                }
                if !seen.insert((status.id.clone(), status.state.clone())) {
                    continue;
                }
                let (reply, _answer) = oneshot::channel();
                let action = crate::ipc::Action::Note(status);
                let incoming = Incoming {
                    request: Request::new("bg-watch", serde_json::Value::Null, "job"),
                    reply,
                    action: Some(action),
                };
                if tx.send(incoming).is_err() {
                    return;
                }
            }
        }
    });
}

/// The provider's model list and how this process came by it.
///
/// `cache` is the list kelir is using now (a fresh one from disk, a list this
/// process fetched, or a failed one kept for its `error`), `fetched` says it
/// came off the wire in this process, and `why` is the short reason the window
/// fell back to `default` — `None` when the list answered
/// (STEER-1 review N4).
struct ModelList {
    cache: Option<crate::models::Cache>,
    fetched: bool,
    why: Option<String>,
}

impl ModelList {
    fn from_cache(cache: Option<crate::models::Cache>) -> Self {
        Self {
            cache,
            fetched: false,
            why: None,
        }
    }

    fn with_why(mut self, why: Option<String>) -> Self {
        self.why = why;
        self
    }
}

/// A `GET /models` running beside the session, never on its critical path.
///
/// The task is spawned at session start (STEER-1 review S2) and sends its answer
/// through a channel: the start takes the answer if it is there within
/// [`crate::models::START_GRACE`] (so the first request carries the capability
/// data when the endpoint is quick), and the first `before_step` — where the
/// *window* is first read, after the first step — takes it otherwise. The
/// channel is what makes the grace non-destructive: running out of grace does
/// not cancel the fetch, it merely stops waiting for it.
struct Fetch {
    rx: Option<oneshot::Receiver<ModelList>>,
}

impl Fetch {
    fn none() -> Self {
        Self { rx: None }
    }

    fn pending(rx: oneshot::Receiver<ModelList>) -> Self {
        Self { rx: Some(rx) }
    }

    fn is_running(&self) -> bool {
        self.rx.is_some()
    }

    /// The answer, if the fetch has already finished. Never waits: a fetch that
    /// is still running costs this call nothing at all.
    fn take_ready(&mut self) -> Option<ModelList> {
        let rx = self.rx.as_mut()?;
        match rx.try_recv() {
            Ok(list) => {
                self.rx = None;
                Some(list)
            }
            Err(oneshot::error::TryRecvError::Empty) => None,
            Err(oneshot::error::TryRecvError::Closed) => {
                // The task ended without sending (it panicked): nothing to
                // apply, and the cache on disk is what the session keeps using.
                self.rx = None;
                Some(ModelList::from_cache(None))
            }
        }
    }

    /// Wait at most `budget` for the answer. Running out of budget leaves the
    /// fetch running for the next boundary.
    async fn wait(&mut self, budget: std::time::Duration) -> Option<ModelList> {
        let rx = self.rx.as_mut()?;
        match tokio::time::timeout(budget, rx).await {
            Ok(Ok(list)) => {
                self.rx = None;
                Some(list)
            }
            Ok(Err(_)) => {
                self.rx = None;
                Some(ModelList::from_cache(None))
            }
            // Not ready: keep the receiver, and the fetch keeps running.
            Err(_) => None,
        }
    }
}

/// Spawn the fetch, unless the cache on disk already answers.
fn start_fetch(client: &Client, provider: &str, endpoint: &str) -> Fetch {
    let now = crate::models::now_ms();
    let cached = crate::models::load(endpoint);
    if let Some(cache) = &cached
        && (cache.is_fresh(now) || cache.in_backoff(now))
    {
        return Fetch::none();
    }
    let client = client.clone();
    let provider = provider.to_string();
    let endpoint = endpoint.to_string();
    let (tx, rx) = oneshot::channel();
    // The task holds only the sender, so dropping the session (or the grace)
    // never cancels it mid-write; a receiver that is gone just loses the send.
    tokio::spawn(async move {
        let list = fetch_models(&client, &provider, &endpoint, cached).await;
        let _ = tx.send(list);
    });
    Fetch::pending(rx)
}

/// One fetch of `GET /models`, its result written beside the session state.
///
/// A failure is remembered for [`crate::models::RETRY_AFTER_MS`] so an offline
/// box pays it once an hour, not on every start; a *timeout* is not a failure
/// (the endpoint was slow, not wrong), so the next start asks again and the
/// cached list keeps its own age. Every failure also records why, which is what
/// `state` and the banner show instead of a silent `default` (N4).
async fn fetch_models(
    client: &Client,
    provider: &str,
    endpoint: &str,
    cached: Option<crate::models::Cache>,
) -> ModelList {
    let now = crate::models::now_ms();
    let base_url = client.base_url();
    match tokio::time::timeout(crate::models::FETCH_TIMEOUT, client.models()).await {
        Ok(Ok(models)) => {
            let cache = crate::models::Cache::new(provider, &base_url, models, now);
            let _ = crate::models::store(&cache);
            ModelList {
                cache: Some(cache),
                fetched: true,
                why: None,
            }
        }
        Ok(Err(reason)) => {
            let mut cache = cached
                .unwrap_or_else(|| crate::models::Cache::new(provider, &base_url, Vec::new(), 0));
            cache.endpoint = endpoint.to_string();
            cache.failed_at_ms = Some(now);
            cache.error = Some(reason.clone());
            let _ = crate::models::store(&cache);
            ModelList {
                cache: Some(cache),
                fetched: false,
                why: Some(reason),
            }
        }
        // Slow, not wrong: the cached list keeps its age and the next start
        // asks again. Nothing is remembered as a failure.
        Err(_) => ModelList::from_cache(cached).with_why(Some("timeout".to_string())),
    }
}

/// What a start uses before the fetch lands: the cached list (stale numbers
/// beat the default) and the reason `state` gives for it.
fn cached_list(provider: &str, base_url: &str) -> ModelList {
    let endpoint = crate::models::endpoint_id(provider, base_url);
    let flushed = crate::models::load(&endpoint);
    let why = flushed
        .as_ref()
        .and_then(|cache| cache.error.clone())
        .or_else(|| flushed.is_none().then(|| "fetching".to_string()));
    ModelList::from_cache(flushed).with_why(why)
}

/// The banner's one line about the window, when the window is *not* the
/// provider's: a start that fell back to the default (an offline endpoint, a
/// 401, a list that has not landed) measures compaction against
/// [`crate::compact::DEFAULT_WINDOW`] instead of the model's real window, which
/// changes when the session compacts. A 401 or an unreachable endpoint must
/// reach a person without them reading the log (STEER-1 review N4), and tui3's
/// brief still wants the banner free of the *normal* window line (F-window).
///
/// `None` for the provider's own answer and for a configured window: those are
/// not surprises.
fn window_banner_line(session: &Session) -> Option<String> {
    if session.window_source == "api" || session.window_source == "config" {
        return None;
    }
    // "fetching" is not a degradation, it is "the answer is not in yet":
    // [`Session::apply_list`] says the real reason when it lands, so the banner
    // does not name a window it is about to correct (and a start never sees two
    // window lines that way).
    if session.window_note.as_deref() == Some("the model list: fetching") {
        return None;
    }
    match &session.window_note {
        Some(note) => Some(format!(
            "window {} tokens ({}) · {note}",
            session.window, session.window_source
        )),
        // A stale cache or a configured number needs no apology; a fallback to
        // the default with nothing to say about why does.
        None if session.window_source == "default" => Some(format!(
            "window {} tokens (default) · the model list did not answer",
            session.window
        )),
        None => None,
    }
}

/// The list to start with, and the fetch still running for the rest.
///
/// A warm box answers from the cache before the session starts and fetches
/// nothing; a cold one gives the list [`crate::models::START_GRACE`] — enough
/// for a local mock or a fast gateway, so the first request carries the
/// capability data the list answers — and leaves a slower fetch running for the
/// first `before_step` (STEER-1 review S2).
async fn begin_models(client: &Client, provider: &str, base_url: &str) -> (ModelList, Fetch) {
    let endpoint = crate::models::endpoint_id(provider, base_url);
    let mut fetch = start_fetch(client, provider, &endpoint);
    if !fetch.is_running() {
        // Fresh (or a failure still inside its backoff): read from disk.
        let list = ModelList::from_cache(crate::models::load(&endpoint));
        let why = list.cache.as_ref().and_then(|cache| cache.error.clone());
        return (list.with_why(why), fetch);
    }
    let early = cached_list(provider, base_url);
    match fetch.wait(crate::models::START_GRACE).await {
        Some(list) => (list, fetch),
        // Not a failure, a slow endpoint: use what is cached and let the fetch
        // finish beside the session.
        None => (early, fetch),
    }
}

async fn prepare(opts: &Options) -> Result<Prepared, Failure> {
    let common = &opts.common;
    let mut cfg = config::load(common.config.as_deref())
        .map_err(|error| Failure::Usage(error.to_string()))?;
    // `--max-steps N` overrides the file, and `0` there means "no cap" as it
    // does in `config.toml`: a stopgap config must never be able to cap a turn
    // at zero steps.
    if let Some(steps) = common.max_steps {
        cfg.max_steps = (steps > 0).then_some(steps);
    }

    // A resume takes its id, cwd, model and tool list from the log; only the
    // config (windows, prompts, provider keys) comes from today's machine.
    if let Some(spec) = &opts.resume {
        let reopened = reopen(spec, &cfg).await?;
        let task = if opts.task.is_empty() {
            String::new()
        } else {
            task_text(&opts.task).await.map_err(Failure::Usage)?
        };
        let herdr = Herdr::detect(opts.json);
        let base_url = reopened.client.base_url();
        let (list, models_fetch) =
            begin_models(&reopened.client, &reopened.provider_name, &base_url).await;
        let over = cfg.override_window(&reopened.model);
        let (window, window_source) =
            crate::models::resolve(over, list.cache.as_ref(), list.fetched, &reopened.model);
        let window_note = window_note(over, list.why.as_deref(), window_source);
        return Ok(Prepared {
            json: opts.json,
            ask: opts.ask,
            keep: opts.keep,
            tui: opts.tui,
            herdr,
            system: crate::sources::Rendered {
                hash: crate::sources::hash_of(&reopened.reopened.system_text),
                text: reopened.reopened.system_text.clone(),
                sources: reopened.reopened.sources.clone(),
                // A resume renders nothing: the frozen prefix is the log's, and
                // what it says about its sources was recorded when it was made.
                warnings: Vec::new(),
            },
            cwd: reopened.reopened.cwd.clone(),
            cfg,
            provider_name: reopened.provider_name,
            model: reopened.model,
            client: reopened.client,
            provider_cfg: reopened.cfg_provider.clone(),
            // A resume keeps the origin the log recorded.
            key_origin: reopened.key_origin,
            id: reopened.id,
            dir: reopened.dir,
            task,
            // `drive` binds the socket and takes the address it really bound,
            // which also replaces a stale `sock` left by the dead process.
            sock: String::new(),
            resume: Some(reopened.reopened),
            window,
            window_source,
            window_note,
            models: list.cache,
            models_fetch: Some(models_fetch),
            governor: governor_for(&reopened.cfg_provider),
        });
    }

    let provider_name = common
        .provider
        .clone()
        .unwrap_or_else(|| cfg.provider.clone());
    let model = common.model.clone().unwrap_or_else(|| cfg.model.clone());
    let provider_cfg = cfg
        .provider(&provider_name)
        .map_err(|error| Failure::Usage(error.to_string()))?;

    let cwd = match &common.cwd {
        Some(path) if path.is_absolute() => path.clone(),
        Some(path) => std::env::current_dir()
            .map_err(|error| Failure::Run(format!("cannot read the current directory: {error}")))?
            .join(path),
        None => std::env::current_dir()
            .map_err(|error| Failure::Run(format!("cannot read the current directory: {error}")))?,
    };
    if !cwd.is_dir() {
        return Err(Failure::Usage(format!(
            "cwd is not a directory: {}",
            cwd.display()
        )));
    }

    // The TUI has no task: the first turn starts when a human sends a message.
    let task = if opts.tui {
        String::new()
    } else {
        task_text(&opts.task).await.map_err(Failure::Usage)?
    };
    let id = new_session_id();
    // The key is resolved here, at session start, and held in memory only
    // (STEER-2). `config::load` stays pure: reading opencode's store is a
    // process read, and a config load happens in tests and in `doctor` too.
    let mut provider_cfg = provider_cfg;
    let credential = credentials::resolve(&mut provider_cfg).map_err(Failure::Usage)?;
    let client = Client::new(provider_cfg.clone(), &id, cfg.cache_ttl.clone())
        .map_err(|error| Failure::Usage(error.message))?;
    // Model metadata from the provider, cached beside the session state and
    // fetched beside the session (never on the first request's critical path);
    // a `[model_windows]`/`window` setting still wins over it.
    let base_url = client.base_url();
    let (list, models_fetch) = begin_models(&client, &provider_name, &base_url).await;
    let over = cfg.override_window(&model);
    let (window, window_source) =
        crate::models::resolve(over, list.cache.as_ref(), list.fetched, &model);
    let window_note = window_note(over, list.why.as_deref(), window_source);
    let dir = crate::paths::session_dir(&id);
    crate::paths::create_private_dir(&dir.join("commands")).map_err(|error| {
        Failure::Run(format!(
            "cannot create the session directory {}: {error}",
            dir.display()
        ))
    })?;
    let system = render_system(&cfg, &cwd);
    // The address this session is expected to use; `drive` re-reads it after
    // `ipc::bind`, because Unix may fall back to a shorter socket path.
    let sock = ipc::value(&dir, &id);
    let herdr = Herdr::detect(opts.json);

    Ok(Prepared {
        json: opts.json,
        ask: opts.ask,
        keep: opts.keep,
        tui: opts.tui,
        herdr,
        cfg,
        provider_name,
        model,
        client,
        provider_cfg: provider_cfg.clone(),
        key_origin: Some(credential.describe()),
        id,
        dir,
        task,
        cwd,
        system,
        sock,
        resume: None,
        window,
        window_source,
        window_note,
        models: list.cache,
        models_fetch: Some(models_fetch),
        governor: governor_for(&provider_cfg),
    })
}

/// Why this start has the window it has, or `None` when the answer needs no
/// explanation: a configured window is the person's own number, and a fetched
/// or cached list needs nothing said. The one case worth a line is a fallback
/// — the list could not be had, so the start used the default (or a stale
/// cache) and [`crate::models::Cache::error`] says why (STEER-1 review N4).
fn window_note(over: Option<u64>, why: Option<&str>, source: &str) -> Option<String> {
    let why = why?;
    if over.filter(|window| *window > 0).is_some() || source == "api" || source == "cache" {
        return None;
    }
    Some(format!("the model list: {why}"))
}

/// The opt-in governor for one provider config: `max_concurrent_requests`
/// slots, advisory file locks under the state root. `None` (the default) is no
/// limit at all.
fn governor_for(provider: &crate::config::ProviderConfig) -> Option<crate::governor::Governor> {
    provider
        .max_concurrent_requests
        .map(|slots| crate::governor::Governor::new(&provider_base(provider), slots))
}

/// The governor's identity for a provider: its base URL, which is what "one
/// provider" means on the wire (two config names pointing at one endpoint share
/// the endpoint's limit).
fn provider_base(provider: &crate::config::ProviderConfig) -> String {
    provider
        .base_url
        .split("://")
        .nth(1)
        .unwrap_or(&provider.base_url)
        .trim_end_matches('/')
        .to_string()
}

/// What [`reopen`] hands back: the session's identity, provider and log state.
struct ReopenedSession {
    id: String,
    dir: PathBuf,
    provider_name: String,
    model: String,
    client: Client,
    /// Where the log's session said its key came from.
    key_origin: Option<String>,
    /// The config of the provider the log is on: the governor is built from it.
    cfg_provider: crate::config::ProviderConfig,
    reopened: Reopened,
}

/// Read a session's log and prove it is not already running.
async fn reopen(spec: &str, cfg: &Config) -> Result<ReopenedSession, Failure> {
    let id = crate::paths::resolve_session(spec).map_err(Failure::Run)?;
    let dir = crate::paths::session_dir(&id);
    let events_path = dir.join("events.jsonl");
    if !events_path.is_file() {
        return Err(Failure::Run(format!(
            "session {id} has no event log ({})",
            crate::util::path_string(&events_path)
        )));
    }
    let read = read_events(&events_path).map_err(|error| {
        Failure::Run(format!(
            "cannot read {}: {error}",
            crate::util::path_string(&events_path)
        ))
    })?;
    // A live session owns its log; a second writer would fork the sequence.
    // The check is connect-then-lock, which is not one atomic step, so `drive`
    // also takes an exclusive lock on the log; this early answer is what a
    // caller sees. A session whose log already ends with `session_ended` is on
    // its way out — `end` acks before the process releases its socket — so give
    // it a moment instead of refusing to resume a session that has stopped.
    let closed = read
        .events
        .last()
        .map(|event| matches!(event.body, EventBody::SessionEnded { .. }))
        .unwrap_or(false);
    let deadline = Instant::now() + Duration::from_secs(2);
    while ipc::connect(&dir, &id).await.is_ok() {
        if !closed || Instant::now() >= deadline {
            return Err(Failure::Run(format!(
                "session {id} is already running ({}); use `kelir ctl` or stop it first",
                ipc::value(&dir, &id)
            )));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let events = read.events.clone();
    let (mut provider_name, mut model, tools, cwd) = events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::SessionStarted {
                provider,
                model,
                tools,
                cwd,
                ..
            } => Some((
                provider.clone(),
                model.clone(),
                tools.clone(),
                PathBuf::from(cwd),
            )),
            _ => None,
        })
        .ok_or_else(|| {
            Failure::Run(format!(
                "session {id} has no session_started event; its log is not a kelir session"
            ))
        })?;
    // A `model` verb's swap is recorded by the next `step_started`.
    for event in &events {
        if let EventBody::StepStarted {
            provider,
            model: step_model,
            ..
        } = &event.body
        {
            provider_name = provider.clone();
            model = step_model.clone();
        }
    }
    let provider_cfg = cfg
        .provider(&provider_name)
        .map_err(|error| Failure::Usage(error.to_string()))?;
    let mut provider_cfg = provider_cfg;
    credentials::resolve(&mut provider_cfg).map_err(Failure::Usage)?;
    let client = Client::new(provider_cfg.clone(), &id, cfg.cache_ttl.clone())
        .map_err(|error| Failure::Usage(error.message))?;
    let system_text = events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::SessionStarted { system, .. } => Some(system.text.resolve_text(&dir)),
            _ => None,
        })
        .unwrap_or_else(|| Ok(String::new()))
        .unwrap_or_default();
    // The sources `session_started` recorded: the watcher re-checks exactly
    // these, so a resumed session reports the same edits it would have live.
    let sources = events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::SessionStarted { sources, .. } => Some(sources.clone()),
            _ => None,
        })
        .unwrap_or_default();

    Ok(ReopenedSession {
        id: id.clone(),
        dir: dir.clone(),
        provider_name,
        model,
        client,
        key_origin: events.iter().rev().find_map(|event| match &event.body {
            EventBody::SessionStarted { key_origin, .. } => key_origin.clone(),
            _ => None,
        }),
        cfg_provider: provider_cfg,
        reopened: Reopened {
            in_flight: resume::interrupted(&events),
            unsettled: resume::unsettled(&events),
            damage: resume::log_damage(&read),
            step: resume::last_step(&events),
            turns: resume::turns_done(&events),
            tools: if tools.is_empty() {
                tools::names()
            } else {
                tools
            },
            system_text,
            sources,
            cwd,
            events,
        },
    })
}

impl Session {
    fn open(
        prepared: Prepared,
        trust: std::sync::Arc<crate::trust::Trust>,
    ) -> Result<Self, String> {
        let mut prepared = prepared;
        let events_path = prepared.dir.join("events.jsonl");
        let writer = EventLog::open(&events_path)
            .map_err(|error| format!("cannot open {}: {error}", events_path.display()))?;
        let ctx = {
            let mut ctx = Ctx::new(
                prepared.dir.clone(),
                prepared.cwd.clone(),
                prepared.cfg.shell.clone(),
                prepared.cfg.spill_limit,
                prepared.sock.clone(),
            );
            ctx.session_id = prepared.id.clone();
            ctx.promote_after = prepared.cfg.promote_after;
            // Every tool child exports the session's token as `KELIR_TOKEN`
            // (STEER-7), exactly as it already gets `KELIR_SOCK`.
            ctx.token = trust.token().to_string();
            ctx.trust = Some(trust.clone());
            ctx
        };
        let reopened = prepared.resume.is_some();
        let mut session = Session {
            id: prepared.id.clone(),
            dir: prepared.dir.clone(),
            writer,
            blobs: Blobs::new(&prepared.dir),
            events: prepared
                .resume
                .as_ref()
                .map(|read| read.events.clone())
                .unwrap_or_default(),
            cfg: prepared.cfg.clone(),
            provider_name: prepared.provider_name.clone(),
            model: prepared.model.clone(),
            client: prepared.client.clone(),
            active_provider: prepared.provider_cfg.clone(),
            ctx,
            json: prepared.json,
            echo_assistant: !prepared.ask && !prepared.tui,
            keep: prepared.keep,
            tui: prepared.tui,
            ui: None,
            herdr: prepared.herdr,
            has_task: !prepared.task.trim().is_empty(),
            final_text: String::new(),
            step: prepared.resume.as_ref().map(|read| read.step).unwrap_or(0),
            turn: prepared.resume.as_ref().map(|read| read.turns).unwrap_or(0),
            turn_steps: 0,
            disabled: BTreeSet::new(),
            refused_fields: BTreeSet::new(),
            live_calls: Vec::new(),
            scheduler: Scheduler::default(),
            compact_request: None,
            last_prompt_tokens: None,
            compact_floor: None,
            tool_names: prepared
                .resume
                .as_ref()
                .map(|read| read.tools.clone())
                .unwrap_or_else(tools::names),
            resumed: reopened,
            in_flight: prepared
                .resume
                .as_ref()
                .map(|read| read.in_flight)
                .unwrap_or(false),
            retryable_failure: false,
            pending_task: prepared.task.clone(),
            window: prepared.window,
            window_source: prepared.window_source,
            // Seeded from the startup answer, so a later `/model` swap announces
            // a changed window: `apply_list` must see which model the current
            // number belongs to, and a fresh cache starts no fetch at all, so
            // nothing else would tell it (tui3 review, F-window addendum).
            window_model: Some(prepared.model.clone()),
            window_note: prepared.window_note.take(),
            models: prepared.models.take(),
            models_fetch: prepared.models_fetch.take(),
            watch: crate::sources::Watch::default(),
            governor: prepared.governor,
            trust,
            refused_status: std::collections::HashSet::new(),
            seen_seq: std::collections::HashMap::new(),
        };
        // The watcher re-checks the sources the frozen system was rendered
        // from; on a resume it is rebuilt from the log's `sources[]` *and* the
        // `source` events it already carries, so a source the verb added keeps
        // being watched and an edit the log already announces is not announced
        // again (F8).
        if let Some(read) = prepared.resume {
            session.watch = crate::sources::Watch::rebuild(&read.events, &session.dir);
            // A turn that was in flight continues without a new task.
            session.has_task |= read.in_flight;
            session.reopen(read)?;
            return Ok(session);
        }
        session.watch = crate::sources::Watch::from_rendered(&prepared.system);
        session.record(EventBody::SessionStarted {
            id: prepared.id.clone(),
            parent: None,
            depth: 0,
            cwd: crate::util::path_string(&prepared.cwd),
            os: std::env::consts::OS.to_string(),
            kelir_version: VERSION.to_string(),
            provider: prepared.provider_name.clone(),
            model: prepared.model.clone(),
            system: SystemInfo {
                text: prepared.system.text.clone().into(),
                hash: prepared.system.hash.clone(),
            },
            sources: prepared.system.sources.clone(),
            tools: session.tool_names.clone(),
            // What the loop will ask this model for: the configured effort,
            // unless the provider's list says the model does not take the
            // parameter (STEER-6 review S3). `None` means no field is sent at
            // all — config `none`, or a model the list rules out — and the log
            // then agrees with the request instead of naming a field the
            // provider would reject. A `model` verb later swaps the client and
            // the request follows the new provider.
            reasoning_effort: session.first_effort(),
            // Where the key came from, never the key (STEER-2).
            key_origin: prepared.key_origin.clone(),
        })?;
        // What the render had to say about its sources (a non-UTF-8 file read
        // lossily, a truncation, a malformed skill skipped): recorded once, so
        // a file never vanishes without a trace (F5, F6, F7).
        for warning in &prepared.system.warnings {
            session.record(EventBody::Error {
                location: "source".into(),
                message: warning.clone(),
                retryable: false,
                status: None,
                attempt: None,
                retry_in_ms: None,
            })?;
        }
        if session.has_task {
            session.record(EventBody::User {
                text: prepared.task.clone().into(),
                from: "cli".into(),
                delivery: "now".into(),
                attachments: Vec::new(),
            })?;
        }
        session.sync();
        Ok(session)
    }

    /// Pick the log up where it stopped: report the damage, mark the resume,
    /// settle the calls nobody settled (as *unknown*, never by running them
    /// again), and add a new task if one was given.
    fn reopen(&mut self, read: Reopened) -> Result<(), String> {
        // Gaps in the log first: a torn last line and every line that never
        // parsed are reported, never dropped silently — but a gap that a
        // previous resume already reported is not reported again (M3 review
        // S3: three resumes, one `error`).
        let reported: Vec<String> = self
            .events
            .iter()
            .filter_map(|event| match &event.body {
                EventBody::Error {
                    location, message, ..
                } if location == "log" => Some(message.clone()),
                _ => None,
            })
            .collect();
        for damage in &read.damage {
            if reported.contains(damage) {
                continue;
            }
            self.record(EventBody::Error {
                location: "log".into(),
                message: damage.clone(),
                retryable: false,
                status: None,
                attempt: None,
                retry_in_ms: None,
            })?;
        }
        let from_seq = resume::last_seq(&read.events);
        self.record(EventBody::Resumed {
            from_seq,
            unsettled: read
                .unsettled
                .iter()
                .map(|call| call.call_id.clone())
                .collect(),
        })?;
        self.sync();

        // A call the log shows as requested or started, with no result, gets
        // exactly one synthetic result. Re-running it is what a naive replay
        // does; it is how `rm -rf` runs twice.
        let mut unknown = 0usize;
        for call in &read.unsettled {
            let partial = self.interrupted_output(call);
            let result = resume::unknown_result(
                partial
                    .as_ref()
                    .map(|(path, bytes, lines)| (path.as_str(), *bytes, *lines)),
            );
            let (out_path, bytes, lines) = partial.unwrap_or_default();
            self.record(EventBody::ToolSettled {
                call_id: call.call_id.clone(),
                outcome: "unknown".into(),
                exit_code: None,
                bytes,
                lines,
                out_path,
                spilled: false,
                result: Payload::text(result),
            })?;
            unknown += 1;
        }
        if unknown > 0 {
            self.emit(UiEvent::Info(format!(
                "kelir: resumed · {unknown} call(s) had an unknown outcome and were not re-run"
            )));
        }
        // An agent-called compaction that was settled but never applied — a
        // kill between the tool's `compaction queued` result and the boundary
        // that summarises — is queued again here (M3 review N2), so the model's
        // request survives the restart.
        if self.compact_request.is_none()
            && let Some(focus) = compact::queued_compaction(&self.events, &self.dir)
        {
            self.compact_request = Some(CompactRequest { by: "agent", focus });
            self.emit(UiEvent::Info(
                "kelir: resumed · a queued compaction is applied at the next step".to_string(),
            ));
        }
        // Background jobs that started or settled while no session was running:
        // the shim's status file is the record, and this is where it reaches the
        // log (`bg_started`, `bg_settled` and the message that wakes the model).
        //
        // The registry is rebuilt first, **from the log and only from the
        // log** (STEER-7 review B1): the ids are this session's own — a
        // `tool_started` carries the call's `commands/<dir>` name, a
        // `bg_started` a job's id — and a file in `bg/` proves nothing,
        // because a tool child can write one. A note is recorded only for an
        // id already adopted here; a status file nobody minted is left alone
        // (it is still visible to `kelir bg ls`, which reads the files).
        for event in &self.events {
            match &event.body {
                EventBody::BgStarted { bg_id, pubkey, .. } => {
                    self.trust.adopt(bg_id, pubkey.as_deref())
                }
                EventBody::ToolStarted { dir: Some(dir), .. } => self.trust.adopt(dir, None),
                _ => {}
            }
        }
        let mut woke = false;
        for status in crate::bg::list(&self.dir.join("bg")) {
            if !status.is_job() {
                continue;
            }
            woke |= self.note_bg(&status)?;
        }
        if woke {
            // A job settling while the session was down *is* something to do:
            // the resumed turn reads the message like any other.
            self.has_task = true;
            self.emit(UiEvent::Info(
                "kelir: resumed · a background job settled while this session was not running"
                    .to_string(),
            ));
        }
        if !self.pending_task.trim().is_empty() {
            // `kelir resume <id> <task>`: a new message on the existing log.
            // When a turn was in flight it lands after the unknown results the
            // model must read first, so it is `next` rather than `now`.
            let delivery = if read.in_flight { "next" } else { "now" };
            self.record(EventBody::User {
                text: self.pending_task.clone().into(),
                from: "cli".into(),
                delivery: delivery.to_string(),
                attachments: Vec::new(),
            })?;
        } else {
            self.continue_a_turn_that_ends_on_an_answer()?;
        }
        self.sync();
        Ok(())
    }

    /// S4: a kill can leave a log whose last message is the assistant's final
    /// answer, and a request may not end on one — providers read a trailing
    /// assistant message as prefill or refuse it. When the resume is about to
    /// run a turn the log did not leave ready (an unclosed log, or a turn that
    /// ended stopped or in error), it says what happened in a message of its
    /// own instead.
    fn continue_a_turn_that_ends_on_an_answer(&mut self) -> Result<(), String> {
        if !self.pending_task.trim().is_empty() {
            return Ok(());
        }
        let closed = self
            .events
            .iter()
            .any(|event| matches!(event.body, EventBody::SessionEnded { .. }));
        let continuing = !closed
            || matches!(
                resume::last_turn_outcome(&self.events).as_deref(),
                Some("stopped") | Some("paused") | Some("error")
            );
        if !continuing {
            return Ok(());
        }
        let folded = crate::fold::fold(&self.events, &self.dir)?;
        if folded.messages.last().map(|message| message.role.as_str()) != Some("assistant") {
            return Ok(());
        }
        self.record(EventBody::User {
            text: Payload::text(CONTINUE_NOTE),
            from: "cli".into(),
            delivery: "now".into(),
            attachments: Vec::new(),
        })?;
        self.has_task = true;
        Ok(())
    }

    /// The partial output of a call the previous process started, when its
    /// `commands/<seq>-*` directory still has one. The model gets the counts
    /// and the path; the result still says the outcome is unknown.
    fn interrupted_output(&self, call: &UnsettledCall) -> Option<(String, u64, u64)> {
        if !call.started {
            return None;
        }
        // The last start for this id, in case a provider reuses call ids.
        let seq = self
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.body {
                EventBody::ToolStarted { call_id, .. } if *call_id == call.call_id => {
                    Some(event.seq)
                }
                _ => None,
            })?;
        let prefix = format!("{seq}-");
        let entry = std::fs::read_dir(self.dir.join("commands"))
            .ok()?
            .flatten()
            .find(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))?;
        let out = entry.path().join("out");
        let partial = crate::spill::partial(&out).ok()?;
        if partial.bytes == 0 {
            return None;
        }
        Some((crate::util::path_string(&out), partial.bytes, partial.lines))
    }

    /// Append one event. Oversized payloads go to `blobs/` first, named by the
    /// seq the event is about to get.
    fn record(&mut self, body: EventBody) -> Result<LogEvent, String> {
        let seq = self.writer.next_seq();
        let body = spill_payloads(body, &self.blobs, seq)?;
        let event = self
            .writer
            .append(body)
            .map_err(|error| format!("cannot append to the event log: {error}"))?;
        if self.json {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let _ = writeln!(
                lock,
                "{}",
                serde_json::to_string(&event).unwrap_or_default()
            );
            let _ = lock.flush();
        }
        self.events.push(event.clone());
        Ok(event)
    }

    fn sync(&mut self) {
        let _ = self.writer.sync();
    }

    /// Tell the UI what happened. A closed UI is not an error: the session is
    /// the source of truth and keeps working headless.
    fn emit(&self, event: UiEvent) {
        if let Some(ui) = &self.ui {
            let _ = ui.send(event);
        }
    }

    /// Report Herdr status (no-op unless a writer was opened).
    ///
    /// In the TUI the record travels as a [`UiEvent`] and the TUI task writes
    /// it between paints: one thread owns the terminal, so an OSC can never
    /// split a crossterm sequence (M2 review F2). Non-TUI paths (`run`,
    /// `run --keep`) write it here, as before.
    fn report(&mut self, state: HerdrState) {
        let Some(herdr) = &self.herdr else {
            return;
        };
        let state = crate::herdr::state_for(state, herdr.telegram());
        if self.tui {
            self.emit(UiEvent::Herdr { state });
        } else if let Some(herdr) = &mut self.herdr {
            herdr.report(state);
        }
    }

    /// One turn is over: the UI gets its outcome, Herdr gets its word.
    ///
    /// Herdr 0.1.21 takes no `msg` on the record (it drops the whole record),
    /// so a failed turn's reason stays in the log and on the pane's screen.
    fn finish_turn(&mut self, outcome: &str) {
        self.emit(UiEvent::TurnEnded {
            outcome: outcome.to_string(),
            final_text: self.final_text.clone(),
        });
        let state = match outcome {
            "done" => HerdrState::Done,
            "error" => HerdrState::Error,
            // A paused turn waits for a human to say `continue`; that is the
            // one Herdr word a supervisor can see as waiting (review N2).
            "paused" => HerdrState::Blocked,
            _ => HerdrState::Idle,
        };
        self.report(state);
    }

    fn print_result(&self, exit_code: u8) {
        if !self.json {
            return;
        }
        let steps = self
            .events
            .iter()
            .filter(|event| matches!(event.body, EventBody::StepStarted { .. }))
            .count();
        // The last turn's outcome, so a stopped turn is reported as such.
        let outcome = self
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.body {
                EventBody::TurnEnded { outcome, .. } => Some(outcome.clone()),
                _ => None,
            })
            .unwrap_or_else(|| {
                if exit_code == 0 {
                    "done".to_string()
                } else {
                    "error".to_string()
                }
            });
        print_result_event(
            &outcome,
            exit_code,
            &self.final_text,
            steps,
            Some((&self.id, &self.dir)),
            None,
        );
    }

    /// The state a verb's answer reports back to its client.
    fn state(&self) -> Value {
        json!({
            "turn": self.turn,
            "step": self.step,
            // The turn's own count and its cap, so a steering human sees a
            // pause coming (FINDINGS: "a status line with steps used/limit
            // would have shown it coming"). `max_steps` is null with no cap.
            "turn_steps": self.turn_steps,
            "max_steps": self.cfg.max_steps,
            "running": self.live_calls,
            "provider": self.provider_name,
            "model": self.model,
            "window": self.window,
            "window_source": self.window_source,
            // Why this start does not have the provider's window, when it does
            // not (STEER-1 review N4): `http 401`, `offline`, `timeout`,
            // `malformed`, `fetching`. Absent when nothing needs saying.
            "window_note": self.window_note,
            // Whether the list is still being fetched beside the session.
            "models_fetching": self
                .models_fetch
                .as_ref()
                .map(Fetch::is_running)
                .unwrap_or(false),
            "disabled": self.disabled.iter().cloned().collect::<Vec<_>>(),
        })
    }

    // -------------------------------------------------------------- verbs

    /// Log the verb (applied or not), validate it and answer the client. The
    /// verb is *applied* later, at its boundary.
    fn accept(&mut self, incoming: Incoming) -> Option<Pending> {
        self.accept_as(incoming, None)
    }

    /// `accept`, with the answer's `applies` word overridden. The idle loop
    /// uses it for a `stop` that has nothing to stop: the verb is dropped, so
    /// the answer says `dropped`, not `now` (M2 review N1).
    fn accept_as(&mut self, incoming: Incoming, applies: Option<&'static str>) -> Option<Pending> {
        let Incoming {
            request,
            reply,
            action,
        } = incoming;
        if let Some(pending) = self.note_own(action) {
            // The session's own watcher, not a client: nothing to log and
            // nobody to answer, but the note still waits for a boundary like
            // the `bg` report it stands in for, so `bg_started`/`bg_settled`
            // land in the same order whichever of the two arrives first.
            self.scheduler.push(pending);
            return None;
        }
        let from = request.from.clone();
        let name = request.verb.clone();
        // `bg-spawn` is the one thing a job cannot do itself: on Windows a
        // `kelir bg run` inside a call sits in the call's Job Object, which
        // forbids breakaway (it has to — an escapable job cannot be
        // cancelled), so the session, which is outside it, starts the shim.
        // There is nothing to queue: the job's own status file is the record.
        if name == "bg-spawn" {
            let answer = self.bg_spawn_answer(&from, &request);
            let _ = reply.send(answer);
            return None;
        }
        if name == "bg-claim" {
            let answer = self.bg_spawn_answer(&from, &request);
            let _ = reply.send(answer);
            return None;
        }
        let parsed = self.parse_verb(&request, &from);
        let scope = match &parsed {
            Ok(verb) => applies.unwrap_or_else(|| verb.scope()),
            Err(_) => "rejected",
        };
        let error = parsed.as_ref().err().cloned();
        // A refusal is recorded only while the refusal budget lasts: a token
        // holder that names foreign jobs one by one cannot grow the log without
        // bound (STEER-7 review B1). The answer still goes out.
        let recorded = if error.is_some() && !self.trust.may_record_refusal() {
            Err("the refusal budget is spent".to_string())
        } else {
            self.log_verb(&name, &request.args, &from, scope, error)
        };
        // The ack must not outrun the log: a crash right after `{"ok": true}`
        // may not lose a verb the client believes was accepted (M1 review N1).
        self.sync();
        match (parsed, recorded) {
            (Ok(verb), Ok(event)) => {
                let pending = Pending {
                    seq: event.seq,
                    verb: verb.clone(),
                    from,
                };
                let _ = reply.send(Response::ok(&name, event.seq, scope, self.state()));
                Some(pending)
            }
            (Ok(_), Err(message)) => {
                let _ = reply.send(Response::error(message));
                None
            }
            (Err(message), _) => {
                let _ = reply.send(Response::error(message));
                None
            }
        }
    }

    /// `accept` for a verb a wait is about to apply at once (`say --now`,
    /// `model`): the answer goes out *after* the verb landed, so it says
    /// `applies: "now"` and `state.model` (or the queued text) is what the
    /// next request really carries (STEER-5 review N1).
    async fn accept_at_once(&mut self, incoming: Incoming) -> AtOnce {
        let Incoming {
            request,
            reply,
            action,
        } = incoming;
        if let Some(pending) = self.note_own(action) {
            self.scheduler.push(pending);
            return AtOnce::Rejected;
        }
        let from = request.from.clone();
        let name = request.verb.clone();
        if name == "bg-spawn" {
            let answer = self.bg_spawn_answer(&from, &request);
            let _ = reply.send(answer);
            return AtOnce::Rejected;
        }
        if name == "bg-claim" {
            let answer = self.bg_spawn_answer(&from, &request);
            let _ = reply.send(answer);
            return AtOnce::Rejected;
        }
        let parsed = self.parse_verb(&request, &from);
        let boundary = if parsed.is_ok() { "now" } else { "rejected" };
        let error = parsed.as_ref().err().cloned();
        // The refusal budget (STEER-7 review B1) applies here too: a token
        // holder naming foreign jobs one by one gets its answer either way, and
        // the log grows by no more than the budget.
        let recorded = if error.is_some() && !self.trust.may_record_refusal() {
            Err("the refusal budget is spent".to_string())
        } else {
            self.log_verb(&name, &request.args, &from, boundary, error)
        };
        self.sync();
        let (verb, event) = match (parsed, recorded) {
            (Ok(verb), Ok(event)) => (verb, event),
            (Ok(_), Err(message)) | (Err(message), _) => {
                let _ = reply.send(Response::error(message));
                // A refusal changes nothing, so the wait it arrived in goes on.
                return AtOnce::Rejected;
            }
        };
        let pending = Pending {
            seq: event.seq,
            verb,
            from,
        };
        let mut end: Option<StreamEnd> = None;
        let applied = self.apply_one(pending, &mut end).await;
        let answer = match applied {
            Ok(()) => Response::ok(&name, event.seq, "now", self.state()),
            Err(message) => Response::error(message),
        };
        // The answer goes out either way: the verb has landed (or the log said
        // why it could not), and a client must not wait on a reply nobody kept.
        let _ = reply.send(answer);
        AtOnce::Applied(end.map(Box::new))
    }

    /// The `bg-spawn` answer: the shim only ever answers a job, and a job is
    /// the only sender allowed to ask. A refusal — a foreign job directory, a
    /// path that escapes this session's own `bg/` — is recorded like any other
    /// refused verb, so the log says what was tried (STEER-7).
    ///
    /// `bg-claim` is the sibling verb a `kelir bg run` sends *before* it starts
    /// its own shim: the session mints the id and the capability and starts
    /// nothing. The id is the session's, never the caller's (review B1), so a
    /// child cannot pick a name it later reports for.
    fn bg_spawn_answer(&mut self, from: &str, request: &Request) -> Response {
        if from != "job" {
            return Response::error("bg-spawn: an internal verb; jobs report themselves");
        }
        if request.verb == "bg-claim" {
            return self.bg_claim_answer();
        }
        if let Err(error) = self.trust.check_spawn(request) {
            return match self.refuse_verb("bg-spawn", &request.args, from, &error) {
                Ok(()) => Response::error(error),
                Err(message) => Response::error(message),
            };
        }
        // Only `cmd`, `shell`, `cwd` and a filtered `env` are the caller's; the
        // kind, the socket, the token, the job name and the call are the
        // session's own (STEER-7 review round 4, S1).
        let call_is_live = |call: &str| self.live_calls.iter().any(|live| live == call);
        match crate::trust::spawn_request(request).and_then(|spawn| {
            let spawn = spawn.pinned(&self.ctx.sock, &self.ctx.token, call_is_live);
            crate::bg::spawn_shim_for_client(&self.trust, &spawn)
        }) {
            Ok(spawned) => Response::ok("bg-spawn", 0, "now", json!({"id": spawned.id})),
            Err(error) => Response::error(error),
        }
    }

    /// `bg-claim`, retired: nothing sends it, so it answers an id and nothing
    /// else, from a small budget of its own (it must not spend the one
    /// `bg-spawn` mints from).
    fn bg_claim_answer(&mut self) -> Response {
        match self.trust.claim_ack() {
            Ok(id) => Response::ok("bg-claim", 0, "now", json!({"id": id})),
            Err(error) => Response::error(error),
        }
    }

    /// Record a refused verb, unless the refusal budget is spent: a token
    /// holder can name foreign jobs one by one and the log must not grow by one
    /// event per try (STEER-7 review B1).
    fn refuse_verb(
        &mut self,
        name: &str,
        args: &Value,
        from: &str,
        error: &str,
    ) -> Result<(), String> {
        if !self.trust.may_record_refusal() {
            return Ok(());
        }
        let event = self.log_verb(name, args, from, "rejected", Some(error.to_string()))?;
        self.sync();
        let _ = event;
        Ok(())
    }

    /// Validate one arrival. `bg` is internal, and `stop --call` for a call that
    /// is not running (`live_calls` holds only the unsettled calls of the
    /// batch) is a refusal rather than a silent no-op.
    fn parse_verb(&self, request: &Request, from: &str) -> Result<Verb, String> {
        let name = request.verb.as_str();
        if name == "bg" && from != "job" {
            return Err("bg: an internal verb; jobs report themselves".to_string());
        }
        // Only for the session's own jobs: a foreign id is refused here, where
        // it is recorded as a `verb` event marked refused (STEER-7).
        self.trust.check_job(request)?;
        let parsed = verbs::parse(name, &request.args)?;
        // `view` is validated when it arrives, not when it applies: the CLI
        // prints "image queued" from the ack, so a bad path must be refused in
        // the ack and not a boundary later (where nobody is listening).
        if let Verb::View { path } = &parsed
            && let Err(message) = self.check_view(path)
        {
            return Err(message);
        }
        if let Verb::Stop {
            call: Some(call), ..
        } = &parsed
            && !self.live_calls.iter().any(|id| id == call)
        {
            return Err(format!("no running call with id `{call}`"));
        }
        Ok(parsed)
    }

    /// Log the arrival, before it is answered or applied.
    fn log_verb(
        &mut self,
        name: &str,
        args: &Value,
        from: &str,
        boundary: &str,
        error: Option<String>,
    ) -> Result<crate::event::LogEvent, String> {
        self.record(EventBody::Verb {
            name: name.to_string(),
            // Never the request's args wholesale: a whitelist per verb
            // (STEER-7 review round 4, B1).
            args: Payload::value(verbs::loggable_args(name, args)),
            from: from.to_string(),
            boundary: boundary.to_string(),
            error,
        })
    }

    /// Apply the verbs due at `boundary`.
    async fn apply(&mut self, boundary: Boundary) -> Result<Applied, String> {
        let mut applied = Applied::default();
        for pending in self.scheduler.due(boundary) {
            self.apply_pending(pending, &mut applied).await?;
        }
        self.sync();
        Ok(applied)
    }

    /// Apply the pending verbs that are *actions* rather than messages. A
    /// `cancel` kills a job and records it; it never starts a turn, and while
    /// the session is idle no boundary arrives to apply it — so it is applied
    /// here, at once, instead of sitting in the queue for ever (STEER-7).
    async fn apply_actions(&mut self) -> Result<(), String> {
        let actions = self.scheduler.take_where(Verb::is_action);
        if actions.is_empty() {
            return Ok(());
        }
        let mut applied = Applied::default();
        for pending in actions {
            self.apply_pending(pending, &mut applied).await?;
        }
        self.sync();
        Ok(())
    }

    /// Apply one verb. The caller decides when: a boundary, or a wait the verb
    /// is allowed to cut short.
    async fn apply_pending(
        &mut self,
        pending: Pending,
        applied: &mut Applied,
    ) -> Result<(), String> {
        {
            match pending.verb {
                Verb::Say { text, now } => {
                    // A message from anywhere but this UI gets echoed, so a
                    // steering `ctl say` is visible in the pane it steers.
                    if pending.from != "tui" {
                        self.emit(UiEvent::Info(format!("› {}", one_line(&text))));
                    }
                    // The text carries the pointers; the list carries the paths
                    // they name, so a reader (and a later tool) can see which
                    // images a message was about without parsing prose (N5).
                    let attachments = crate::attachments::paths_in(&text)
                        .into_iter()
                        .map(serde_json::Value::String)
                        .collect();
                    self.record(EventBody::User {
                        text: Payload::text(text),
                        from: pending.from,
                        delivery: if now { "now" } else { "next" }.to_string(),
                        attachments,
                    })?;
                    applied.messages += 1;
                }
                // Due only at a turn end, where `take_turn_messages` gets it.
                Verb::Later { .. } => {}
                // The call already settled; there is nothing left to kill.
                Verb::Stop { call: Some(_), .. } => {}
                Verb::Stop { call: None, reason } => {
                    applied.stopped = Some(reason.unwrap_or_else(|| "stopped by user".into()));
                }
                Verb::Model { target } => {
                    // A bad model must not kill the session: it is recorded and
                    // the old one keeps working.
                    if let Err(message) = self.swap_model(&target) {
                        self.record(EventBody::Error {
                            location: "verb".into(),
                            message,
                            retryable: false,
                            status: None,
                            attempt: None,
                            retry_in_ms: None,
                        })?;
                    }
                }
                Verb::Tools { disable, enable } => {
                    for name in disable {
                        self.disabled.insert(name);
                    }
                    for name in enable {
                        self.disabled.remove(&name);
                    }
                }
                Verb::View { path } => {
                    // An image queued by `kelir view` from inside a call: it
                    // becomes a `source` event at the tail, so the next request
                    // carries an image part and every earlier message (and the
                    // cached prefix) stays byte-identical.
                    match self.apply_view(&path) {
                        Ok((rel, bytes)) => {
                            applied.messages += 1;
                            self.emit(UiEvent::Attachment { rel, bytes });
                            self.sync();
                        }
                        Err(message) => {
                            self.emit(UiEvent::Error(message.clone()));
                            self.record(EventBody::Error {
                                location: "view".into(),
                                message,
                                retryable: false,
                                status: None,
                                attempt: None,
                                retry_in_ms: None,
                            })?;
                            self.sync();
                        }
                    }
                }
                Verb::End => applied.end = true,
                Verb::Compact { focus } => {
                    // Applies at `before_step`: the compaction needs a provider
                    // call and must never land between a tool call and its
                    // result.
                    self.emit(UiEvent::Info(match &focus {
                        Some(focus) => format!("» compact: {focus}"),
                        None => "» compact".to_string(),
                    }));
                    if self.compact_request.is_none() {
                        self.compact_request = Some(CompactRequest {
                            by: "user",
                            focus: focus.clone(),
                        });
                    }
                }
                Verb::Bg(status) => {
                    if self.note_bg(&status)? {
                        applied.messages += 1;
                    }
                }
                // A job of this session's own, killed on request (STEER-7).
                // The refusal (a foreign id) already answered the client; here
                // the kill happens and, if it worked, the job's own settled
                // report reaches the log through the next `bg` arrival.
                Verb::Cancel { id } => {
                    match crate::bg::cancel_in(
                        &self.trust.bg_dir(),
                        &id,
                        self.trust.key(&id).as_deref(),
                    )
                    .await
                    {
                        Ok(text) => self.emit(UiEvent::Info(format!("· {text}"))),
                        Err(text) => {
                            self.record(EventBody::Error {
                                location: "verb".into(),
                                message: format!("cancel {id}: {text}"),
                                retryable: false,
                                status: None,
                                attempt: None,
                                retry_in_ms: None,
                            })?;
                        }
                    }
                }
                // Adds or replaces one source, as a message at the tail: the
                // system prompt (and the provider's cache) is untouched.
                Verb::Skills {
                    kind,
                    name,
                    path,
                    text,
                } => {
                    match self.apply_skills(kind, &name, path.as_deref(), text.as_deref()) {
                        Ok(()) => applied.messages += 1,
                        // A bad source must not kill the session: it is
                        // recorded and the turn continues.
                        Err(message) => {
                            self.record(EventBody::Error {
                                location: "verb".into(),
                                message,
                                retryable: false,
                                status: None,
                                attempt: None,
                                retry_in_ms: None,
                            })?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// `skills`: read one skill or instruction source (from a file, a skill
    /// directory, or the verb's own text) and add it to the context as a
    /// `source` message.
    ///
    /// The body is the file's whole text, the same bytes the watcher hashes, so
    /// a skill added here is not announced again at the next boundary (F3, N5)
    /// — for a skill that also means its `description`, which lives in the
    /// front matter, arrives too. Repeating the text the last message for that
    /// name already carried is refused; going *back* to an earlier body is not,
    /// because the model's latest word on the source is the one in between
    /// (F1b).
    fn apply_skills(
        &mut self,
        kind: crate::sources::SourceKind,
        name: &str,
        path: Option<&str>,
        text: Option<&str>,
    ) -> Result<(), String> {
        let (body, resolved, notes) = match (text, path) {
            (Some(text), _) => (text.to_string(), None, Vec::new()),
            (None, Some(path)) => {
                let path = std::path::PathBuf::from(path);
                let file = if path.is_file() {
                    path.clone()
                } else {
                    path.join("SKILL.md")
                };
                let (body, notes) =
                    crate::sources::read_source(&file, crate::sources::limit_for(kind))
                        .ok_or_else(|| {
                            format!("skills: cannot read {}", crate::util::path_string(&file))
                        })?;
                // A skill's front matter must parse: a broken `SKILL.md` is
                // refused here rather than handed to the model as if it were
                // one.
                if kind == crate::sources::SourceKind::Skill {
                    let skill = crate::sources::parse_skill(&body)?;
                    if skill.name != name {
                        return Err(format!(
                            "skills: {} names itself `{}`, not `{name}`",
                            crate::util::path_string(&file),
                            skill.name
                        ));
                    }
                }
                (body, Some(file), notes)
            }
            (None, None) => return Err("skills: nothing to add".into()),
        };
        let hash = crate::sources::hash_of(&body);
        if self.latest_source_is(kind, name, &hash) {
            return Err(format!(
                "skills: `{name}` already carries exactly this text"
            ));
        }
        for note in notes {
            self.record(EventBody::Error {
                location: "source".into(),
                message: note,
                retryable: false,
                status: None,
                attempt: None,
                retry_in_ms: None,
            })?;
        }
        let logged = match &resolved {
            Some(path) => crate::sources::SourcePath::File(path.clone()),
            None => crate::sources::SourcePath::Embedded,
        };
        self.record_source(kind, name, &body, &logged)?;
        self.emit(UiEvent::Info(format!("» skills: {name}")));
        self.sync();
        Ok(())
    }

    /// Does the *last* thing this context says about a source carry `hash`?
    ///
    /// Only the last one counts: an earlier message with the same text is not
    /// the current word on the file (F1b).
    fn latest_source_is(&self, kind: crate::sources::SourceKind, name: &str, hash: &str) -> bool {
        let last = self
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.body {
                EventBody::Source {
                    kind: event_kind,
                    name: event_name,
                    text,
                    ..
                } if event_name == name
                    && crate::sources::SourceKind::parse(event_kind) == kind =>
                {
                    // A big body is spilled to a blob: read it back, or a
                    // repeat of the current text would slip through.
                    Some(
                        text.as_ref()
                            .and_then(|text| text.resolve_text(&self.dir).ok())
                            .map(|text| crate::sources::hash_of(&text)),
                    )
                }
                _ => None,
            });
        matches!(last, Some(Some(seen)) if seen == hash)
    }

    /// `model` swaps the provider/model for the rest of the session. It applies
    /// at the next step, so the cache restart is expected: another model has
    /// another cache. The verb event records the swap; the next `step_started`
    /// records it in the log.
    fn swap_model(&mut self, target: &str) -> Result<(), String> {
        let (provider, model) = split_model_target(target, &self.cfg.providers);
        let provider = provider
            .map(str::to_string)
            .unwrap_or_else(|| self.provider_name.clone());
        let model = model.to_string();
        let provider_cfg = if provider == self.provider_name {
            // The same provider: the key already in memory is the live one, and
            // a re-read here could only cost a process start.
            self.active_provider.clone()
        } else {
            // A swap to another provider needs *that* provider's key, resolved
            // the same way the start resolved this one (STEER-2). It happens
            // once, on a verb, so the bounded read is fine here.
            let mut provider_cfg = self
                .cfg
                .provider(&provider)
                .map_err(|error| error.to_string())?;
            credentials::resolve(&mut provider_cfg)?;
            provider_cfg
        };
        // The governor belongs to the provider a request goes to: a swap to a
        // governed provider must take its slots (and a swap away must stop
        // being held to the old limit). `max_concurrent_requests` is read from
        // the provider config the swap resolved to, never from the one the
        // session started on (STEER-5 review S1).
        let governor = governor_for(&provider_cfg);
        let client = Client::new(provider_cfg.clone(), &self.id, self.cfg.cache_ttl.clone())
            .map_err(|error| error.message)?;
        self.provider_name = provider;
        self.model = model;
        self.client = client;
        self.active_provider = provider_cfg;
        self.governor = governor;
        // Another model, possibly another provider: resolve from that
        // endpoint's cache (never the network mid-turn) and start a fetch
        // beside the session when it is stale, so the new provider's list
        // arrives at the next `before_step` instead of waiting for another
        // process to write it. A model no list knows falls back to the default
        // window and `state` says why (STEER-1 review S2/S4).
        let base_url = self.client.base_url();
        let endpoint = crate::models::endpoint_id(&self.provider_name, &base_url);
        self.models = crate::models::load(&endpoint);
        self.models_fetch = Some(start_fetch(&self.client, &self.provider_name, &endpoint));
        let over = self.cfg.override_window(&self.model);
        let (window, source) =
            crate::models::resolve(over, self.models.as_ref(), false, &self.model);
        // A swap is the one moment a person must be told the window changed:
        // it is not the startup answer any more, and it sets the compaction
        // threshold for the rest of the session. The banner no longer carries
        // the number, so this is where it is said (F-window). A later answer
        // for the same model that changes it again is announced by
        // [`Session::apply_list`], which knows when the swap's own cache was
        // already right.
        if window != self.window || source != self.window_source {
            self.emit(UiEvent::Info(format!(
                "» window {window} tokens ({source})"
            )));
        }
        self.window = window;
        self.window_source = source;
        let why = self.models.as_ref().and_then(|cache| cache.error.clone());
        self.window_note = window_note(over, why.as_deref(), source);
        Ok(())
    }

    /// Take the metadata fetch's answer if it has landed, and apply it: the
    /// list a `model` swap or a later step will read, and the window if the
    /// configuration does not override it.
    ///
    /// Called at `before_step`, which is the first place the window is read
    /// (the auto-compaction trigger), so a slow `/models` never holds the first
    /// request (STEER-1 review S2). Never waits: a fetch still running is
    /// picked up at a later boundary.
    async fn apply_models(&mut self) {
        let ready = match self.models_fetch.as_mut() {
            Some(fetch) => fetch.take_ready(),
            None => None,
        };
        if let Some(list) = ready {
            self.apply_list(list);
        }
    }

    /// Apply one answer from the metadata fetch: the list every later step and
    /// a `model` swap read, and the window when the configuration does not
    /// override it.
    fn apply_list(&mut self, list: ModelList) {
        if let Some(cache) = list.cache {
            self.models = Some(cache);
        }
        let over = self.cfg.override_window(&self.model);
        let (window, source) =
            crate::models::resolve(over, self.models.as_ref(), list.fetched, &self.model);
        // The startup answer is silent: the banner no longer carries the window
        // (tui3). Two cases are not silent, and both are one dim line:
        //
        // * the start fell back (a dead endpoint, a 401): the banner's note is
        //   written before this answer exists, so a fallback that the start
        //   could not see is said here instead — once, when the source changes
        //   to the fallback or its reason does (F-window);
        // * a `/model` swap moved the number: the new model's window is what
        //   compaction now measures against (STEER-1 review N4).
        let note = window_note(over, list.why.as_deref(), source);
        let previous = self.window_model.replace(self.model.clone());
        let swapped = previous.is_some_and(|model| model != self.model);
        let moved = swapped && (window != self.window || source != self.window_source);
        // A start whose fetch had not landed says the fallback here instead:
        // the banner could only say "fetching" (the answer did not exist yet),
        // and the reason is what a person needs (F-window). Once: the same
        // answer twice says nothing new.
        let fell_back = source == "default"
            && note.is_some()
            && (note != self.window_note || source != self.window_source);
        if moved || fell_back {
            let mut line = format!("» window {window} tokens ({source})");
            if let Some(note) = &note
                && fell_back
            {
                line.push_str(" · ");
                line.push_str(note);
            }
            self.emit(UiEvent::Info(line));
        }
        self.window = window;
        self.window_source = source;
        self.window_note = note;
    }

    /// Messages that should start the next turn: `later` deliveries, and any
    /// `say` that was still waiting when the turn ended.
    fn take_turn_messages(&mut self) -> Option<TurnStart> {
        let mut messages = Vec::new();
        let mut end = false;
        let mut woke = false;
        for pending in self
            .scheduler
            .take_where(|verb| verb.applies_at(Boundary::TurnEnd) && !verb.is_action())
        {
            match pending.verb {
                Verb::Say { text, now } => messages.push(Message {
                    text,
                    delivery: if now { "now" } else { "next" },
                    from: pending.from,
                }),
                Verb::Later { text } => messages.push(Message {
                    text,
                    delivery: "later",
                    from: pending.from,
                }),
                // A job report that arrived while the turn was ending: the
                // wake message is written here, and the next turn reads it.
                Verb::Bg(status) => woke |= self.note_bg(&status).unwrap_or(false),
                Verb::End => end = true,
                Verb::Stop { .. }
                | Verb::Cancel { .. }
                | Verb::Model { .. }
                | Verb::Tools { .. }
                | Verb::Compact { .. }
                | Verb::Skills { .. }
                | Verb::View { .. } => {}
            }
        }
        if end {
            Some(TurnStart::End)
        } else if !messages.is_empty() {
            Some(TurnStart::Messages(messages))
        } else if woke {
            // The message is already in the log; the turn reads it from the
            // fold like any other.
            Some(TurnStart::Messages(Vec::new()))
        } else {
            None
        }
    }

    /// Record what a job's status file says: `bg_started` once, and — when it
    /// settled — `bg_settled` plus the `user` message from `job` that wakes the
    /// agent. Returns whether a wake message was written.
    ///
    /// The log is the record and the dedupe: a report that arrives twice (the
    /// socket and the watcher, or a watcher restarted by a resume) is recorded
    /// once.
    fn note_bg(&mut self, reported: &crate::bg::Status) -> Result<bool, String> {
        use crate::trust::Seal;
        // The id is this session's own or nothing: a status file nobody minted
        // never reaches the log as a job (STEER-7 review B1).
        let id = reported.id.clone();
        if !self.trust.owns(&id) {
            return Ok(false);
        }
        // The file is the truth as of now, the argument is a snapshot from when
        // the report was made. Re-reading matters: a job can die between the
        // two (a `cancel` in flight kills the shim, then writes `cancelled`),
        // and recording the snapshot after calling its dead shim `lost` put
        // the wrong outcome in the log (STEER-7).
        //
        // And a file is not evidence by itself (steer7b review B1'): a tool
        // child can write `bg/<id>.status` as easily as the shim. Only a record
        // signed with the key the session gave that job's shim is believed;
        // the snapshot is held to the same rule.
        let dir = self.trust.bg_dir();
        let file = crate::bg::Status::read(&crate::bg::status_path(&dir, &id));
        // A final record is terminal: the shim also keeps it beside the status
        // file, and a validly signed final record wins over whatever the status
        // file now says, so putting an older record back cannot un-settle the
        // job (a replay can withhold a result, never change it).
        let last = crate::bg::Status::read(&crate::bg::final_path(&dir, &id)).filter(|last| {
            last.id == id && last.is_job() && last.settled() && self.trust.seal(last) == Seal::Good
        });
        let candidate = last.as_ref().or(file.as_ref()).unwrap_or(reported);
        let sealed = if candidate.id == id {
            self.trust.seal(candidate)
        } else {
            Seal::Forged
        };
        // Sequence numbers only go up: a record older than one already
        // believed is a replay.
        if sealed == Seal::Good {
            let seen = self.seen_seq.entry(id.clone()).or_insert(0);
            if candidate.seq < *seen {
                return Ok(false);
            }
            *seen = candidate.seq;
        }
        // A signed record still cannot point the session at a file outside its
        // own `bg/`: its `out` is quoted into the wake message.
        if sealed == Seal::Good
            && !candidate.out.trim().is_empty()
            && !self.trust.inside_bg(&candidate.out)
        {
            return Ok(false);
        }
        let status = match sealed {
            Seal::Good => crate::bg::observed_in(&dir, candidate),
            Seal::Unowned => return Ok(false),
            // A resumed session adopted this id from its log and no longer holds
            // the key (it died with the process that minted it), so a record
            // cannot be authenticated. What the log already says is the only
            // thing believed: the job is recorded as lost, with the output path
            // the log holds, never the file's.
            Seal::NoKey => match self.adopted_outcome(&id, candidate) {
                Some(lost) => lost,
                None => return Ok(false),
            },
            Seal::Unsigned | Seal::Forged => {
                if self.refused_status.insert(id.clone()) {
                    let why = if sealed == Seal::Unsigned {
                        "its status file carries no signature"
                    } else {
                        "its status file's signature is not the job's"
                    };
                    let message = format!(
                        "bg: the status file of `{id}` was ignored: {why}, so it was not written by that job's shim"
                    );
                    self.refuse_verb("bg", &json!({"id": id}), "job", &message)?;
                }
                return Ok(false);
            }
        };

        let status = &status;
        let started = self.events.iter().any(|event| {
            matches!(&event.body, EventBody::BgStarted { bg_id, .. } if bg_id == &status.id)
        });
        if !started {
            // The public key goes into the log with the job: a resume verifies
            // a surviving shim's records under it.
            let mut body = crate::bg::started_body(status);
            if let EventBody::BgStarted { pubkey, .. } = &mut body {
                *pubkey = self.trust.public(&status.id);
            }
            self.record(body)?;
        }
        if !status.settled() {
            self.sync();
            return Ok(false);
        }
        let settled = self.events.iter().any(|event| {
            matches!(&event.body, EventBody::BgSettled { bg_id, .. } if bg_id == &status.id)
        });
        if settled {
            return Ok(false);
        }
        self.record(crate::bg::settled_body(status))?;
        let text = crate::bg::wake_text(status);
        self.record(EventBody::User {
            text: Payload::text(text.clone()),
            from: "job".into(),
            delivery: "next".into(),
            attachments: Vec::new(),
        })?;
        self.emit(UiEvent::Info(format!("» {}", one_line(&text))));
        self.sync();
        Ok(true)
    }

    /// What a resumed session may say about a job whose status it cannot
    /// authenticate: nothing while the file says it runs, and `lost` (built
    /// from the log's own `bg_started`) once the file says it stopped. A job the
    /// log never recorded is left alone.
    fn adopted_outcome(&self, id: &str, file: &crate::bg::Status) -> Option<crate::bg::Status> {
        if !file.settled() {
            return None;
        }
        let (cmd, out, call) = self.events.iter().find_map(|event| match &event.body {
            EventBody::BgStarted {
                bg_id,
                cmd,
                out_path,
                call_id,
                ..
            } if bg_id == id => Some((
                cmd.inline_str().unwrap_or("").to_string(),
                out_path.clone(),
                call_id.clone(),
            )),
            _ => None,
        })?;
        let mut lost =
            crate::bg::Status::new(id, crate::bg::KIND_JOB, &cmd, std::path::Path::new(&out));
        lost.state = crate::bg::STATE_LOST.into();
        lost.finished = Some(crate::event::now_ts());
        lost.call = call;
        lost.error = Some(
            "the session was resumed after this job started, so its result could not be authenticated; read its output"
                .into(),
        );
        Some(lost)
    }

    /// A wake-up the session raised itself (its own `bg/` watcher) is not a
    /// request: there is no `Request` to answer and nobody is waiting. It is
    /// turned into a `Pending` here and queued through the scheduler like any
    /// other verb, so `bg_started`/`bg_settled` keep the order the old
    /// `bg`-verb path gave them (STEER-7: the watcher used to send a `bg`
    /// request pretending to be `job`, which is exactly the shape a foreign
    /// caller had).
    fn note_own(&self, action: Option<crate::ipc::Action>) -> Option<Pending> {
        action.map(|crate::ipc::Action::Note(status)| Pending {
            // The watcher wrote no `verb` event, so there is no seq of its
            // own; the scheduler keeps arrival order and `note_bg` writes
            // the `bg_*` events in the order the notes apply.
            seq: 0,
            verb: Verb::Bg(Box::new(status)),
            from: "job".into(),
        })
    }

    /// One image queued by `kelir view`: resolve the path, record the `source`
    /// event (kind `attachment`), and return what the TUI shows.
    ///
    /// The event carries the relative path (`attachments/1-x.png`) and it is
    /// the fold that turns it into a data URL at request time, so the log stays
    /// small and free of base64.
    fn apply_view(&mut self, named: &str) -> Result<(String, u64), String> {
        let path = self.check_view(named)?;
        let bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        // The image is copied into the session's attachments dir, so the
        // request does not depend on a path outside the session (and a later
        // resume still finds it).
        let rel = self.import_attachment(&path)?;
        self.record(EventBody::Source {
            kind: "attachment".to_string(),
            name: rel.clone(),
            text: None,
            path: Some(rel.clone()),
        })?;
        self.sync();
        Ok((rel, bytes))
    }

    /// Resolve and validate a `view` path without side effects: the same check
    /// the ack runs, so the CLI's answer is the truth.
    fn check_view(&self, named: &str) -> Result<std::path::PathBuf, String> {
        let path = crate::attachments::resolve(&self.dir, &self.ctx.cwd, named);
        if !path.is_file() {
            return Err(format!(
                "view: no such image: {}",
                crate::util::path_string(&path)
            ));
        }
        let mime = crate::attachments::mime_of(&path);
        if mime == "application/octet-stream" {
            return Err(format!(
                "view: {} is not an image kelir knows (png, jpg, gif, webp)",
                crate::util::path_string(&path)
            ));
        }
        let bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        if bytes > crate::attachments::MAX_BYTES {
            return Err(format!(
                "view: {} is {} (the limit is {})",
                crate::util::path_string(&path),
                crate::attachments::bytes_label(bytes),
                crate::attachments::bytes_label(crate::attachments::MAX_BYTES)
            ));
        }
        // An extension is a claim, not proof: the bytes have to start with a
        // format's signature and carry a header kelir can read a size from.
        // Queueing garbage would put it in every later request (the shared
        // conversation is poisoned by a provider 400 that repeats), so a file
        // that is not really an image is refused here, before the log (`S3`).
        match crate::attachments::sniff(&path) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(format!(
                    "view: {} does not look like an image (empty, or not a png, jpg, gif or webp)",
                    crate::util::path_string(&path)
                ));
            }
            Err(message) => return Err(format!("view: {message}")),
        }
        // `None` (the provider did not state the model's vision support) is a
        // "try it": refusing on a missing flag would block a model that works.
        if self.vision_support() == Some(false) {
            return Err(format!(
                "view: {} cannot take images: it is a text-only model, so the image would be refused by the provider (pick a vision model with `/model`, or `--model`). The conversation is unchanged.",
                self.model
            ));
        }
        Ok(path)
    }

    /// Whether the provider says the current model takes images: `Some(true)`
    /// and `Some(false)` are answers, `None` is "not stated" (the fetched or
    /// cached model list has no entry, or the entry has no vision field).
    fn vision_support(&self) -> Option<bool> {
        self.models
            .as_ref()
            .and_then(|models| models.vision_for(&self.model))
    }

    /// Copy one image into `<session>/attachments/` and return its relative
    /// path. Content that is already there is not copied twice.
    fn import_attachment(&self, path: &Path) -> Result<String, String> {
        let dir = crate::attachments::dir(&self.dir);
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("view: cannot create {}: {error}", dir.display()))?;
        // Keep the file where it is when it already lives in the session's
        // attachments (a prompt pointer names exactly that).
        if let Ok(rest) = path.strip_prefix(&self.dir) {
            return Ok(crate::util::path_string(rest));
        }
        // The sequence number counts the session's copies, so two different
        // files get two numbers (review N6: they all used to be `1-…`). Names
        // stay unique either way, but a human reading the session's
        // `attachments/` should be able to count them.
        let mut seen = std::collections::HashMap::new();
        let seq = crate::attachments::next_seq(&self.dir);
        let saved = crate::attachments::save(&self.dir, seq, path, &mut seen)
            .map_err(|error| format!("view: {error}"))?;
        Ok(saved.rel)
    }

    /// Record one `source` event: context that arrived after the start. It
    /// renders as a message at the tail, so the system prefix (and its cache)
    /// is untouched.
    fn record_source(
        &mut self,
        kind: crate::sources::SourceKind,
        name: &str,
        text: &str,
        path: &crate::sources::SourcePath,
    ) -> Result<(), String> {
        self.record(EventBody::Source {
            kind: kind.event_kind().to_string(),
            name: name.to_string(),
            text: Some(Payload::text(text)),
            path: path.file().map(crate::util::path_string),
        })?;
        // The context's current word on the source is now this message: the
        // watcher compares against this one hash, so the file on disk and the
        // watcher agree at once (no second announcement) and an edit back to an
        // older body is news (F1, F3).
        self.watch
            .add(kind, name, path, &crate::sources::hash_of(text));
        Ok(())
    }

    /// The per-boundary mtime/hash check of the recorded sources: a file
    /// edited on disk since it went into the context becomes one `source`
    /// message. The system prompt is never rewritten (that is what keeps the
    /// provider's cache warm).
    fn check_sources(&mut self) -> Result<usize, String> {
        // A source whose file is gone is one note per disappearance, never a
        // silent hole (N4).
        for removed in self.watch.removals() {
            self.record(EventBody::Error {
                location: "source".into(),
                message: format!(
                    "source: {} ({}) was deleted; the context still carries its last text",
                    crate::util::path_string(&removed.path),
                    removed.name
                ),
                retryable: false,
                status: None,
                attempt: None,
                retry_in_ms: None,
            })?;
            self.emit(UiEvent::Info(format!(
                "» source deleted on disk: {}",
                crate::util::path_string(&removed.path)
            )));
        }
        let changes = self.watch.changes();
        let count = changes.len();
        for change in changes {
            for note in change.notes {
                self.record(EventBody::Error {
                    location: "source".into(),
                    message: note,
                    retryable: false,
                    status: None,
                    attempt: None,
                    retry_in_ms: None,
                })?;
            }
            let path = change.path.clone();
            self.record_source(
                change.kind,
                &change.name,
                &change.text,
                &crate::sources::SourcePath::File(path.clone()),
            )?;
            self.emit(UiEvent::Info(format!(
                "» source changed on disk: {}",
                crate::util::path_string(&path)
            )));
        }
        if count > 0 {
            self.sync();
        }
        Ok(count)
    }

    fn record_messages(&mut self, messages: Vec<Message>) -> Result<(), String> {
        for message in messages {
            if message.from != "tui" {
                self.emit(UiEvent::Info(format!("› {}", one_line(&message.text))));
            }
            // A message deferred to the end of a turn (`/later`) is the same
            // message: the list of the images its text points at goes with it
            // (N5), or a queued prompt would be the one shape whose
            // `attachments[]` lied.
            let attachments = crate::attachments::paths_in(&message.text)
                .into_iter()
                .map(serde_json::Value::String)
                .collect();
            self.record(EventBody::User {
                text: Payload::text(message.text),
                from: message.from,
                delivery: message.delivery.to_string(),
                attachments,
            })?;
        }
        self.sync();
        Ok(())
    }

    // ---------------------------------------------------------- compaction

    /// The effort the first request will carry: the configured value, unless the
    /// provider's list says the model does not take the parameter. Recorded in
    /// `session_started`, so the log and the request agree from the first step.
    fn first_effort(&self) -> Option<String> {
        if !self.effort_allowed() {
            return None;
        }
        self.cfg
            .provider(&self.provider_name)
            .ok()
            .and_then(|provider| provider.reasoning_effort)
    }

    /// Whether the loop may ask this model for several tool calls in one
    /// message: the provider's list when it has an entry, else the built-in
    /// family table, so a first session (cold or slow list) still asks a model
    /// that can do it (STEER-6 review S2).
    fn parallel_calls(&self) -> bool {
        crate::models::parallel_calls(
            self.models.as_ref().map(|cache| cache.models.as_slice()),
            &self.model,
        )
    }

    /// Whether `reasoning_effort` may go in the body: only where the provider
    /// advertises the parameter, or for a model the list says nothing about
    /// when the person configured an effort explicitly (STEER-6 review S3).
    fn effort_allowed(&self) -> bool {
        let explicit = self
            .cfg
            .provider(&self.provider_name)
            .map(|provider| provider.reasoning_effort_explicit)
            .unwrap_or(false);
        crate::models::reasoning_effort_allowed(
            self.models.as_ref().map(|cache| cache.models.as_slice()),
            &self.model,
            explicit,
        )
    }

    /// The request for one step, with the two capability decisions applied.
    fn step_request(&self, messages: &[crate::chat::ChatMessage]) -> provider::BuiltRequest {
        self.capabilities(self.client.build_with(
            &self.model,
            messages,
            &self.tool_specs(),
            self.parallel_calls(),
        ))
    }

    /// Apply what this provider/model actually takes: `reasoning_effort` only
    /// where the provider advertises it or the config asked for it on an
    /// unknown model, and nothing this session already saw a 400 for.
    fn capabilities(&self, request: provider::BuiltRequest) -> provider::BuiltRequest {
        let mut request = request;
        if !self.effort_allowed() {
            request = request.without_field("reasoning_effort");
        }
        for field in &self.refused_fields {
            request = request.without_field(field);
        }
        request
    }

    /// The tool list sent to the provider: the specs for the names frozen at
    /// `session_started`, in that order. A session started before a tool existed
    /// keeps its old list, so the request prefix (and its cache) is unchanged.
    fn tool_specs(&self) -> Vec<Value> {
        let all = tools::specs();
        self.tool_names
            .iter()
            .filter_map(|name| {
                all.iter()
                    .find(|spec| spec["function"]["name"].as_str() == Some(name.as_str()))
                    .cloned()
            })
            .collect()
    }

    /// Compact the context if something asked for it: the `compact` tool, the
    /// `compact` verb, or the auto trigger at the configured fraction of the
    /// model's window.
    ///
    /// `Some(end)` when a verb interrupted the summarising request: the turn
    /// ends there, exactly as `stop` during a step would.
    async fn maybe_compact(&mut self, rx: &mut Inbox) -> Result<Option<TurnEnd>, String> {
        if self.compact_request.is_none() {
            // Nothing to compact before the first step: the estimate would be
            // little more than the system prompt, and the "conversation" is the
            // task alone.
            let has_history = self
                .events
                .iter()
                .any(|event| matches!(event.body, EventBody::StepStarted { .. }));
            if has_history && compact::plan(&self.events, self.cfg.compact_keep).is_some() {
                let folded = crate::fold::fold(&self.events, &self.dir)?;
                let estimate = compact::estimate_tokens(&folded.request_messages());
                // The provider's own count is the honest size; the estimate
                // covers the step after a compaction.
                let tokens = self.last_prompt_tokens.unwrap_or(estimate);
                let window = self.window;
                // After a compaction that did not shrink anything, wait for the
                // context to grow past it instead of paying for a summary on
                // every step (M3 review N1).
                let above_floor = self
                    .compact_floor
                    .map(|floor| tokens > floor)
                    .unwrap_or(true);
                if above_floor
                    && compact::should_auto(
                        window,
                        self.cfg.auto_compact_at,
                        self.cfg.compact_at_tokens,
                        tokens,
                    )
                {
                    self.compact_request = Some(CompactRequest {
                        by: "auto",
                        focus: None,
                    });
                }
            }
        }
        let Some(request) = self.compact_request.take() else {
            return Ok(None);
        };
        match self.compact(rx, &request).await {
            Ok(CompactRun::Applied(info)) => {
                self.emit(UiEvent::Info(format!(
                    "» compacted through seq {}: ~{} → ~{} tokens ({} pinned) · cache restarts",
                    info.through_seq, info.tokens_before, info.tokens_after, info.pinned
                )));
                if info.tokens_after >= info.tokens_before {
                    self.emit(UiEvent::Info(format!(
                        "» compact: it did not shrink the context (~{} → ~{} tokens); the event \
                         records both",
                        info.tokens_before, info.tokens_after
                    )));
                }
            }
            Ok(CompactRun::Nothing) => {
                self.emit(UiEvent::Info(
                    "» compact: nothing old enough to summarise".to_string(),
                ));
            }
            Ok(CompactRun::Skipped { before, would_be }) => {
                // Cost control (N1): a summary that cannot shrink the context
                // buys nothing, so it is not requested again until the context
                // grows. The floor is on the scale the trigger measures with —
                // the provider's own count when there is one — so the trigger
                // goes quiet instead of repeating the decision every step.
                let first_refusal = self.compact_floor.is_none();
                self.compact_floor = Some(self.last_prompt_tokens.unwrap_or(before));
                let message = format!(
                    "automatic compaction skipped: the summary and pins would not be smaller \
                     than the range they replace (~{before} → ~{would_be} tokens)"
                );
                self.emit(UiEvent::Info(format!("» compact skipped: {message}")));
                // One line in the log per episode, not one per step: the
                // exception is the decision, the repeats add nothing.
                if first_refusal {
                    self.record(EventBody::Error {
                        location: "compact".into(),
                        message,
                        retryable: false,
                        status: None,
                        attempt: None,
                        retry_in_ms: None,
                    })?;
                    self.sync();
                }
            }
            Ok(CompactRun::Stopped(reason)) => return Ok(Some(TurnEnd::Stopped(reason))),
            Ok(CompactRun::End) => return Ok(Some(TurnEnd::End)),
            Err(message) => {
                // A failed summary is not a failed turn: the context stays as
                // it was and the next step runs.
                self.record(EventBody::Error {
                    location: "compact".into(),
                    message,
                    retryable: true,
                    status: None,
                    attempt: None,
                    retry_in_ms: None,
                })?;
                self.sync();
            }
        }
        Ok(None)
    }

    /// The size of the request the fold would build with `preview` appended.
    fn preview_tokens(&self, preview: &EventBody, seq: u64) -> Result<u64, String> {
        let mut events = self.events.clone();
        events.push(LogEvent::new(seq, preview.clone()));
        let folded = crate::fold::fold(&events, &self.dir)?;
        Ok(compact::estimate_tokens(&folded.request_messages()))
    }

    /// Summarise the range and write the `compacted` event.
    ///
    /// The summarising request is a provider call like any other: verbs are
    /// read while it is in flight, so a `stop` does not wait for a large
    /// transcript to be summarised (M3 review S2), and the turn ends without a
    /// `compacted` event.
    async fn compact(
        &mut self,
        rx: &mut Inbox,
        request: &CompactRequest,
    ) -> Result<CompactRun, String> {
        let Some(plan) = compact::plan(&self.events, self.cfg.compact_keep) else {
            return Ok(CompactRun::Nothing);
        };
        // The transcript is the fold of the range, not the raw events: a
        // compaction that covers an earlier one summarises its summary instead
        // of losing it (B2).
        let range = crate::fold::range_events(&self.events, plan.through_seq);
        let folded_range = crate::fold::fold(&range, &self.dir)?;
        let transcript = compact::render_transcript(&folded_range.messages);
        // Both numbers describe the whole request the provider sees, so asking
        // "did it shrink?" is a real question about the context (N1).
        let whole = crate::fold::fold(&self.events, &self.dir)?;
        let tokens_before = compact::estimate_tokens(&whole.request_messages());
        let mut body = EventBody::Compacted {
            through_seq: plan.through_seq,
            summary: Payload::text(""),
            pinned_seqs: plan.pinned_seqs.clone(),
            by: request.by.to_string(),
            tokens_before,
            tokens_after: 0,
            cache_restart: true,
        };
        // The smallest possible result: an empty summary. If even that is not
        // smaller, no summary can be, so an automatic compaction declines
        // before paying for one.
        let smallest = self.preview_tokens(&body, self.writer.next_seq())?;
        if request.by == "auto" && smallest >= tokens_before {
            return Ok(CompactRun::Skipped {
                before: tokens_before,
                would_be: smallest,
            });
        }

        self.emit(UiEvent::Info(format!(
            "» compacting {} pinned message(s) through seq {} · ~{} tokens",
            plan.pinned_seqs.len(),
            plan.through_seq,
            tokens_before
        )));
        let summarising = self.capabilities(compact::summarise_request(
            &self.client,
            &self.model,
            &self.cfg.compact_prompt,
            &transcript,
            request.focus.as_deref(),
        ));
        let assembler = match self
            .stream(rx, RequestFor::Built(&summarising), true)
            .await?
        {
            StreamEnd::Assembled(assembler) => assembler,
            StreamEnd::Stopped(reason) => return Ok(CompactRun::Stopped(reason)),
            StreamEnd::End => return Ok(CompactRun::End),
            // A summary is tried once and has no budget to spend, so it can
            // never pause; if that ever changes, a paused summary is a summary
            // that did not happen.
            StreamEnd::Paused(_) => return Ok(CompactRun::End),
        };
        // The summary's spend belongs in the log like any step's, marked so a
        // reader (and `kelir ls`) can tell it is not a step of the turn
        // (M3 review N4).
        if let Some(usage) = &assembler.usage {
            self.record(EventBody::Usage {
                step: self.step,
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                cached_tokens: usage.cached_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                cost: usage.cost,
                allowance: usage.allowance.clone(),
                kind: Some("compact".into()),
            })?;
            self.emit(UiEvent::Usage {
                prompt: usage.prompt_tokens,
                completion: usage.completion_tokens,
                cached: usage.cached_tokens,
                cost: usage.cost,
            });
            self.sync();
        }
        let summary = assembler.text.trim().to_string();
        if summary.is_empty() {
            return Err("the summarising request returned no text".to_string());
        }
        if let EventBody::Compacted { summary: slot, .. } = &mut body {
            *slot = Payload::text(summary);
        }
        // The size after: fold the log as it *will* be, so the number in the
        // event is the one the next request is built from.
        let tokens_after = self.preview_tokens(&body, self.writer.next_seq())?;
        if request.by == "auto" && tokens_after >= tokens_before {
            return Ok(CompactRun::Skipped {
                before: tokens_before,
                would_be: tokens_after,
            });
        }
        if let EventBody::Compacted {
            tokens_after: slot, ..
        } = &mut body
        {
            *slot = tokens_after;
        }
        let pinned = plan.pinned_seqs.len();
        // A `source` message folded into the summary is no longer in the
        // context: the watcher will say what the file holds at the next
        // boundary, even when its bytes did not change (F1's compaction case).
        self.watch
            .forget_folded(&self.events, plan.through_seq, &plan.pinned_seqs);
        self.record(body)?;
        self.sync();
        // The next trigger measures the compacted fold, not the old size.
        self.last_prompt_tokens = None;
        self.compact_floor = None;
        Ok(CompactRun::Applied(CompactedInfo {
            through_seq: plan.through_seq,
            tokens_before,
            tokens_after,
            pinned,
        }))
    }

    // --------------------------------------------------------------- turns

    /// Run the session: one turn, then more when `--keep` or when messages are
    /// queued. Ends with `session_ended`.
    async fn run(&mut self, rx: &mut Inbox) -> u8 {
        let mut code = 0u8;
        // Every way out of the loop below sets it (the loop always runs at
        // least one turn).
        let mut reason;
        // A resumed session with nothing in flight and nothing new has no turn
        // to run: `kelir resume <id>` on a finished session reports and leaves.
        // A TUI resume still waits for the next message (`keep`).
        if self.resumed && !self.in_flight && !self.has_task && !self.keep {
            let _ = self.record(EventBody::SessionEnded {
                reason: "resumed".into(),
            });
            self.sync();
            return 0;
        }
        // A TUI session starts with no task: wait for the first message.
        if !self.has_task && self.keep {
            match self.idle(rx).await {
                Idle::Turn => {}
                Idle::End => {
                    let _ = self.record(EventBody::SessionEnded {
                        reason: "end".into(),
                    });
                    self.sync();
                    return 0;
                }
            }
        }
        let mut turn_retries = 0u32;
        loop {
            self.turn += 1;
            let end = self.drive_turn(rx).await;
            let failed = match &end {
                Ok(TurnEnd::Done) => {
                    let _ = self.record(EventBody::TurnEnded {
                        outcome: "done".into(),
                        reason: None,
                        final_text: self.final_text.clone().into(),
                    });
                    self.finish_turn("done");
                    code = 0;
                    reason = "done".into();
                    false
                }
                Ok(TurnEnd::Stopped(why)) => {
                    if !self.json && !self.tui && !why.is_empty() {
                        eprintln!("→ turn stopped: {why}");
                    }
                    let _ = self.record(EventBody::TurnEnded {
                        outcome: "stopped".into(),
                        reason: None,
                        final_text: self.final_text.clone().into(),
                    });
                    self.finish_turn("stopped");
                    reason = "stopped".into();
                    false
                }
                Ok(TurnEnd::End) => {
                    let _ = self.record(EventBody::TurnEnded {
                        outcome: "stopped".into(),
                        reason: None,
                        final_text: self.final_text.clone().into(),
                    });
                    self.finish_turn("stopped");
                    reason = "end".into();
                    false
                }
                // A pause is never a failure: nothing is broken and the fold is
                // intact. The hint is the whole point of `paused` over `error`:
                // it goes to stderr even under `--json` (stdout stays NDJSON)
                // and into the TUI's scrollback, never only into the log. A
                // one-shot run exits 0. `reason` tells a step cap from an
                // unavailable provider.
                Ok(TurnEnd::Paused(report)) => {
                    let message = report.message();
                    if report.reason == PauseReason::Steps {
                        if !self.tui {
                            eprintln!("→ {message}");
                        }
                    } else {
                        if !self.tui {
                            eprintln!("kelir: {message}");
                        }
                        self.emit(UiEvent::Info(format!("» {message}")));
                        self.final_text = message.clone();
                    }
                    let text = match report.reason {
                        PauseReason::Steps => self.final_text.clone(),
                        PauseReason::Provider => message.clone(),
                    };
                    let _ = self.record(EventBody::TurnEnded {
                        outcome: "paused".into(),
                        reason: Some(report.reason.as_str().into()),
                        final_text: text.into(),
                    });
                    self.finish_turn("paused");
                    code = 0;
                    reason = "paused".into();
                    false
                }
                Err(message) => {
                    if !self.tui {
                        eprintln!("kelir: {message}");
                    }
                    self.emit(UiEvent::Error(message.clone()));
                    let _ = self.record(EventBody::TurnEnded {
                        outcome: "error".into(),
                        reason: None,
                        final_text: String::new().into(),
                    });
                    self.finish_turn("error");
                    // A long-lived session survives a failed turn: the error is
                    // in the log and the next `say` gets another chance (M1
                    // review N6). A one-shot run still exits 1.
                    if !self.keep {
                        code = 1;
                    }
                    reason = "error".into();
                    true
                }
            };
            self.sync();
            // A resumed turn that failed on an error the provider called
            // retryable — an outage — gets the turn tried again: surviving one
            // is what `resume` is for, and a provider often recovers within
            // seconds. The budget is the same one a step gets, so a resume can
            // never turn into a loop against a dead provider.
            let retry_the_turn = failed
                && !self.keep
                && self.resumed
                && self.retryable_failure
                && turn_retries + 1 < provider::TURN_ATTEMPTS;
            if matches!(end, Ok(TurnEnd::End)) || (failed && !self.keep && !retry_the_turn) {
                break;
            }
            if retry_the_turn {
                turn_retries += 1;
                self.retryable_failure = false;
                code = 0;
                continue;
            }
            match self.take_turn_messages() {
                Some(TurnStart::End) => {
                    reason = "end".into();
                    break;
                }
                Some(TurnStart::Messages(messages)) => {
                    if let Err(message) = self.record_messages(messages) {
                        eprintln!("kelir: {message}");
                        code = 1;
                        reason = "error".into();
                        break;
                    }
                    continue;
                }
                None => {
                    if !self.keep {
                        break;
                    }
                    match self.idle(rx).await {
                        Idle::Turn => continue,
                        Idle::End => {
                            reason = "end".into();
                            break;
                        }
                    }
                }
            }
        }
        let _ = self.record(EventBody::SessionEnded { reason });
        self.sync();
        // The session is ending: the token it minted is no longer served by
        // anything, so it must not stay on disk for whoever comes next (STEER-7
        // review S1). Jobs that outlive the session keep their capabilities: a
        // resume adopts them from the registry.
        self.trust.retire();
        code
    }

    /// `--keep` and no turn running: take verbs until one starts a turn.
    async fn idle(&mut self, rx: &mut Inbox) -> Idle {
        loop {
            // A metadata fetch that started with the session lands here as well
            // as at `before_step`: an idle session answers `state` too, and a
            // list that arrived while nobody was asking should still be applied
            // (STEER-1 review S2).
            let landed = match self.models_fetch.as_mut() {
                Some(fetch) if fetch.is_running() => fetch.wait(IDLE_MODELS_WAIT).await,
                _ => None,
            };
            if let Some(list) = landed {
                self.apply_list(list);
                continue;
            }
            // A fetch still running must not strand its answer: blocking on
            // `rx.recv()` here would keep the idle session from ever applying
            // a list that lands a second later (the offline reason the banner
            // and `state` owe a person, STEER-1 review N4). So while one runs,
            // wait for a verb and for the next look at the fetch, whichever
            // comes first; with none running, wait for the verb alone.
            let incoming = match self.models_fetch.as_ref().map(Fetch::is_running) {
                Some(true) => {
                    tokio::select! {
                        incoming = rx.recv() => incoming,
                        _ = tokio::time::sleep(IDLE_MODELS_WAIT) => continue,
                    }
                }
                _ => rx.recv().await,
            };
            let Some(mut incoming) = incoming else {
                return Idle::End;
            };
            if let Some(note) = self.note_own(incoming.action.take()) {
                // Its own watcher: a job started or settled while idle. It is
                // queued like the `bg` report it stands in for, and the turn it
                // may start begins below, through `take_turn_messages`.
                self.scheduler.push(note);
            } else {
                // Nothing is running in idle, so a `stop` has nothing to stop:
                // queueing it would stop the *next* turn at its first boundary,
                // and answering "now" would be a lie. It is dropped, and both
                // the screen and the client are told so (M2 review N1).
                let bare_stop =
                    incoming.request.verb == "stop" && incoming.request.args.get("call").is_none();
                let pending = self.accept_as(incoming, bare_stop.then_some("dropped"));
                if let Some(pending) = pending {
                    if matches!(pending.verb, Verb::Stop { call: None, .. }) {
                        self.emit(UiEvent::Info("» stop: nothing is running".into()));
                    } else {
                        if matches!(pending.verb, Verb::Compact { .. }) {
                            self.emit(UiEvent::Info("» compact: applies at the next step".into()));
                        }
                        self.scheduler.push(pending);
                    }
                }
            }
            match self.take_turn_messages() {
                Some(TurnStart::End) => return Idle::End,
                Some(TurnStart::Messages(messages)) => {
                    if self.record_messages(messages).is_err() {
                        return Idle::End;
                    }
                    // A `cancel` queued in the same breath must still be
                    // applied (it is not a message and no boundary is coming).
                    if self.apply_actions().await.is_err() {
                        return Idle::End;
                    }
                    return Idle::Turn;
                }
                None => {}
            }
            if self.apply_actions().await.is_err() {
                return Idle::End;
            }
        }
    }

    async fn drive_turn(&mut self, rx: &mut Inbox) -> Result<TurnEnd, String> {
        self.final_text.clear();
        self.retryable_failure = false;
        self.emit(UiEvent::TurnStarted {
            turn: self.turn,
            provider: self.provider_name.clone(),
            model: self.model.clone(),
            limit: self.cfg.max_steps,
        });
        self.report(HerdrState::Working);
        self.turn_steps = 0;
        loop {
            // An explicit cap counts the steps of *this* turn: reaching it
            // pauses the turn (the work so far is kept, and the next message
            // opens a new turn with a fresh count). No cap at all, the
            // default, never stops a turn. The test comes *before* the
            // increment, so a paused turn's count is the number of steps that
            // ran, not one more: `state` says `2` of `2`, never `3` of `2`
            // (review S1).
            if let Some(cap) = self.cfg.max_steps
                && self.turn_steps >= cap
            {
                self.emit(UiEvent::Paused { steps: cap });
                return Ok(TurnEnd::Paused(PauseReport::steps(cap)));
            }
            self.turn_steps += 1;

            // ---------------------------------------------------- before_step
            // A metadata fetch that ran beside the session is applied here:
            // this is the first boundary that reads the window, so the list
            // has had the first step's worth of time to arrive (STEER-1
            // review S2).
            self.apply_models().await;
            let applied = self.apply(Boundary::BeforeStep).await?;
            if applied.end {
                return Ok(TurnEnd::End);
            }
            if let Some(reason) = applied.stopped {
                return Ok(TurnEnd::Stopped(reason));
            }

            // Compaction applies here and nowhere else: after the previous
            // batch settled, before the next request, never between a tool call
            // and its result. All three triggers arrive as one request. A
            // `stop` or `end` during the summarising request ends the turn.
            if let Some(end) = self.maybe_compact(rx).await? {
                return Ok(end);
            }
            // Sources edited on disk while the turn ran: one `source` message
            // each, at the tail. Never a system rewrite.
            self.check_sources()?;
            // A key that is within two minutes of its expiry is re-read before
            // the request: opencode refreshes its token whenever it runs, so a
            // copied-out one goes stale before its own `expires` does
            // (STEER-2).
            self.refresh_expiring_key();

            let folded = crate::fold::fold(&self.events, &self.dir)?;
            // Ask for several calls in one message when the provider says the
            // model takes them, or (cold or slow list) when the model's family
            // is known to; the loop already runs a batch concurrently and
            // appends results in call order. `reasoning_effort` goes only where
            // the provider advertises it (or where the config asked for it on
            // an unknown model). Both do not depend on the cache being warm
            // (STEER-6 review S2/S3).
            let request = self.step_request(&folded.request_messages());
            let request_text = serde_json::to_string(&request.body).unwrap_or_default();
            self.step += 1;
            let step = self.step;
            self.record(EventBody::StepStarted {
                step,
                provider: self.provider_name.clone(),
                model: self.model.clone(),
                request_hash: crate::util::fnv1a64_hex(request_text.as_bytes()),
            })?;
            self.emit(UiEvent::Step {
                step,
                provider: self.provider_name.clone(),
                model: self.model.clone(),
                turn_steps: self.turn_steps,
                limit: self.cfg.max_steps,
            });

            let assembler = match self.stream(rx, RequestFor::Step, false).await? {
                StreamEnd::Assembled(assembler) => assembler,
                StreamEnd::Stopped(reason) => return Ok(TurnEnd::Stopped(reason)),
                StreamEnd::End => return Ok(TurnEnd::End),
                StreamEnd::Paused(report) => return Ok(TurnEnd::Paused(report)),
            };
            let calls = assembler.tool_calls();
            let text = assembler.text.clone();

            self.record(EventBody::Assistant {
                step,
                text: text.clone().into(),
                reasoning: if assembler.reasoning.is_empty() {
                    None
                } else {
                    Some(assembler.reasoning.clone().into())
                },
                tool_calls: calls
                    .iter()
                    .map(|call| LoggedToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        args: Payload::value(call.args.clone()),
                    })
                    .collect(),
                finish_reason: assembler.finish_reason.clone(),
            })?;
            self.emit(UiEvent::Assistant { text: text.clone() });
            if let Some(usage) = &assembler.usage {
                self.record(EventBody::Usage {
                    step,
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                    cached_tokens: usage.cached_tokens,
                    cache_write_tokens: usage.cache_write_tokens,
                    cost: usage.cost,
                    allowance: usage.allowance.clone(),
                    kind: None,
                })?;
                // The size of the context the provider just saw: what the auto
                // trigger compares to the window.
                self.last_prompt_tokens = Some(usage.prompt_tokens);
                self.emit(UiEvent::Usage {
                    prompt: usage.prompt_tokens,
                    completion: usage.completion_tokens,
                    cached: usage.cached_tokens,
                    cost: usage.cost,
                });
            }
            // Even a turn that ends up stopped keeps the last thing the model
            // said in `turn_ended.final_text`.
            self.final_text = text.clone();
            self.sync();
            // Documented test hook: die where a crash would leave the log
            // between an assistant message and its tool calls.
            crash_at("usage");

            if !self.json && self.echo_assistant && !text.is_empty() {
                println!("{text}");
                let _ = std::io::stdout().flush();
            }

            if calls.is_empty() {
                self.final_text = text;
                // ---------------------------------------------------- after_step
                let applied = self.apply(Boundary::AfterStep).await?;
                if applied.end {
                    return Ok(TurnEnd::End);
                }
                if let Some(reason) = applied.stopped {
                    return Ok(TurnEnd::Stopped(reason));
                }
                // A `say` that arrived while the model was answering gets
                // another step in the same turn.
                if applied.messages > 0 {
                    continue;
                }
                return Ok(TurnEnd::Done);
            }

            // ------------------------------------------------------ tool batch
            self.live_calls = calls.iter().map(|call| call.id.clone()).collect();
            let batch = self.run_tools(rx, step, &calls).await?;
            self.live_calls.clear();
            let applied = self.apply(Boundary::AfterTools).await?;
            if batch.end || applied.end {
                return Ok(TurnEnd::End);
            }
            if let Some(reason) = batch.stopped {
                return Ok(TurnEnd::Stopped(reason));
            }
            if let Some(reason) = applied.stopped {
                return Ok(TurnEnd::Stopped(reason));
            }
        }
    }

    /// A 400 that names one of the capability fields kelir added: remove it
    /// from the request being retried and remember it, so no later request in
    /// this session pays the same 400 (STEER-6 review S3). Answers `true` when
    /// the request should be retried, once, without the field.
    fn drop_refused_field(
        &mut self,
        request: &mut provider::BuiltRequest,
        error: &provider::ProviderError,
        dropped: &mut Option<String>,
    ) -> bool {
        // One drop per request: a second 400 means the field was not the
        // (only) reason, and looping through the body would hide it.
        if dropped.is_some() || !error.message.starts_with("http 400") {
            return false;
        }
        let field = ["reasoning_effort", "parallel_tool_calls"]
            .into_iter()
            .find(|field| {
                request.has_field(field)
                    && !self.refused_fields.contains(*field)
                    && provider::names_field(&error.message, field)
            });
        let Some(field) = field else {
            return false;
        };
        *request = request.without_field(field);
        self.refused_fields.insert(field.to_string());
        *dropped = Some(field.to_string());
        true
    }

    /// Re-read a key that is about to expire, before the next request uses it.
    ///
    /// The credential the log's source published may be minutes from its
    /// `expires`; opencode refreshes its own token whenever it runs, so reading
    /// the source again is the cheapest fix there is. A re-read that fails or
    /// hands back the same value is not an error here: the request goes out, and
    /// if the provider refuses it the 401/403 path does the rest. Never printed,
    /// never logged beyond a one-line `error` event that names the source.
    fn refresh_expiring_key(&mut self) {
        let Some(expires) = self.active_provider.key_expires_at_ms else {
            return;
        };
        let now = crate::models::now_ms();
        if expires.saturating_sub(now) >= credentials::EXPIRY_MARGIN.as_millis() as u64 {
            return;
        }
        let before = self.active_provider.key.clone();
        match credentials::resolve(&mut self.active_provider) {
            Ok(fresh) => {
                if self.active_provider.key == before {
                    // Same value, so the same expiry: the provider will say so.
                    return;
                }
                if let Ok(client) = Client::new(
                    self.active_provider.clone(),
                    &self.id,
                    self.cfg.cache_ttl.clone(),
                ) {
                    self.client = client;
                }
                let _ = self.record(EventBody::Error {
                    location: "provider".into(),
                    message: format!(
                        "the key from {} was about to expire ({}); re-read it, new expiry {}",
                        fresh.describe(),
                        credentials::format_ms(expires),
                        fresh.expiry_text()
                    ),
                    retryable: false,
                    status: None,
                    attempt: None,
                    retry_in_ms: None,
                });
            }
            Err(message) => {
                // Not fatal: the request goes out with the key we have.
                if !self.json && !self.tui {
                    eprintln!("kelir: {message}");
                }
            }
        }
    }

    /// Re-read the key after a refusal that means "this credential is stale"
    /// (HTTP 401/403) and rebuild the request with it, once per request.
    ///
    /// opencode refreshes its token whenever it runs, so a re-read can return a
    /// live one even though ours expired; when it does not, the failure stays
    /// the failure it was. The OAuth refresh endpoint is deliberately not
    /// called: opencode rotates the refresh token, and a second refresher would
    /// invalidate the account it shares with us.
    fn reread_credential(
        &mut self,
        error: &provider::ProviderError,
        request: &mut provider::BuiltRequest,
        attempt: u32,
        reread: &mut bool,
    ) -> bool {
        if *reread || !matches!(error.status, Some(401 | 403)) {
            return false;
        }
        *reread = true;
        let before = self.active_provider.key.clone();
        let fresh = match credentials::resolve(&mut self.active_provider) {
            Ok(fresh) => fresh,
            Err(message) => {
                self.record_error(message, error.status, attempt);
                return false;
            }
        };
        if self.active_provider.key == before {
            // The source had nothing newer: retrying the same key buys nothing,
            // and the provider's own message (a refusal, not a wait) is the
            // answer.
            return false;
        }
        if let Ok(client) = Client::new(
            self.active_provider.clone(),
            &self.id,
            self.cfg.cache_ttl.clone(),
        ) {
            self.client = client;
        }
        let messages = crate::fold::fold(&self.events, &self.dir)
            .map(|folded| folded.request_messages())
            .unwrap_or_default();
        *request = self.step_request(&messages);
        self.record_error(
            format!(
                "{}: re-read the key from {} and retrying once",
                error.message,
                fresh.describe()
            ),
            error.status,
            attempt,
        );
        true
    }

    /// An `error` event and a sync, for the two credential paths.
    fn record_error(&mut self, message: String, status: Option<u16>, attempt: u32) {
        let _ = self.record(EventBody::Error {
            location: "provider".into(),
            message,
            retryable: false,
            status,
            attempt: Some(attempt),
            retry_in_ms: None,
        });
    }

    /// Stream one response, taking verbs while it is in flight. A `stop` ends
    /// the turn without committing the half-response; anything else waits for
    /// the next boundary.
    ///
    /// A retryable failure — 429, 408, 5xx, a dead socket, a stream that ended
    /// early **before any tool call of the step began** — is waited out and the
    /// same request re-sent from the same fold. Every wait is an `error` event
    /// (`where: "provider"`, `retryable: true`, the status, the attempt and the
    /// wait) plus a line on the screen, and it is interruptible: `stop`,
    /// `say --now`, `later`, `model`, `end` and a signal take effect at once.
    /// When the request's waiting budget is spent the turn **pauses** instead of
    /// failing: the fold is intact, so the next `say` retries from it.
    ///
    /// `summary` is the compaction's own request: its text is not rendered as
    /// the answer being streamed, the crash hook stays where a *step* would
    /// land, and it is tried exactly once (a summary that failed the first time
    /// buys nothing the second; M3 review N4).
    async fn stream(
        &mut self,
        rx: &mut Inbox,
        request: RequestFor<'_>,
        summary: bool,
    ) -> Result<StreamEnd, String> {
        let policy = self.cfg.retry;
        let budget_wait = if summary {
            Some(Duration::ZERO)
        } else {
            policy.max_wait
        };
        let mut budget = crate::retry::Budget::new(budget_wait, policy.max_attempts);
        let mut attempt = 1u32;
        let mut rng = crate::retry::Xorshift::from_clock();
        let mut inbox_open = true;
        // The first request is the one `drive_turn` built (and hashed into
        // `step_started`); a retry is a new request and is built again.
        let mut built = request.build(self);
        // A `model`/`say --now` that cut a wait short already applied its verb,
        // so the next request is built (new model, new fold) but the
        // `before_step` boundary is not run again.
        let mut rebuild = false;
        // The body field a 400 asked kelir to drop, once per request
        // (STEER-6 review S3): the second 400 is the answer, not a queue of
        // fields to keep removing.
        let mut dropped: Option<String> = None;
        // Whether this request already re-read the key once (see
        // [`Session::reread_credential`]).
        let mut reread = false;
        loop {
            // The governor (opt-in) can make this request wait for a slot that
            // another local kelir holds. That wait is visible and interruptible
            // like any other. A compaction's summary is a provider request on
            // the same account, so it takes a slot like every other request
            // (STEER-5 review S1; only a turn's first request used to).
            let mut slot: Option<crate::governor::Slot> = None;
            if let Some(governor) = self.governor.clone() {
                match self.await_slot(rx, &governor, &mut inbox_open).await? {
                    SlotWait::Taken(taken) => slot = taken,
                    SlotWait::Interrupted(end) => return Ok(end),
                }
            }
            // A *retry* is a new request, so a wait is a `before_step`
            // boundary: a `model` verb that arrived during the wait changes the
            // model, a queued `say` joins the fold, and `end`/`stop` take
            // effect here. The brief asks for exactly that ("`model` (the retry
            // uses the new model)"), and the `verb` event already records the
            // swap. The first attempt is not one: `drive_turn` applied that
            // boundary before it built the request, and applying it again here
            // would swallow a verb the caller has already accounted for.
            if !summary && attempt > 1 {
                let applied = self.apply(Boundary::BeforeStep).await?;
                if applied.end {
                    return Ok(StreamEnd::End);
                }
                if let Some(reason) = applied.stopped {
                    return Ok(StreamEnd::Stopped(reason));
                }
                rebuild = true;
            }
            if rebuild {
                built = request.build(self);
                rebuild = false;
            }
            budget.record_attempt();
            let outcome = self.attempt(rx, &built, summary, &mut inbox_open).await;
            // The slot is held for exactly one request: another local kelir can
            // start as soon as this one's answer is in.
            drop(slot);
            let error = match outcome {
                Attempt::End(end) => return Ok(end),
                Attempt::Ok(assembler) => return Ok(StreamEnd::Assembled(assembler)),
                Attempt::Failed(error) => error,
            };
            // A 400 that names a field kelir put in the body: drop it, remember
            // it for the session, and try once more with the same request
            // without it. Not a rate limit or a server error — the same request
            // would fail the same way — so it is not the retryable *wait* budget
            // (STEER-6 review S3). `refused_fields` (filled by the drop) keeps
            // the field out of every later request this session builds.
            if self.drop_refused_field(&mut built, &error, &mut dropped) {
                self.record(EventBody::Error {
                    location: "provider".into(),
                    message: format!(
                        "{}: retrying without `{}` and leaving it out for the rest of the session",
                        error.message,
                        dropped.as_deref().unwrap_or("?")
                    ),
                    retryable: false,
                    status: error.status,
                    attempt: Some(attempt),
                    // Not a wait: the retry is sent at once, so no countdown.
                    retry_in_ms: None,
                })?;
                self.sync();
                continue;
            }
            // The credential went stale: opencode rotates its own token, so the
            // store may hold a fresh one already (STEER-2). One re-read per
            // request, then the request is sent again. Never our own OAuth
            // refresh: rotating the refresh token would break opencode.
            if self.reread_credential(&error, &mut built, attempt, &mut reread) {
                self.sync();
                continue;
            }
            // A summary is tried once; a step within its budget.
            let will_retry = error.retryable && !summary && budget.fits(Duration::ZERO);
            // The wait, decided before it is recorded so the event can say how
            // long it is (and the screen can count it down).
            let wait = will_retry
                .then(|| crate::retry::delay(&policy, attempt - 1, error.retry_after, &mut rng))
                .filter(|wait| budget.fits(*wait));
            self.record(EventBody::Error {
                location: "provider".into(),
                message: error.message.clone(),
                retryable: error.retryable,
                status: error.status,
                attempt: Some(attempt),
                retry_in_ms: wait.map(|wait| wait.as_millis() as u64),
            })?;
            if !will_retry {
                if !summary {
                    self.retryable_failure = error.retryable;
                }
                // A retryable failure whose budget is spent is an outage, not a
                // bug: the turn pauses and a later `say` starts the next
                // attempt from the same fold.
                if error.retryable && !summary {
                    self.sync();
                    return Ok(StreamEnd::Paused(PauseReport {
                        reason: PauseReason::Provider,
                        attempts: budget.attempts(),
                        waited: budget.spent(),
                        detail: error.reason(),
                    }));
                }
                return Err(error.message);
            }
            self.sync();
            // A wait the budget cannot afford (or the count cap reached) means
            // the provider asked for longer than what is left: pausing now is
            // honest, waiting a little and failing is not.
            let Some(wait) = wait else {
                self.sync();
                return Ok(StreamEnd::Paused(PauseReport {
                    reason: PauseReason::Provider,
                    attempts: budget.attempts(),
                    waited: budget.spent(),
                    detail: error.reason(),
                }));
            };
            budget.charge(wait);
            self.show_wait(error.status, wait, attempt);
            match self.wait_for(rx, wait, &mut inbox_open).await? {
                Waited::Timeout => {}
                // `model`/`say --now`: a *new* request, sent now, with a fresh
                // slot and a fresh budget. The wait belonged to a request that
                // is no longer the one being made (the review's "a model swap
                // to a provider that is not rate limited must not sit out the
                // old provider's wait"), and the message must reach the model
                // now, not at the end of a `Retry-After` that can be days long.
                Waited::Retry => {
                    self.emit(UiEvent::RetryingDone);
                    budget = crate::retry::Budget::new(budget_wait, policy.max_attempts);
                    attempt = 1;
                    rebuild = true;
                    continue;
                }
                // A verb ended the turn during the wait: the wait's note is
                // cleared here too, so no swap/`end` race can leave a stale
                // countdown behind (STEER-5 review N4).
                Waited::End(end) => {
                    self.emit(UiEvent::RetryingDone);
                    return Ok(*end);
                }
            }
            self.emit(UiEvent::RetryingDone);
            attempt = attempt.saturating_add(1);
        }
    }

    /// One provider request, with verbs taken while it is in flight.
    async fn attempt(
        &mut self,
        rx: &mut Inbox,
        request: &provider::BuiltRequest,
        summary: bool,
        inbox_open: &mut bool,
    ) -> Attempt {
        let client = self.client.clone();
        // Live text goes to the UI as it arrives; the callback is a send to an
        // unbounded channel, so it can never stall the stream.
        let ui = if summary { None } else { self.ui.clone() };
        // Documented test hook: die with half a response streamed and nothing
        // committed, which is where a crash in the stream lands.
        let crash_mid_stream = !summary && crash_armed("mid_stream");
        let mut on_delta = move |delta: crate::stream::Delta| {
            if let crate::stream::Delta::Text(text) = &delta {
                if crash_mid_stream {
                    crash_now("mid_stream");
                }
                if let Some(ui) = &ui {
                    let _ = ui.send(UiEvent::Delta(text.clone()));
                }
            }
        };
        let future = client.stream_with(request, &mut on_delta);
        tokio::pin!(future);
        let mut outcome: Option<Result<crate::stream::Assembler, provider::ProviderError>> = None;
        let mut interrupt: Option<StreamEnd> = None;
        while outcome.is_none() && interrupt.is_none() {
            tokio::select! {
                result = &mut future => outcome = Some(result),
                incoming = rx.recv(), if *inbox_open => {
                    match incoming {
                        Some(incoming) => self.take_interrupt(incoming, &mut interrupt),
                        None => *inbox_open = false,
                    }
                }
            }
        }
        if let Some(end) = interrupt {
            return Attempt::End(end);
        }
        match outcome.expect("the stream ended without a result") {
            Ok(assembler) => Attempt::Ok(assembler),
            Err(error) => Attempt::Failed(error),
        }
    }

    /// Say what is happening and start the status line's countdown. The `error`
    /// event was already recorded, so NDJSON carries it; this is the human line.
    fn show_wait(&mut self, status: Option<u16>, wait: Duration, attempt: u32) {
        let line = crate::retry::wait_line(status, wait, attempt);
        self.emit(UiEvent::Info(format!("» {line}")));
        if !self.json && !self.tui {
            eprintln!("kelir: {line}");
        }
        self.emit(UiEvent::Retrying {
            line,
            until: Instant::now() + wait,
            status,
            attempt,
        });
    }

    /// Wait `wait`, taking verbs.
    ///
    /// A wait is where a person's correction must not be made to sit still
    /// (STEER-5 review B2): `stop`, `end` and a signal end the turn now,
    /// `say --now` and `model` end the *wait* now — the verb is applied, and
    /// the caller re-sends the request at once rather than finishing somebody
    /// else's `Retry-After` (up to seven days of it). `later`, `tools`,
    /// `compact` and `skills` apply at their boundary as always, and a queued
    /// `say` joins the fold at the next boundary. A signal is handled the same
    /// way as `stop`: it must not kill a session mid-wait while the log says
    /// nothing.
    async fn wait_for(
        &mut self,
        rx: &mut Inbox,
        wait: Duration,
        inbox_open: &mut bool,
    ) -> Result<Waited, String> {
        if wait.is_zero() {
            return Ok(Waited::Timeout);
        }
        // An interrupt that landed before this wait began is still pending (the
        // flag is level-triggered and no one else consumes it), so a wait never
        // has to be entered at exactly the right instant to see it — the race
        // that made the Linux gate flake (STEER-5 fixes round 2).
        let deadline = tokio::time::sleep(wait);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => return Ok(Waited::Timeout),
                _ = crate::signals::interrupted() => {
                    // One interrupt ends one turn: clear the level so the next
                    // wait is not ended by the same Ctrl-C.
                    crate::signals::take();
                    // The turn ends `stopped`, exactly like `stop`: the log
                    // says so and a one-shot run exits 0.
                    return Ok(Waited::End(Box::new(StreamEnd::Stopped(
                        "interrupted (Ctrl-C)".into(),
                    ))));
                }
                incoming = rx.recv(), if *inbox_open => match incoming {
                    Some(incoming) => {
                        let mut end: Option<StreamEnd> = None;
                        let resume = self.take_wait_verb(incoming, &mut end).await?;
                        if let Some(end) = end {
                            return Ok(Waited::End(Box::new(end)));
                        }
                        if resume {
                            return Ok(Waited::Retry);
                        }
                    }
                    None => *inbox_open = false,
                }
            }
        }
    }

    /// Handle one verb that arrived *during* a retry or slot wait. Returns
    /// whether the wait should end with the request re-sent at once.
    async fn take_wait_verb(
        &mut self,
        incoming: Incoming,
        interrupt: &mut Option<StreamEnd>,
    ) -> Result<bool, String> {
        // `say --now` and `model` are applied here and now, so they are
        // accepted and answered as such: `applies: "now"`, with the state the
        // next request will really see (STEER-5 review N1).
        if at_once_verb(&incoming.request) {
            return Ok(match self.accept_at_once(incoming).await {
                AtOnce::Applied(end) => {
                    if let Some(end) = end {
                        *interrupt = Some(*end);
                    }
                    true
                }
                // A refused verb changed nothing: the wait it arrived in goes on.
                AtOnce::Rejected => false,
            });
        }
        let Some(pending) = self.accept(incoming) else {
            return Ok(false);
        };
        match &pending.verb {
            Verb::Stop { call: None, reason } => {
                *interrupt = Some(StreamEnd::Stopped(
                    reason.clone().unwrap_or_else(|| "stopped by user".into()),
                ));
                Ok(false)
            }
            Verb::End => {
                *interrupt = Some(StreamEnd::End);
                Ok(false)
            }
            // `say --now` and `model` never reach here: the branch above
            // applies them through `accept_at_once`, which is what makes the
            // ack truthful about a swap that already happened.
            Verb::Say { .. } | Verb::Model { .. } => {
                let mut applied = Option::<StreamEnd>::None;
                self.apply_one(pending, &mut applied).await?;
                if let Some(end) = applied {
                    *interrupt = Some(end);
                }
                Ok(true)
            }
            _ => {
                // Anything else keeps the boundary it asked for (`later` at the
                // turn end, a tool/skill change at `before_step`).
                self.scheduler.push(pending);
                Ok(false)
            }
        }
    }

    /// Apply one verb now (the body of [`Session::apply`]'s loop for one
    /// `Pending`).
    async fn apply_one(
        &mut self,
        pending: Pending,
        end: &mut Option<StreamEnd>,
    ) -> Result<(), String> {
        let mut applied = Applied::default();
        self.apply_pending(pending, &mut applied).await?;
        if applied.end {
            *end = Some(StreamEnd::End);
        }
        if let Some(reason) = applied.stopped {
            *end = Some(StreamEnd::Stopped(reason));
        }
        Ok(())
    }

    /// Wait for a governor slot, polling (there is nothing to be told about: the
    /// slot appears when another process closes its handle). The status line
    /// says so while it lasts.
    async fn await_slot(
        &mut self,
        rx: &mut Inbox,
        governor: &crate::governor::Governor,
        inbox_open: &mut bool,
    ) -> Result<SlotWait, String> {
        let mut announced = false;
        loop {
            if let Some(slot) = governor.try_acquire() {
                if announced {
                    self.emit(UiEvent::RetryingDone);
                }
                return Ok(SlotWait::Taken(Some(slot)));
            }
            if !announced {
                announced = true;
                let line = format!("waiting for a provider slot ({})", governor.slots());
                self.emit(UiEvent::Info(format!("» {line} …")));
                self.emit(UiEvent::Retrying {
                    line,
                    until: Instant::now() + Duration::from_secs(86_400),
                    // A slot wait is not a provider retry: no status, no attempt,
                    // so the state row keeps saying what it was doing.
                    status: None,
                    attempt: 0,
                });
            }
            match self.wait_for(rx, SLOT_POLL, inbox_open).await? {
                // The slot poll is a poll: a verb that ends the *wait* just
                // means "look again", and the next look takes the slot if it is
                // free. The verb itself was already applied or queued.
                Waited::Timeout | Waited::Retry => {}
                Waited::End(end) => {
                    self.emit(UiEvent::RetryingDone);
                    return Ok(SlotWait::Interrupted(*end));
                }
            }
        }
    }

    /// Handle one verb that arrived while a request was in flight. `stop` and
    /// `end` interrupt now; anything else waits for its boundary.
    fn take_interrupt(&mut self, incoming: Incoming, interrupt: &mut Option<StreamEnd>) {
        if let Some(pending) = self.accept(incoming) {
            match pending.verb {
                Verb::Stop { call: None, reason } => {
                    *interrupt = Some(StreamEnd::Stopped(
                        reason.unwrap_or_else(|| "stopped by user".into()),
                    ));
                }
                Verb::End => *interrupt = Some(StreamEnd::End),
                _ => self.scheduler.push(pending),
            }
        }
    }

    /// Run one batch of calls concurrently, taking verbs while it runs.
    /// `stop`/`say --now` kill the process trees; results are still committed
    /// in call order, and a user message never lands between a call and its
    /// result.
    async fn run_tools(
        &mut self,
        rx: &mut Inbox,
        step: u32,
        calls: &[ToolCall],
    ) -> Result<Batch, String> {
        let mut plan: Vec<(String, ToolCall)> = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            let started = self.record(EventBody::ToolStarted {
                call_id: call.id.clone(),
                name: call.name.clone(),
                input: Payload::value(call.args.clone()),
                pid: None,
                // The call's own directory name: a resume adopts the call's job
                // id from the log, not from a directory a child could make
                // (STEER-7 review B1).
                dir: Some(format!("{}-{}-{}", self.writer.next_seq(), step, index + 1)),
            })?;
            self.emit(UiEvent::ToolStarted {
                name: call.name.clone(),
                input: call.args.clone(),
            });
            let dir = format!("{}-{}-{}", started.seq, step, index + 1);
            // The job registry (STEER-7): a call's output dir name is the id
            // its `bg` reports carry, so the session registers it — with a
            // capability minted here and handed to that call's shim only —
            // before the call can report anything (review B1). A machine with
            // no secure randomness still runs the call: it simply has no job
            // id it will answer for.
            let _ = self.trust.issue_call(&dir, &call.id);
            plan.push((dir, call.clone()));
        }
        self.sync();
        // Documented test hook: die after the intent is durable, before any
        // process started, which is where a crash right after the log write
        // lands.
        crash_at("tool_started");

        let count = plan.len();
        let (done_tx, mut done_rx) =
            mpsc::channel::<(usize, ToolOutcome, Duration)>(count.max(1) * 2);
        // A call whose gate closed before its task was polled never starts: that
        // is the `skipped` case.
        let gates: Vec<Arc<AtomicBool>> = (0..count)
            .map(|_| Arc::new(AtomicBool::new(false)))
            .collect();
        let mut handles = Vec::with_capacity(count);
        for (index, (dir, call)) in plan.iter().enumerate() {
            let ctx = self.ctx.clone();
            let dir = dir.clone();
            let name = call.name.clone();
            let args = call.args.clone();
            let disabled = self.disabled.contains(&call.name);
            // `compact` is a native tool whose work is the session's: a request
            // for the next boundary, answered here like any other call.
            let preset = if disabled {
                Some(ToolOutcome::disabled(&name))
            } else if name == "compact" {
                let focus = call
                    .args
                    .get("focus")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
                    .filter(|focus| !focus.trim().is_empty());
                self.compact_request = Some(CompactRequest {
                    by: "agent",
                    focus: focus.clone(),
                });
                Some(ToolOutcome::ok(compact_queued(&focus)))
            } else {
                None
            };
            let gate = gates[index].clone();
            let tx = done_tx.clone();
            let call_id = call.id.clone();
            handles.push(tokio::spawn(async move {
                let started = Instant::now();
                let outcome = if gate.load(Ordering::SeqCst) {
                    ToolOutcome::skipped("stopped before it started")
                } else if let Some(preset) = preset {
                    preset
                } else {
                    tools::dispatch(&name, &args, &ctx, &dir, &call_id).await
                };
                let _ = tx.send((index, outcome, started.elapsed())).await;
            }));
        }
        drop(done_tx);
        // Documented test hook: die with the calls running and their side
        // effects already made, which is where a crash mid-tool lands.
        if crash_armed("mid_tool") {
            tokio::time::sleep(Duration::from_millis(150)).await;
            crash_now("mid_tool");
        }

        let mut slots: Vec<Option<(ToolOutcome, Duration)>> = (0..count).map(|_| None).collect();
        // Killed calls: their result becomes `stopped by user: <reason>`.
        let mut forced: Vec<Option<String>> = (0..count).map(|_| None).collect();
        // A grace window per killed call, never one for the batch: a sibling
        // that legitimately runs longer must not be cut off (M1 review F1).
        let mut deadlines: Vec<Option<Instant>> = (0..count).map(|_| None).collect();
        let mut batch = Batch::default();

        while slots.iter().any(Option::is_none) {
            // A call whose own window ran out is settled here; the rest of the
            // batch is untouched.
            let now = Instant::now();
            for index in 0..count {
                let Some(end) = deadlines[index] else {
                    continue;
                };
                if now < end || slots[index].is_some() {
                    continue;
                }
                deadlines[index] = None;
                let reason = forced[index].clone().unwrap_or_default();
                slots[index] = Some((ToolOutcome::stopped(&reason), Duration::ZERO));
                handles[index].abort();
                self.settle_call(&plan[index].1.id);
            }
            tokio::select! {
                item = done_rx.recv() => match item {
                    Some((index, outcome, elapsed)) => {
                        if let Some(deadline) = deadlines.get_mut(index) {
                            *deadline = None;
                        }
                        if let Some(slot) = slots.get_mut(index) {
                            *slot = Some((outcome, elapsed));
                        }
                        if let Some((_, call)) = plan.get(index) {
                            self.settle_call(&call.id);
                        }
                    }
                    None => break,
                },
                incoming = rx.recv() => {
                    let Some(incoming) = incoming else { break };
                    let Some(pending) = self.accept(incoming) else { continue };
                    match pending.verb.clone() {
                        Verb::Stop { call: Some(id), reason } => {
                            let reason = reason.unwrap_or_default();
                            if let Some(index) = plan.iter().position(|(_, call)| call.id == id) {
                                gates[index].store(true, Ordering::SeqCst);
                                self.ctx.running.kill(&plan[index].0);
                                if slots[index].is_none() {
                                    forced[index].get_or_insert(reason);
                                    deadlines[index] = Some(Instant::now() + STOP_GRACE);
                                }
                            }
                        }
                        Verb::Stop { call: None, reason } => {
                            let reason = reason.unwrap_or_default();
                            self.kill_batch(
                                &plan,
                                &slots,
                                &gates,
                                &mut forced,
                                &mut deadlines,
                                &reason,
                            );
                            batch.stopped = Some(reason);
                        }
                        Verb::Say { now: true, .. } => {
                            // Stop the running calls, settle, and let the
                            // message in at the after_tools boundary.
                            self.kill_batch(&plan, &slots, &gates, &mut forced, &mut deadlines, "");
                            self.scheduler.push(pending);
                        }
                        Verb::End => {
                            self.kill_batch(&plan, &slots, &gates, &mut forced, &mut deadlines, "");
                            batch.end = true;
                        }
                        _ => self.scheduler.push(pending),
                    }
                }
                _ = tokio::time::sleep(STOP_POLL), if deadlines.iter().any(Option::is_some) => {
                    // A call that started just after the kill is taken down too,
                    // so no orphan keeps working after the turn moved on.
                    let reason = batch.stopped.as_deref().unwrap_or("");
                    self.retake(&plan, &slots, &deadlines, &mut forced, reason);
                }
            }
        }

        // Anything still unsettled is settled by the session: aborted, and
        // reported as stopped (it ran) or skipped (it never did).
        for handle in &handles {
            handle.abort();
        }
        for (index, slot) in slots.iter_mut().enumerate() {
            if slot.is_some() {
                continue;
            }
            let outcome = match (&forced[index], &batch.stopped) {
                (Some(reason), _) => ToolOutcome::stopped(reason),
                (None, Some(reason)) => ToolOutcome::skipped(reason),
                (None, None) => ToolOutcome::error(format!(
                    "tool task for call {} did not settle (panicked or cancelled)",
                    index + 1
                )),
            };
            *slot = Some((outcome, Duration::ZERO));
        }
        // Every call of the batch has settled: none is a `stop --call` target
        // any more, and the next batch repopulates the list.
        self.live_calls.clear();

        // Calls that became background jobs, recorded after the whole batch.
        let mut promoted: Vec<String> = Vec::new();
        for (index, ((dir, call), slot)) in plan.iter().zip(slots).enumerate() {
            let (mut outcome, elapsed) = slot.unwrap_or_else(|| {
                (
                    ToolOutcome::error("the call never reported a result"),
                    Duration::ZERO,
                )
            });
            if let Some(reason) = &forced[index] {
                // The call was stopped: keep the partial output the model can
                // still use, and say where the whole of it is (M1 review N2).
                outcome = self.stopped_outcome(dir, reason);
            }
            self.record(EventBody::ToolSettled {
                call_id: call.id.clone(),
                outcome: outcome.outcome.clone(),
                exit_code: outcome.exit_code,
                bytes: outcome.bytes,
                lines: outcome.lines,
                out_path: outcome.out_path.clone(),
                spilled: outcome.spilled,
                result: outcome.result.clone().into(),
            })?;
            // A promoted call became a background job at this instant: a reader
            // of the log should see the job, not have to infer it from the
            // `promoted` outcome. The status file the shim already wrote is
            // the truth (`bg_started`, and the wake message if it settled
            // while the model was still reading this result). It is recorded
            // after the whole batch's results, never between them: a wake
            // message is a `user` event, and a call's results must stay
            // contiguous (M4 review N5).
            if outcome.outcome == "promoted" {
                promoted.push(dir.clone());
            }
            // The TUI renders its own line from `UiEvent::ToolSettled`; only the
            // headless path prints here, so the painter is the only writer.
            if !self.json && !self.tui {
                println!(
                    "→ {} {} · {} · {} B · {:.1}s",
                    call.name,
                    compact_input(&call.args),
                    outcome.outcome,
                    outcome.bytes,
                    elapsed.as_secs_f64()
                );
            }
            // A failed call's tail row: the last line it printed, so the
            // transcript says *why* without unfolding anything.
            let tail = if outcome.outcome == "ok" && outcome.exit_code.unwrap_or(0) == 0 {
                String::new()
            } else {
                call_tail(&outcome)
                    .or_else(|| last_result_line(&outcome.result))
                    .unwrap_or_default()
            };
            self.emit(UiEvent::ToolSettled {
                name: call.name.clone(),
                input: call.args.clone(),
                outcome: outcome.outcome.clone(),
                exit_code: outcome.exit_code,
                elapsed_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
                bytes: outcome.bytes,
                lines: outcome.lines,
                spilled: outcome.spilled,
                out_path: outcome.out_path.clone(),
                tail,
            });
        }
        self.sync();
        // Now that the batch's results are all in the log, a promoted call's
        // job can be recorded (`bg_started`, and the wake message if it settled
        // while the model was reading the result).
        for dir in promoted {
            if let Some(status) =
                crate::bg::Status::read(&crate::bg::status_path(&self.dir.join("bg"), &dir))
            {
                let _ = self.note_bg(&status);
            }
        }
        // Documented test hook: die with the batch committed, which is where a
        // crash after `tool_settled` lands.
        crash_at("tool_settled");
        Ok(batch)
    }

    /// One call is done: it is no longer a target for `stop --call`.
    fn settle_call(&mut self, call_id: &str) {
        self.live_calls.retain(|id| id != call_id);
    }

    /// The `stopped` result of a call that had already written output: the
    /// byte and line counts, the file, and the last 1 KB, so the model can pick
    /// up the partial work (M1 review N2).
    fn stopped_outcome(&self, dir: &str, reason: &str) -> ToolOutcome {
        let out_path = self.ctx.call_dir(dir).join("out");
        match crate::spill::partial(&out_path) {
            Ok(partial) if partial.bytes > 0 => ToolOutcome::stopped_with_output(
                reason,
                &crate::util::path_string(&out_path),
                partial.bytes,
                partial.lines,
                &partial.tail,
            ),
            _ => ToolOutcome::stopped(reason),
        }
    }

    /// Kill every call that has a process, and gate the ones that do not.
    /// Calls already settled keep their real result.
    fn kill_batch(
        &self,
        plan: &[(String, ToolCall)],
        slots: &[Option<(ToolOutcome, Duration)>],
        gates: &[Arc<AtomicBool>],
        forced: &mut [Option<String>],
        deadlines: &mut [Option<Instant>],
        reason: &str,
    ) {
        let running = self.ctx.running.keys();
        for (index, (dir, _)) in plan.iter().enumerate() {
            gates[index].store(true, Ordering::SeqCst);
            if slots[index].is_some() || forced[index].is_some() {
                continue;
            }
            if running.iter().any(|key| key == dir) {
                self.ctx.running.kill(dir);
                forced[index] = Some(reason.to_string());
            }
            // Every unsettled call gets its own window, whether or not it had
            // a process yet: one that starts just after the gate closed is
            // taken down by the poll, and a task that never reports back must
            // not hold the batch open.
            deadlines[index] = Some(Instant::now() + STOP_GRACE);
        }
    }

    /// Take down calls that registered a process after the kill.
    fn retake(
        &self,
        plan: &[(String, ToolCall)],
        slots: &[Option<(ToolOutcome, Duration)>],
        deadlines: &[Option<Instant>],
        forced: &mut [Option<String>],
        reason: &str,
    ) {
        let running = self.ctx.running.keys();
        for (index, (dir, _)) in plan.iter().enumerate() {
            if slots[index].is_some() || deadlines[index].is_none() {
                continue;
            }
            if running.iter().any(|key| key == dir) {
                self.ctx.running.kill(dir);
                if forced[index].is_none() {
                    forced[index] = Some(reason.to_string());
                }
            }
        }
    }
}

/// `provider/model` splits only when the prefix is a configured provider:
/// `deepseek/deepseek-chat` is a model id on the current provider, while
/// `openrouter/deepseek/deepseek-chat` names a provider.
fn split_model_target<'a>(
    target: &'a str,
    providers: &'a std::collections::BTreeMap<String, crate::config::ProviderConfig>,
) -> (Option<&'a str>, &'a str) {
    match target.split_once('/') {
        Some((prefix, rest)) if providers.contains_key(prefix) && !rest.is_empty() => {
            (Some(prefix), rest)
        }
        _ => (None, target),
    }
}

fn print_result_event(
    outcome: &str,
    exit_code: u8,
    final_text: &str,
    steps: usize,
    session: Option<(&str, &Path)>,
    error: Option<&str>,
) {
    let mut value = json!({
        "type": "result",
        "outcome": outcome,
        "exit_code": exit_code,
        "final_text": final_text,
        "steps": steps,
        "kelir_version": VERSION,
    });
    if let Some((id, dir)) = session {
        value["session"] = json!(id);
        value["session_dir"] = json!(crate::util::path_string(dir));
    }
    if let Some(message) = error {
        value["error"] = json!(message);
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{value}");
    let _ = lock.flush();
}

fn compact_input(args: &Value) -> String {
    let text = args
        .get("command")
        .and_then(|value| value.as_str())
        .or_else(|| args.get("path").and_then(|value| value.as_str()))
        .map(str::to_string)
        .unwrap_or_else(|| args.to_string());
    crate::util::truncate_chars(&text.replace('\n', " ↵ "), 60)
}

/// The last non-empty line of a call's output file: a failed call's tail row.
/// Reads only the end of the file, so a 200 MB log stays cheap.
fn call_tail(outcome: &ToolOutcome) -> Option<String> {
    if outcome.out_path.is_empty() {
        return None;
    }
    let text = crate::tui::read_tail(std::path::Path::new(&outcome.out_path), 4096).ok()?;
    let line = crate::tui::last_line(&text);
    (!line.is_empty()).then_some(line)
}

/// The last non-empty line of a result that never reached a file (a stopped or
/// disabled call's message).
fn last_result_line(result: &str) -> Option<String> {
    let line = crate::tui::last_line(result);
    (!line.is_empty()).then_some(line)
}

/// One line for the UI: newlines folded, length bounded.
fn one_line(text: &str) -> String {
    crate::util::truncate_chars(&text.replace('\n', " ↵ "), 200)
}

fn new_session_id() -> String {
    format!("ses_{}", ulid::Ulid::new().to_string().to_lowercase())
}

/// Render the system prompt once, from the config and the cwd's sources.
///
/// The skills directories come from the config (`skills_dirs`) plus the
/// conventional ones; the base prompt is the config's, the machine
/// `prompt.md`, or the embedded default (the project's `.kelir/prompt.md` wins
/// inside [`crate::sources::render_system`]).
fn render_system(cfg: &Config, cwd: &Path) -> crate::sources::Rendered {
    let places = crate::sources::places(cwd, &cfg.skills_dirs, Some(crate::paths::config_dir()));
    let date = today();
    crate::sources::render_system(&crate::sources::Inputs {
        base: &cfg.system_prompt,
        base_path: cfg.prompt_path.as_deref(),
        places: &places,
        cwd,
        date: &date,
    })
}

/// The date goes in, never the time: the system prefix must be stable for the
/// life of a session (and across a resume).
fn today() -> String {
    time::OffsetDateTime::now_utc().date().to_string()
}

async fn task_text(parts: &[String]) -> Result<String, String> {
    if !parts.is_empty() {
        return Ok(parts.join(" "));
    }
    let mut buffer = String::new();
    tokio::io::stdin()
        .read_to_string(&mut buffer)
        .await
        .map_err(|error| format!("cannot read the task from stdin: {error}"))?;
    let trimmed = buffer.trim().to_string();
    if trimmed.is_empty() {
        return Err("no task: pass it as an argument or on stdin".to_string());
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_are_prefixed_lowercase_ulids() {
        let id = new_session_id();
        assert!(id.starts_with("ses_"), "{id}");
        let body = &id[4..];
        assert_eq!(body.len(), 26);
        assert!(
            body.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
    }

    #[test]
    fn a_pause_says_what_happened_and_how_to_go_on() {
        let report = PauseReport {
            reason: PauseReason::Provider,
            attempts: 7,
            waited: Duration::from_secs(30 * 60),
            detail: "rate limited (429)".into(),
        };
        assert_eq!(
            report.message(),
            "provider unavailable after 7 attempts, 30 min: rate limited (429); say continue to retry"
        );
        let one = PauseReport {
            reason: PauseReason::Provider,
            attempts: 1,
            waited: Duration::from_millis(400),
            detail: "provider error (500)".into(),
        };
        assert_eq!(
            one.message(),
            "provider unavailable after 1 attempt, 400 ms: provider error (500); say continue to retry"
        );
    }

    #[test]
    fn a_step_request_is_rebuilt_after_a_wait() {
        // `RequestFor::Step` builds from the session's *current* model: that is
        // what makes a `model` verb during a wait apply to the retry.
        let request = RequestFor::Step.build(&test_session("first/model"));
        assert_eq!(request.body["model"], "first/model");
        let fixed = provider::build_request(
            &test_provider(),
            "ses_1",
            "pinned/model",
            &[],
            &[],
            None,
            false,
        );
        assert_eq!(
            RequestFor::Built(&fixed)
                .build(&test_session("second/model"))
                .body["model"],
            "pinned/model",
            "a compaction request is built once, from the model it was made with"
        );
    }

    fn test_provider() -> crate::config::ProviderConfig {
        crate::config::ProviderConfig {
            kind: crate::config::ProviderKind::OpenRouter,
            base_url: "https://example.invalid/v1".into(),
            key_env: None,
            key: Some("test".into()),
            key_cmd: None,
            key_origin: None,
            key_expires_at_ms: None,
            extra_headers: Default::default(),
            reasoning_field: None,
            reasoning_effort: None,
            reasoning_effort_explicit: false,
            session_header: None,
            max_concurrent_requests: None,
        }
    }

    /// A session with nothing but a model set: enough for a request to be built
    /// from the empty fold.
    fn test_session(model: &str) -> Session {
        let dir = std::env::temp_dir().join(format!("kelir-session-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let writer = EventLog::open(&dir.join("events.jsonl")).unwrap();
        Session {
            id: "ses_1".into(),
            dir: dir.clone(),
            writer,
            blobs: Blobs::new(&dir),
            events: Vec::new(),
            cfg: Config {
                provider: String::new(),
                model: String::new(),
                shell: vec![],
                spill_limit: 0,
                max_steps: None,
                cache_ttl: None,
                system_prompt: crate::config::DEFAULT_PROMPT.to_string(),
                prompt_path: None,
                providers: Default::default(),
                window: None,
                model_windows: Default::default(),
                auto_compact_at: 0.8,
                compact_at_tokens: 0,
                compact_prompt: String::new(),
                skills_dirs: vec![],
                compact_keep: 0,
                promote_after: std::time::Duration::from_secs(0),
                retry: crate::retry::Policy::default(),
            },
            provider_name: "openrouter".into(),
            model: model.into(),
            client: Client::new(test_provider(), "ses_1", None).unwrap(),
            active_provider: test_provider(),
            ctx: Ctx::new(dir, std::env::temp_dir(), vec![], 0, String::new()),
            json: true,
            echo_assistant: false,
            keep: false,
            tui: false,
            ui: None,
            herdr: None,
            has_task: false,
            final_text: String::new(),
            step: 0,
            turn: 0,
            turn_steps: 0,
            disabled: BTreeSet::new(),
            refused_fields: BTreeSet::new(),
            live_calls: Vec::new(),
            scheduler: Scheduler::default(),
            compact_request: None,
            last_prompt_tokens: None,
            compact_floor: None,
            tool_names: tools::names(),
            resumed: false,
            in_flight: false,
            retryable_failure: false,
            pending_task: String::new(),
            window: 1_000,
            window_source: "default",
            window_model: Some(model.to_string()),
            window_note: None,
            models: None,
            models_fetch: None,
            watch: crate::sources::Watch::default(),
            governor: None,
            // A throwaway trust state in the same temp dir: no socket here, and
            // nothing in these tests sends a `bg` or `cancel` verb.
            trust: std::sync::Arc::new(crate::trust::Trust::new(dir_for_trust(), String::new())),
            refused_status: std::collections::HashSet::new(),
            seen_seq: std::collections::HashMap::new(),
        }
    }

    fn dir_for_trust() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kelir-trust-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn system_prompt_has_a_date_and_no_time() {
        let system = crate::sources::render_system(&crate::sources::Inputs {
            base: "base",
            base_path: None,
            places: &crate::sources::Places::default(),
            cwd: Path::new("work"),
            date: "2026-11-19",
        });
        let text = system.text;
        assert!(text.starts_with("base\n"), "{text}");
        assert!(text.contains("cwd: work"), "{text}");
        let date_line = text.lines().find(|line| line.starts_with("date:")).unwrap();
        // `date: YYYY-MM-DD`: the only colon is the label's.
        assert_eq!(date_line.chars().filter(|c| *c == ':').count(), 1);
        assert_eq!(date_line.len(), "date: 2026-11-18".len());
    }

    #[test]
    fn system_prompt_is_stable_within_a_run() {
        let cfg = Config {
            provider: String::new(),
            model: String::new(),
            shell: vec![],
            spill_limit: 0,
            max_steps: None,
            cache_ttl: None,
            system_prompt: crate::config::DEFAULT_PROMPT.to_string(),
            prompt_path: None,
            providers: Default::default(),
            window: None,
            model_windows: Default::default(),
            auto_compact_at: 0.8,
            compact_at_tokens: crate::compact::DEFAULT_SOFT_TARGET,
            compact_prompt: String::new(),
            skills_dirs: vec![],
            compact_keep: 0,
            promote_after: std::time::Duration::from_secs(0),
            retry: crate::retry::Policy::default(),
        };
        let a = render_system(&cfg, Path::new("tmp"));
        let b = render_system(&cfg, Path::new("tmp"));
        assert_eq!(a, b);
    }

    #[test]
    fn compact_input_prefers_the_command() {
        assert_eq!(compact_input(&json!({"command": "ls -la"})), "ls -la");
        assert_eq!(compact_input(&json!({"path": "src/a.rs"})), "src/a.rs");
        assert_eq!(compact_input(&json!({"x": 1})), "{\"x\":1}");
    }

    #[test]
    fn a_model_target_splits_only_on_a_known_provider() {
        let mut providers = std::collections::BTreeMap::new();
        for name in ["openrouter", "opencode-go"] {
            providers.insert(
                name.to_string(),
                crate::config::ProviderConfig {
                    kind: crate::config::ProviderKind::OpenRouter,
                    base_url: "http://x/v1".into(),
                    key_env: None,
                    key: None,
                    key_cmd: None,
                    key_origin: None,
                    key_expires_at_ms: None,
                    extra_headers: Default::default(),
                    reasoning_field: None,
                    reasoning_effort: None,
                    reasoning_effort_explicit: false,
                    session_header: None,
                    max_concurrent_requests: None,
                },
            );
        }
        assert_eq!(
            split_model_target("openrouter/deepseek/deepseek-chat", &providers),
            (Some("openrouter"), "deepseek/deepseek-chat")
        );
        assert_eq!(
            split_model_target("opencode-go/glm-5", &providers),
            (Some("opencode-go"), "glm-5")
        );
        // A model id whose first segment is not a provider stays whole.
        assert_eq!(
            split_model_target("deepseek/deepseek-chat", &providers),
            (None, "deepseek/deepseek-chat")
        );
        assert_eq!(
            split_model_target("no-slash-model", &providers),
            (None, "no-slash-model")
        );
        // `provider/` with nothing after it is a model name, not a swap.
        assert_eq!(
            split_model_target("openrouter/", &providers),
            (None, "openrouter/")
        );
    }

    /// F-window: the banner is silent about the window the provider answered,
    /// and says one line when the start fell back — with the reason a person
    /// needs (a 401, an offline endpoint) — because that fallback changes the
    /// compaction threshold.
    #[test]
    fn the_banner_names_a_window_only_when_it_is_not_the_providers() {
        let mut session = test_session("m");
        // The provider's own answer: no line at all.
        session.window = 262_144;
        session.window_source = "api";
        session.window_note = None;
        assert_eq!(window_banner_line(&session), None);
        // A configured number is the person's own: no apology either.
        session.window_source = "config";
        assert_eq!(window_banner_line(&session), None);
        // A stale cache has numbers that are real, and `state` carries the why.
        session.window_source = "cache";
        session.window = 100_000;
        assert_eq!(window_banner_line(&session), None);
        // The fallback: the number and the reason, on one line.
        session.window_source = "default";
        session.window = crate::compact::DEFAULT_WINDOW;
        session.window_note = Some("the model list: offline".into());
        let line = window_banner_line(&session).expect("a fallback is announced");
        assert!(line.contains("window 128000 tokens (default)"), "{line}");
        assert!(line.contains("offline"), "{line}");
        // A fallback with nothing to say about why still says it fell back.
        session.window_note = None;
        let line = window_banner_line(&session).expect("a fallback is announced");
        assert!(line.contains("window 128000 tokens (default)"), "{line}");
        assert!(line.contains("did not answer"), "{line}");
    }

    /// F-window (addendum): a `/model` swap after a fresh-cache startup still
    /// announces a changed window. `window_model` is seeded from the startup
    /// answer, so the answer that lands after the swap knows it is a different
    /// model — the case a startup with a fresh list used to lose silently.
    #[test]
    fn a_window_that_changes_after_a_swap_is_announced_once() {
        let mut session = test_session("first/model");
        assert_eq!(
            session.window_model.as_deref(),
            Some("first/model"),
            "the startup answer names its model"
        );
        // The swap itself is the first announcement, from `swap_model`: this
        // test only has the constructor's seeding to check, so it drives
        // `apply_list` the way a landed fetch does after a swap.
        session.model = "second/model".into();
        session.window = 1_000;
        session.window_source = "default";
        let (tx, mut rx) = mpsc::unbounded_channel();
        session.ui = Some(tx);
        let mut cache = crate::models::Cache::new(
            "openrouter",
            "https://example.invalid/v1",
            vec![crate::models::ModelInfo {
                id: "second/model".into(),
                context: Some(262_144),
                tools: None,
                vision: None,
                parallel_calls: None,
                reasoning_effort: None,
            }],
            0,
        );
        cache.fetched_at_ms = crate::models::now_ms();
        session.apply_list(ModelList {
            cache: Some(cache),
            fetched: true,
            why: None,
        });
        let announced: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|event| match event {
                UiEvent::Info(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(announced.len(), 1, "one line, once: {announced:?}");
        assert!(
            announced[0].contains("262144 tokens (api)"),
            "{announced:?}"
        );
        assert_eq!(session.window, 262_144);
        assert_eq!(session.window_source, "api");

        // A second answer for the same model is silent: the number did not move
        // for a new model, so there is nothing new to say.
        session.apply_list(ModelList {
            cache: None,
            fetched: true,
            why: None,
        });
        assert!(
            rx.try_recv().is_err(),
            "the same model's answer is not announced twice"
        );
    }
}
