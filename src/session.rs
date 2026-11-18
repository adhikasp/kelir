//! The loop: fold → request → stream → log → tools → boundaries → repeat.
//!
//! One session is one process and one append-only log. Every verb that arrives
//! on the session socket is logged and then applied at exactly one
//! [`Boundary`], never between a tool call and its result.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::AsyncReadExt as _;
use tokio::sync::mpsc;

use crate::VERSION;
use crate::chat::ToolCall;
use crate::cli::{AskArgs, CommonArgs, RunArgs};
use crate::config::{self, Config};
use crate::event::{
    Blobs, EventBody, EventLog, LogEvent, LoggedToolCall, Payload, SystemInfo, spill_payloads,
};
use crate::ipc::{self, Incoming, Response};
use crate::provider::{self, Client};
use crate::tools::{self, Ctx, ToolOutcome};
use crate::verbs::{self, Boundary, Pending, Scheduler, Verb};

/// How long a stopped call may take to report back before the batch settles it
/// on the user's behalf. The kill is immediate; this is for the process tree to
/// unwind. The `say --now` requirement is "within 2 s".
const STOP_GRACE: Duration = Duration::from_millis(1500);
/// While a stop is settling, look for calls that started just after the kill
/// and take them down too.
const STOP_POLL: Duration = Duration::from_millis(25);

type Inbox = mpsc::UnboundedReceiver<Incoming>;

pub async fn run(args: RunArgs) -> u8 {
    drive(Options {
        json: args.json,
        ask: false,
        keep: args.keep,
        common: args.common,
    })
    .await
}

pub async fn ask(args: AskArgs) -> u8 {
    drive(Options {
        json: false,
        ask: true,
        keep: false,
        common: args.common,
    })
    .await
}

struct Options {
    json: bool,
    ask: bool,
    keep: bool,
    common: CommonArgs,
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
    id: String,
    dir: PathBuf,
    task: String,
    cwd: PathBuf,
    system_text: String,
    sock: String,
    json: bool,
    ask: bool,
    keep: bool,
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
    ctx: Ctx,
    json: bool,
    echo_assistant: bool,
    keep: bool,
    final_text: String,
    /// Session-wide step counter: every `step_started` in a `--keep` session
    /// keeps counting, so step numbers stay unique in one log.
    step: u32,
    /// The turn being run (0 before the first one).
    turn: u32,
    /// Tools turned off for the rest of the session. The list sent to the
    /// provider never changes.
    disabled: BTreeSet<String>,
    /// The provider call ids in the batch that is running right now.
    live_calls: Vec<String>,
    /// Verbs waiting for their boundary.
    scheduler: Scheduler,
}

/// What a turn ended as.
enum TurnEnd {
    Done,
    Stopped(String),
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
}

/// What a tool batch ended as.
#[derive(Default)]
struct Batch {
    stopped: Option<String>,
    end: bool,
}

async fn drive(opts: Options) -> u8 {
    let prepared = match prepare(&opts).await {
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

    let mut session = match Session::start(prepared) {
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
    ipc::serve(listener, tx);

    let code = session.run(&mut rx).await;
    if opts.ask && !session.final_text.is_empty() {
        println!("{}", session.final_text);
    }
    session.print_result(code);
    code
}

async fn prepare(opts: &Options) -> Result<Prepared, Failure> {
    let common = &opts.common;
    let cfg = config::load(common.config.as_deref())
        .map_err(|error| Failure::Usage(error.to_string()))?;
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

    let task = task_text(&common.task).await.map_err(Failure::Usage)?;
    let id = new_session_id();
    let client = Client::new(provider_cfg, &id, cfg.cache_ttl.clone())
        .map_err(|error| Failure::Usage(error.message))?;
    let dir = crate::paths::session_dir(&id);
    std::fs::create_dir_all(dir.join("commands")).map_err(|error| {
        Failure::Run(format!(
            "cannot create the session directory {}: {error}",
            dir.display()
        ))
    })?;
    let system_text = render_system(&cfg.system_prompt, &cwd);
    let sock = ipc::value(&dir, &id);

    Ok(Prepared {
        json: opts.json,
        ask: opts.ask,
        keep: opts.keep,
        cfg,
        provider_name,
        model,
        client,
        id,
        dir,
        task,
        cwd,
        system_text,
        sock,
    })
}

impl Session {
    fn start(prepared: Prepared) -> Result<Self, String> {
        let events_path = prepared.dir.join("events.jsonl");
        let writer = EventLog::open(&events_path)
            .map_err(|error| format!("cannot open {}: {error}", events_path.display()))?;
        let ctx = Ctx::new(
            prepared.dir.clone(),
            prepared.cwd.clone(),
            prepared.cfg.shell.clone(),
            prepared.cfg.spill_limit,
            prepared.sock.clone(),
        );
        let mut session = Session {
            id: prepared.id.clone(),
            dir: prepared.dir.clone(),
            writer,
            blobs: Blobs::new(&prepared.dir),
            events: Vec::new(),
            cfg: prepared.cfg.clone(),
            provider_name: prepared.provider_name.clone(),
            model: prepared.model.clone(),
            client: prepared.client.clone(),
            ctx,
            json: prepared.json,
            echo_assistant: !prepared.ask,
            keep: prepared.keep,
            final_text: String::new(),
            step: 0,
            turn: 0,
            disabled: BTreeSet::new(),
            live_calls: Vec::new(),
            scheduler: Scheduler::default(),
        };
        let hash = crate::util::fnv1a64_hex(prepared.system_text.as_bytes());
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
                text: prepared.system_text.clone().into(),
                hash,
            },
            sources: Vec::new(),
            tools: tools::names(),
        })?;
        session.record(EventBody::User {
            text: prepared.task.clone().into(),
            from: "cli".into(),
            delivery: "now".into(),
            attachments: Vec::new(),
        })?;
        session.sync();
        Ok(session)
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
            "running": self.live_calls,
            "provider": self.provider_name,
            "model": self.model,
            "disabled": self.disabled.iter().cloned().collect::<Vec<_>>(),
        })
    }

    // -------------------------------------------------------------- verbs

    /// Log the verb (applied or not), validate it and answer the client. The
    /// verb is *applied* later, at its boundary.
    fn accept(&mut self, incoming: Incoming) -> Option<Pending> {
        let Incoming { request, reply } = incoming;
        let from = request.from.clone();
        let name = request.verb.clone();
        let mut parsed = verbs::parse(&name, &request.args);
        // `stop --call` for a call that is not running is a refusal, not a
        // silent no-op.
        if let Ok(Verb::Stop {
            call: Some(call), ..
        }) = &parsed
        {
            if !self.live_calls.iter().any(|id| id == call) {
                parsed = Err(format!("no running call with id `{call}`"));
            }
        }
        let boundary = match &parsed {
            Ok(verb) => verb.scope().to_string(),
            Err(_) => "rejected".to_string(),
        };
        let error = parsed.as_ref().err().cloned();
        let recorded = self.record(EventBody::Verb {
            name: name.clone(),
            args: Payload::value(request.args.clone()),
            from: from.clone(),
            boundary,
            error: error.clone(),
        });
        let pending = match (parsed, recorded) {
            (Ok(verb), Ok(event)) => {
                let pending = Pending {
                    seq: event.seq,
                    verb: verb.clone(),
                    from,
                };
                let _ = reply.send(Response::ok(&name, event.seq, verb.scope(), self.state()));
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
        };
        self.sync();
        pending
    }

    /// Apply the verbs due at `boundary`.
    fn apply(&mut self, boundary: Boundary) -> Result<Applied, String> {
        let mut applied = Applied::default();
        for pending in self.scheduler.due(boundary) {
            match pending.verb {
                Verb::Say { text, now } => {
                    self.record(EventBody::User {
                        text: Payload::text(text),
                        from: pending.from,
                        delivery: if now { "now" } else { "next" }.to_string(),
                        attachments: Vec::new(),
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
                Verb::End => applied.end = true,
            }
        }
        self.sync();
        Ok(applied)
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
        let provider_cfg = self
            .cfg
            .provider(&provider)
            .map_err(|error| error.to_string())?;
        let client = Client::new(provider_cfg, &self.id, self.cfg.cache_ttl.clone())
            .map_err(|error| error.message)?;
        self.provider_name = provider;
        self.model = model;
        self.client = client;
        Ok(())
    }

    /// Messages that should start the next turn: `later` deliveries, and any
    /// `say` that was still waiting when the turn ended.
    fn take_turn_messages(&mut self) -> Option<TurnStart> {
        let mut messages = Vec::new();
        let mut end = false;
        for pending in self.scheduler.due(Boundary::TurnEnd) {
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
                Verb::End => end = true,
                Verb::Stop { .. } | Verb::Model { .. } | Verb::Tools { .. } => {}
            }
        }
        if end {
            Some(TurnStart::End)
        } else if messages.is_empty() {
            None
        } else {
            Some(TurnStart::Messages(messages))
        }
    }

    fn record_messages(&mut self, messages: Vec<Message>) -> Result<(), String> {
        for message in messages {
            self.record(EventBody::User {
                text: Payload::text(message.text),
                from: message.from,
                delivery: message.delivery.to_string(),
                attachments: Vec::new(),
            })?;
        }
        self.sync();
        Ok(())
    }

    // --------------------------------------------------------------- turns

    /// Run the session: one turn, then more when `--keep` or when messages are
    /// queued. Ends with `session_ended`.
    async fn run(&mut self, rx: &mut Inbox) -> u8 {
        let mut code = 0u8;
        let mut reason = "done".to_string();
        loop {
            self.turn += 1;
            let end = self.drive_turn(rx).await;
            let failed = match &end {
                Ok(TurnEnd::Done) => {
                    let _ = self.record(EventBody::TurnEnded {
                        outcome: "done".into(),
                        final_text: self.final_text.clone().into(),
                    });
                    false
                }
                Ok(TurnEnd::Stopped(why)) => {
                    if !self.json && !why.is_empty() {
                        eprintln!("→ turn stopped: {why}");
                    }
                    let _ = self.record(EventBody::TurnEnded {
                        outcome: "stopped".into(),
                        final_text: self.final_text.clone().into(),
                    });
                    reason = "stopped".into();
                    false
                }
                Ok(TurnEnd::End) => {
                    let _ = self.record(EventBody::TurnEnded {
                        outcome: "stopped".into(),
                        final_text: self.final_text.clone().into(),
                    });
                    reason = "end".into();
                    false
                }
                Err(message) => {
                    eprintln!("kelir: {message}");
                    let _ = self.record(EventBody::TurnEnded {
                        outcome: "error".into(),
                        final_text: String::new().into(),
                    });
                    code = 1;
                    reason = "error".into();
                    true
                }
            };
            self.sync();
            if failed || matches!(end, Ok(TurnEnd::End)) {
                break;
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
        code
    }

    /// `--keep` and no turn running: take verbs until one starts a turn.
    async fn idle(&mut self, rx: &mut Inbox) -> Idle {
        loop {
            let Some(incoming) = rx.recv().await else {
                return Idle::End;
            };
            if let Some(pending) = self.accept(incoming) {
                self.scheduler.push(pending);
            }
            match self.take_turn_messages() {
                Some(TurnStart::End) => return Idle::End,
                Some(TurnStart::Messages(messages)) => {
                    if self.record_messages(messages).is_err() {
                        return Idle::End;
                    }
                    return Idle::Turn;
                }
                None => {}
            }
        }
    }

    async fn drive_turn(&mut self, rx: &mut Inbox) -> Result<TurnEnd, String> {
        self.final_text.clear();
        let mut turn_steps = 0u32;
        loop {
            turn_steps += 1;
            if turn_steps > self.cfg.max_steps {
                return Err(format!("step limit {} reached", self.cfg.max_steps));
            }

            // ---------------------------------------------------- before_step
            let applied = self.apply(Boundary::BeforeStep)?;
            if applied.end {
                return Ok(TurnEnd::End);
            }
            if let Some(reason) = applied.stopped {
                return Ok(TurnEnd::Stopped(reason));
            }

            let folded = crate::fold::fold(&self.events, &self.dir)?;
            let request =
                self.client
                    .build(&self.model, &folded.request_messages(), &tools::specs());
            let request_text = serde_json::to_string(&request.body).unwrap_or_default();
            self.step += 1;
            let step = self.step;
            self.record(EventBody::StepStarted {
                step,
                provider: self.provider_name.clone(),
                model: self.model.clone(),
                request_hash: crate::util::fnv1a64_hex(request_text.as_bytes()),
            })?;

            let assembler = match self.stream(rx, &request).await? {
                StreamEnd::Assembled(assembler) => assembler,
                StreamEnd::Stopped(reason) => return Ok(TurnEnd::Stopped(reason)),
                StreamEnd::End => return Ok(TurnEnd::End),
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
            if let Some(usage) = &assembler.usage {
                self.record(EventBody::Usage {
                    step,
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                    cached_tokens: usage.cached_tokens,
                    cache_write_tokens: usage.cache_write_tokens,
                    cost: usage.cost,
                    allowance: usage.allowance.clone(),
                })?;
            }
            // Even a turn that ends up stopped keeps the last thing the model
            // said in `turn_ended.final_text`.
            self.final_text = text.clone();
            self.sync();

            if !self.json && self.echo_assistant && !text.is_empty() {
                println!("{text}");
                let _ = std::io::stdout().flush();
            }

            if calls.is_empty() {
                self.final_text = text;
                // ---------------------------------------------------- after_step
                let applied = self.apply(Boundary::AfterStep)?;
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
            let applied = self.apply(Boundary::AfterTools)?;
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

    /// Stream one response, taking verbs while it is in flight. A `stop` ends
    /// the turn without committing the half-response; anything else waits for
    /// the next boundary.
    async fn stream(
        &mut self,
        rx: &mut Inbox,
        request: &provider::BuiltRequest,
    ) -> Result<StreamEnd, String> {
        let mut attempt = 0u32;
        loop {
            let client = self.client.clone();
            let future = client.stream(request);
            tokio::pin!(future);
            let mut outcome: Option<Result<crate::stream::Assembler, provider::ProviderError>> =
                None;
            let mut interrupt: Option<StreamEnd> = None;
            while outcome.is_none() && interrupt.is_none() {
                tokio::select! {
                    result = &mut future => outcome = Some(result),
                    incoming = rx.recv() => {
                        if let Some(incoming) = incoming {
                            if let Some(pending) = self.accept(incoming) {
                                match pending.verb {
                                    Verb::Stop { call: None, reason } => {
                                        interrupt = Some(StreamEnd::Stopped(
                                            reason.unwrap_or_else(|| "stopped by user".into()),
                                        ));
                                    }
                                    Verb::End => interrupt = Some(StreamEnd::End),
                                    _ => self.scheduler.push(pending),
                                }
                            }
                        }
                    }
                }
            }
            if let Some(end) = interrupt {
                return Ok(end);
            }
            match outcome.expect("the stream ended without a result") {
                Ok(assembler) => return Ok(StreamEnd::Assembled(assembler)),
                Err(error) => {
                    let will_retry = error.retryable && attempt + 1 < provider::MAX_ATTEMPTS;
                    self.record(EventBody::Error {
                        location: "provider".into(),
                        message: error.message.clone(),
                        retryable: error.retryable,
                    })?;
                    if !will_retry {
                        return Err(error.message);
                    }
                    self.sync();
                    tokio::time::sleep(provider::backoff(attempt)).await;
                    attempt += 1;
                }
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
            })?;
            plan.push((
                format!("{}-{}-{}", started.seq, step, index + 1),
                call.clone(),
            ));
        }
        self.sync();

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
            let gate = gates[index].clone();
            let tx = done_tx.clone();
            handles.push(tokio::spawn(async move {
                let started = Instant::now();
                let outcome = if gate.load(Ordering::SeqCst) {
                    ToolOutcome::skipped("stopped before it started")
                } else if disabled {
                    ToolOutcome::disabled(&name)
                } else {
                    tools::dispatch(&name, &args, &ctx, &dir).await
                };
                let _ = tx.send((index, outcome, started.elapsed())).await;
            }));
        }
        drop(done_tx);

        let mut slots: Vec<Option<(ToolOutcome, Duration)>> = (0..count).map(|_| None).collect();
        // Killed calls: their result becomes `stopped by user: <reason>`.
        let mut forced: Vec<Option<String>> = (0..count).map(|_| None).collect();
        let mut batch = Batch::default();
        let mut deadline: Option<Instant> = None;

        while slots.iter().any(Option::is_none) {
            if deadline.is_some_and(|end| Instant::now() >= end) {
                break;
            }
            tokio::select! {
                item = done_rx.recv() => match item {
                    Some((index, outcome, elapsed)) => {
                        if let Some(slot) = slots.get_mut(index) {
                            *slot = Some((outcome, elapsed));
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
                                }
                                deadline = Some(Instant::now() + STOP_GRACE);
                            }
                        }
                        Verb::Stop { call: None, reason } => {
                            let reason = reason.unwrap_or_default();
                            self.kill_batch(&plan, &slots, &gates, &mut forced, &reason);
                            batch.stopped = Some(reason);
                            deadline = Some(Instant::now() + STOP_GRACE);
                        }
                        Verb::Say { now: true, .. } => {
                            // Stop the running calls, settle, and let the
                            // message in at the after_tools boundary.
                            self.kill_batch(&plan, &slots, &gates, &mut forced, "");
                            self.scheduler.push(pending);
                            deadline = Some(Instant::now() + STOP_GRACE);
                        }
                        Verb::End => {
                            self.kill_batch(&plan, &slots, &gates, &mut forced, "");
                            batch.end = true;
                            deadline = Some(Instant::now() + STOP_GRACE);
                        }
                        _ => self.scheduler.push(pending),
                    }
                }
                _ = tokio::time::sleep(STOP_POLL), if deadline.is_some() => {
                    // A call that started just after the kill is taken down too,
                    // so no orphan keeps working after the turn moved on.
                    self.retake(&plan, &mut forced);
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

        for (index, ((_, call), slot)) in plan.iter().zip(slots).enumerate() {
            let (mut outcome, elapsed) = slot.unwrap_or_else(|| {
                (
                    ToolOutcome::error("the call never reported a result"),
                    Duration::ZERO,
                )
            });
            if let Some(reason) = &forced[index] {
                outcome = ToolOutcome::stopped(reason);
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
            if !self.json {
                println!(
                    "→ {} {} · {} · {} B · {:.1}s",
                    call.name,
                    compact_input(&call.args),
                    outcome.outcome,
                    outcome.bytes,
                    elapsed.as_secs_f64()
                );
            }
        }
        self.sync();
        Ok(batch)
    }

    /// Kill every call that has a process, and gate the ones that do not.
    /// Calls already settled keep their real result.
    fn kill_batch(
        &self,
        plan: &[(String, ToolCall)],
        slots: &[Option<(ToolOutcome, Duration)>],
        gates: &[Arc<AtomicBool>],
        forced: &mut [Option<String>],
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
        }
    }

    /// Take down calls that registered a process after the kill.
    fn retake(&self, plan: &[(String, ToolCall)], forced: &mut [Option<String>]) {
        let running = self.ctx.running.keys();
        for (index, (dir, _)) in plan.iter().enumerate() {
            if forced[index].is_some() && running.iter().any(|key| key == dir) {
                self.ctx.running.kill(dir);
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

fn new_session_id() -> String {
    format!("ses_{}", ulid::Ulid::new().to_string().to_lowercase())
}

fn render_system(base: &str, cwd: &Path) -> String {
    format!(
        "{}\n\n# Environment\ncwd: {}\nos: {}\ndate: {}\n",
        base.trim_end(),
        crate::util::path_string(cwd),
        std::env::consts::OS,
        today()
    )
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
    fn system_prompt_has_a_date_and_no_time() {
        let text = render_system("base", Path::new("work"));
        assert!(text.starts_with("base\n"));
        assert!(text.contains("cwd: work"));
        let date_line = text.lines().find(|line| line.starts_with("date:")).unwrap();
        // `date: YYYY-MM-DD`: the only colon is the label's.
        assert_eq!(date_line.chars().filter(|c| *c == ':').count(), 1);
        assert_eq!(date_line.len(), "date: 2026-11-18".len());
    }

    #[test]
    fn system_prompt_is_stable_within_a_run() {
        let a = render_system("base", Path::new("tmp"));
        let b = render_system("base", Path::new("tmp"));
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
                    extra_headers: Default::default(),
                    reasoning_field: None,
                    session_header: None,
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
}
