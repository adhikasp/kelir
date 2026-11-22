//! Session-local types and their small helpers.
// These private loop modules expose crate-visible helpers to the feature modules.
#![allow(clippy::redundant_pub_crate)]

use super::*;

pub(crate) struct Options {
    pub(crate) json: bool,
    pub(crate) ask: bool,
    pub(crate) keep: bool,
    /// Interactive: an inline viewport owns the terminal and every turn is
    /// rendered through `tui::View`s instead of stdout.
    pub(crate) tui: bool,
    pub(crate) common: CommonArgs,
    /// The task words from the command line (`run`, `ask`); empty for the TUI.
    pub(crate) task: Vec<String>,
    /// Continue this session (id or unique prefix) from its log.
    pub(crate) resume: Option<String>,
    /// A bound name's session id for a fresh run; otherwise a new id is minted.
    pub(crate) id: Option<String>,
    /// This process is a child session: who started it, where the root is.
    pub(crate) child: Option<crate::child::Handoff>,
    /// A scheduled run's explicit options and pre-task sources, or `None` for
    /// an ordinary interactive/CLI run.
    pub(crate) run_options: Option<(crate::namedrun::RunOptions, Vec<(String, EventBody)>)>,
}

pub(crate) enum Failure {
    /// Exit 2: bad invocation or configuration.
    Usage(String),
    /// Exit 1: the run itself failed.
    Run(String),
}

impl Failure {
    pub(crate) fn code(&self) -> u8 {
        match self {
            Failure::Usage(_) => 2,
            Failure::Run(_) => 1,
        }
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Failure::Usage(message) | Failure::Run(message) => message,
        }
    }
}

pub(crate) struct Prepared {
    pub(crate) cfg: Config,
    pub(crate) provider_name: String,
    pub(crate) model: String,
    pub(crate) client: Client,
    /// The provider config the client was built from, its key included: the
    /// session keeps it as `active_provider`.
    pub(crate) provider_cfg: crate::config::ProviderConfig,
    /// Where this session's key came from (`env OPENROUTER_API_KEY`,
    /// `opencode auth export opencode`). Recorded in `session_started`.
    pub(crate) key_origin: Option<String>,
    pub(crate) id: String,
    pub(crate) dir: PathBuf,
    pub(crate) task: String,
    pub(crate) cwd: PathBuf,
    /// The file named by `--config`, if any: MCP's broker must read the same
    /// file the rest of the session was built from.
    pub(crate) config_path: Option<PathBuf>,
    /// The frozen system prompt: the text, its hash, and the sources that went
    /// in (`session_started.system` and `sources[]`).
    pub(crate) system: crate::sources::Rendered,
    pub(crate) sock: String,
    pub(crate) json: bool,
    pub(crate) ask: bool,
    pub(crate) keep: bool,
    pub(crate) tui: bool,
    pub(crate) herdr: Option<Herdr>,
    pub(crate) resume: Option<Reopened>,
    /// Sources a scheduled run passes in explicitly.
    pub(crate) run_sources: Vec<EventBody>,
    pub(crate) task_from: &'static str,
    pub(crate) step_cap: Option<u32>,
    /// Set when this process is a child session.
    pub(crate) child: Option<crate::child::Handoff>,
    /// The window compaction measures against, and where it came from
    /// (`config`, `api`, `cache`, `default`).
    pub(crate) window: u64,
    pub(crate) window_source: &'static str,
    /// Why the window is what it is, when that needs saying: the short reason the provider's list
    /// did not answer, in words. `None` for a configured window and for a list that answered.
    pub(crate) window_note: Option<String>,
    /// The provider's model list as this process learned it, so a `model` swap
    /// resolves without the network.
    pub(crate) models: Option<crate::models::Cache>,
    /// A metadata fetch still running (or its answer, waiting to be taken): resolved at the next
    /// `before_step`, so a slow list never holds the first request.
    pub(crate) models_fetch: Option<Fetch>,
    /// The opt-in concurrency governor for the provider this session starts with
    /// (`max_concurrent_requests`); `None` when the provider config has no limit.
    pub(crate) governor: Option<crate::governor::Governor>,
}

/// What a resume reads out of the log before the session continues it.
pub(crate) struct Reopened {
    pub(crate) events: Vec<LogEvent>,
    /// The log was not closed cleanly (a turn was running, or the process was
    /// killed between turns): resume continues from the model's next step.
    pub(crate) in_flight: bool,
    /// Calls the log shows as requested but never settled.
    pub(crate) unsettled: Vec<UnsettledCall>,
    /// Torn or unreadable log lines, reported as `error` events.
    pub(crate) damage: Vec<String>,
    pub(crate) step: u32,
    pub(crate) turns: u32,
    /// The tool names frozen at `session_started`, so a resumed session sends
    /// the same tool list (and keeps its cache prefix).
    pub(crate) tools: Vec<String>,
    /// The frozen system text; kept so the log is never rewritten.
    pub(crate) system_text: String,
    /// The sources the frozen system was rendered from, as `session_started`
    /// recorded them: the watcher re-checks exactly these.
    pub(crate) sources: Vec<crate::event::SourceInfo>,
    pub(crate) cwd: PathBuf,
}

/// The log and its identity: the one writer, its blobs, the folded events and
/// whether the session was reopened and already named.
pub(crate) struct Log {
    pub(crate) writer: crate::event::EventLog,
    pub(crate) blobs: Blobs,
    pub(crate) events: Vec<LogEvent>,
    pub(crate) id: String,
    pub(crate) dir: PathBuf,
    pub(crate) config_dir: PathBuf,
    pub(crate) titled: bool,
}

/// The provider side of a session: the client, the active config and the
/// provider-fields a 400 has taught us to drop.
pub(crate) struct Provider {
    pub(crate) client: Client,
    pub(crate) active_provider: crate::config::ProviderConfig,
    pub(crate) provider_name: String,
    pub(crate) model: String,
    pub(crate) refused_fields: BTreeSet<String>,
    pub(crate) governor: Option<crate::governor::Governor>,
}

/// Where a session's output goes: stdout shapes, the UI sink and the Herdr status.
pub(crate) struct Out {
    pub(crate) json: bool,
    pub(crate) echo_assistant: bool,
    pub(crate) keep: bool,
    pub(crate) tui: bool,
    pub(crate) ui: Option<mpsc::UnboundedSender<crate::tui::View>>,
    pub(crate) herdr: Option<Herdr>,
}

/// The compaction request and the context-size bookkeeping behind it.
pub(crate) struct CompactState {
    pub(crate) request: Option<CompactRequest>,
    pub(crate) last_prompt_tokens: Option<u64>,
    pub(crate) floor: Option<u64>,
}

/// The run as a whole: the task, its delivery and the resume/in-flight flags.
pub(crate) struct Run {
    /// Session-wide step counter, turn number and steps taken in this turn.
    pub(crate) step: u32,
    pub(crate) turn: u32,
    pub(crate) turn_steps: u32,
    pub(crate) has_task: bool,
    pub(crate) final_text: String,
    pub(crate) run_sources: Vec<EventBody>,
    pub(crate) task_from: &'static str,
    pub(crate) step_cap: Option<u32>,
    pub(crate) resumed: bool,
    pub(crate) in_flight: bool,
    pub(crate) retryable_failure: bool,
    pub(crate) pending_task: String,
}

/// The tools frozen at session start and the calls live in this batch.
pub(crate) struct ToolState {
    pub(crate) names: Vec<String>,
    pub(crate) disabled: BTreeSet<String>,
    pub(crate) live_calls: Vec<String>,
}

pub(crate) struct Session {
    /// The log, the folded events and the identity they belong to.
    pub(crate) log: Log,
    pub(crate) cfg: Config,
    pub(crate) provider: Provider,
    /// The frozen tool context of skills and sources.
    pub(crate) ctx: Ctx,
    pub(crate) out: Out,
    /// The loop's own inbox sender: broker tasks, children and bg jobs hand
    /// finished work back through it.
    pub(crate) inbox: Option<mpsc::UnboundedSender<Incoming>>,
    pub(crate) scheduler: Scheduler,
    pub(crate) compact: CompactState,
    pub(crate) tools: ToolState,
    pub(crate) run: Run,
    /// Every feature's state, dispatched through the `Feature` boundaries.
    pub(crate) features: crate::features::Features,
    /// This session's socket trust boundary: its token and the jobs it owns.
    pub(crate) trust: std::sync::Arc<crate::trust::Trust>,
    /// What this session has spent, folded from the log as it is written.
    pub(crate) tally: crate::usage::Tally,
    /// This session's place in the family of sessions and its registry of children.
    pub(crate) kids: crate::features::children::Kids,
    /// The retry or slot wait in progress, for `status`.
    pub(crate) retry_wait: std::cell::Cell<Option<RetryWait>>,
}

/// A retry or slot wait in progress: ends at, status, attempt, is-a-slot-wait.
type RetryWait = (Instant, Option<u16>, u32, bool);

pub(crate) struct CompactRequest {
    pub(crate) by: &'static str,
    pub(crate) focus: Option<String>,
    pub(crate) immediate: bool,
}

pub(crate) struct CompactedInfo {
    pub(crate) through_seq: u64,
    pub(crate) tokens_before: u64,
    pub(crate) tokens_after: u64,
    pub(crate) pinned: usize,
}

pub(crate) enum CompactRun {
    /// Nothing old enough to summarise.
    Nothing,
    /// A `compacted` event was written.
    Applied(CompactedInfo),
    /// The context would not shrink, so nothing was written ; `would_be` is
    /// the size the fold would have had.
    Skipped { before: u64, would_be: u64 },
    /// A verb interrupted the summarising request.
    Stopped(String),
    /// The `end` verb arrived during the summarising request.
    End,
}

/// The result the `compact` tool call itself gets: the work happens at the next
/// boundary, not inside the call.
pub(crate) fn compact_queued(focus: &Option<String>) -> String {
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
/// (`KELIR_TEST_CRASH_AT=<point>`) is a no-op unless it names this point exactly.
pub(crate) fn crash_armed(point: &str) -> bool {
    crate::envs::var_os("KELIR_TEST_CRASH_AT")
        .map(|value| value.to_string_lossy().trim() == point)
        .unwrap_or(false)
}

/// Die the way a crash does: abort, no unwinding, no `session_ended`.
pub(crate) fn crash_now(point: &str) -> ! {
    let _ = writeln!(
        std::io::stderr(),
        "kelir: KELIR_TEST_CRASH_AT={point}: aborting (test hook)"
    );
    let _ = std::io::stderr().flush();
    std::process::abort()
}

pub(crate) fn crash_at(point: &str) {
    if crash_armed(point) {
        crash_now(point);
    }
}

pub(crate) enum TurnEnd {
    Done,
    Stopped(String),
    /// The turn paused and waits for a human. Not a failure: the session stays alive (`--keep`)
    /// and the next message opens a new turn.
    Paused(PauseReport),
    /// The `end` verb arrived.
    End,
}

pub(crate) enum TurnStart {
    Messages(Vec<Message>),
    End,
}

pub(crate) enum Idle {
    Turn,
    End,
}

/// A `user` event to inject at a boundary.
pub(crate) struct Message {
    pub(crate) text: String,
    pub(crate) delivery: &'static str,
    pub(crate) from: String,
}

#[derive(Default)]
pub(crate) struct Applied {
    /// The `end` verb was applied.
    pub(crate) end: bool,
    /// The `stop` verb ended the turn, with this reason.
    pub(crate) stopped: Option<String>,
    /// How many messages were injected (a `say` means another step).
    pub(crate) messages: usize,
}

pub(crate) enum StreamEnd {
    Assembled(crate::stream::Assembler),
    Stopped(String),
    End,
    /// The provider stayed unavailable past the request's waiting budget: the turn pauses (it
    /// does not fail) and the next `say` retries from the same fold.
    Paused(PauseReport),
}

/// What a pause says happened, for `turn_ended.final_text` (and its `reason`).
pub(crate) struct PauseReport {
    /// `paused` because of this: an explicit `max_steps` cap (`steps`) or an
    /// unavailable provider (`provider`).
    pub(crate) reason: PauseReason,
    /// Provider requests made inside the budget (`provider` only).
    pub(crate) attempts: u32,
    /// How long the waits inside the budget took (`provider` only).
    pub(crate) waited: Duration,
    /// The last failure's short reason ("rate limited (429)"), `provider` only.
    pub(crate) detail: String,
}

/// The two ways a turn pauses; both log `outcome: "paused"`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PauseReason {
    /// An explicit `max_steps` cap was reached.
    Steps,
    /// The provider stayed unavailable past the waiting budget.
    Provider,
}

impl PauseReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PauseReason::Steps => "steps",
            PauseReason::Provider => "provider",
        }
    }
}

impl PauseReport {
    /// The step cap: "turn paused after 40 steps; say continue to resume".
    pub(crate) fn steps(steps: u32) -> PauseReport {
        PauseReport {
            reason: PauseReason::Steps,
            attempts: steps,
            waited: Duration::ZERO,
            detail: String::new(),
        }
    }

    /// "provider unavailable after 7 attempts / 30 min: rate limited (429);
    /// say continue to retry".
    pub(crate) fn message(&self) -> String {
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

/// Where a request comes from, so it can be rebuilt after a wait: a `model` verb during the
/// wait changes the model (and the provider), and the next attempt has to use it.
pub(crate) enum RequestFor<'a> {
    /// A step: the fold, built from the session's current model.
    Step,
    /// The compaction's own request: it is built once, from the model it was
    /// made with.
    Built(&'a provider::BuiltRequest),
}

impl RequestFor<'_> {
    pub(crate) fn build(&self, session: &Session) -> provider::BuiltRequest {
        match self {
            RequestFor::Built(request) => (*request).clone(),
            RequestFor::Step => {
                // The fold cannot fail here for a reason a rebuild would fix;
                // an unreadable blob is the parser's error to report.
                let messages = crate::fold::fold(&session.log.events, &session.log.dir)
                    .map(|folded| folded.request_messages())
                    .unwrap_or_default();
                // The same builder `drive_turn` used for the first attempt, so a retry keeps's capability decisions
                // (`parallel_calls`, the `reasoning_effort` gate and the fields this session already saw a 400 for) as
                session.step_request(&messages)
            }
        }
    }
}

/// What [`Session::accept_at_once`] did with a verb.
pub(crate) enum AtOnce {
    /// Accepted and applied now; the turn may have ended because of it.
    Applied(Option<Box<StreamEnd>>),
    /// Refused (the client has its error); nothing was applied, so nothing
    /// about the running turn changed.
    Rejected,
}

/// Does this request ask for a verb that is applied the moment it arrives, and so is answered
/// only after it landed? `say --now` and `model`.
pub(crate) fn at_once_verb(request: &Request) -> bool {
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

pub(crate) enum Attempt {
    Ok(crate::stream::Assembler),
    Failed(provider::ProviderError),
    /// A verb ended the turn while the request was in flight.
    End(StreamEnd),
}

/// What a retry or slot wait ended as.
pub(crate) enum Waited {
    /// The wait ran out: try again.
    Timeout,
    /// A verb applied at once ends the wait and the attempt is re-sent right away (`say --now`,
    /// `model`): the request is a new one, so it is rebuilt from the fold the verb just changed.
    Retry,
    /// A verb (or a signal) ended the turn.
    End(Box<StreamEnd>),
}

/// What waiting for a governor slot ended as: a slot (or none, when the
/// governor is off), or a verb ending the turn.
pub(crate) enum SlotWait {
    Taken(Option<crate::governor::Slot>),
    Interrupted(StreamEnd),
}

#[derive(Default)]
pub(crate) struct Batch {
    pub(crate) stopped: Option<String>,
    pub(crate) end: bool,
}
