//! The loop: fold → request → stream → log → tools → boundaries → repeat.
//! One session is one process and one append-only log.
//! Every verb that arrives on the session socket is logged and then applied at exactly one
//! [`Boundary`], never between a tool call and its result.
// These private loop modules expose crate-visible helpers to the feature modules.
#![allow(clippy::redundant_pub_crate)]

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
use crate::config::{self, Config};
use crate::core::boundary::Cx;
use crate::core::compact;
use crate::core::verbs::{self, Boundary, Pending, Scheduler, Verb};
use crate::features::herdr::{Herdr, State as HerdrState};
use crate::features::models::{self, Fetch, begin_models};
use crate::ipc::{self, Incoming, Request, Response};
use crate::log::event::{
    Blobs, EventBody, EventLog, LogEvent, LoggedToolCall, Payload, SystemInfo, read_events,
    spill_payloads,
};
use crate::log::resume::{self, UnsettledCall};
use crate::main_cli::{CommonArgs, RunArgs};
use crate::provider::chat::ToolCall;
use crate::provider::keys;
use crate::provider::{self, Client};
use crate::tools::{self, Ctx, ToolOutcome};
use crate::tui::{self};

/// How long a stopped call may take to report back before the session settles it on the
/// user's behalf. The kill is immediate; this is for the process tree to unwind.
const STOP_GRACE: Duration = Duration::from_millis(1500);
/// While a stop is settling, look for calls that started just after the kill
/// and take them down too.
const STOP_POLL: Duration = Duration::from_millis(25);
const SLOT_POLL: Duration = Duration::from_millis(100);
/// How long an idle session sleeps between looks for a metadata fetch that started with the session, so
/// `state` carries the provider's window as soon as it is there instead of at the next turn.
const IDLE_MODELS_WAIT: Duration = Duration::from_millis(100);

/// What a resume says to a log whose last turn ended cleanly but was never closed: the
/// model's last message is its own answer, and a request may not end on one.
const CONTINUE_NOTE: &str = "[kelir] the session was interrupted between turns; continue the task \
                             from where it stopped.";

#[cfg(test)]
mod tests;

mod boot;
mod compactloop;
mod lifecycle;
mod mainloop;
mod model;
mod report;
mod toolloop;
mod turnloop;
pub(crate) mod types;
pub(crate) mod util;
mod verbloop;
mod view;

use self::boot::drive;
pub(crate) use self::boot::provider_base;
use self::types::*;
pub(crate) use self::util::*;

type Inbox = mpsc::UnboundedReceiver<Incoming>;

pub async fn run(args: RunArgs) -> u8 {
    // `--child` starts the task as a child of the session this tool call runs in. Outside any
    // session it is just a run (and `--background` has nothing to wake).
    if args.child {
        if crate::envs::var_os("KELIR_SOCK").is_some() {
            return crate::features::children::cli::run(args).await;
        }
        if args.background {
            eprintln!(
                "kelir: --background needs a session to wake (KELIR_SOCK is not set); \
                 use `kelir bg run` for a detached job"
            );
            return 2;
        }
    }
    let single = args.single_flight || args.timeout.is_some() || args.fresh;
    if single {
        return crate::features::names::namedrun::run(args).await;
    }
    let mut resume = args.resume;
    let mut bound_id = None;
    if let Some(name) = &args.name {
        match crate::features::names::resolve_for_run(name) {
            Ok(crate::features::names::Target::Resume(id)) => resume = Some(id),
            Ok(crate::features::names::Target::New(id)) => bound_id = Some(id),
            Err(message) => {
                eprintln!("kelir: {message}");
                return 1;
            }
        }
    }
    drive(Options {
        json: args.json,
        ask: args.ask,
        keep: args.keep,
        tui: false,
        common: args.common,
        task: args.task,
        resume,
        id: bound_id,
        child: None,
        run_options: None,
    })
    .await
}

pub struct RunRequest {
    pub common: CommonArgs,
    pub json: bool,
    pub keep: bool,
    pub ask: bool,
    pub task: Vec<String>,
}

/// Run one turn of a session with the scheduled-run options passed in
/// explicitly (no process-wide plan or mutable static).
pub async fn run_with_options(
    request: RunRequest,
    options: crate::features::names::namedrun::RunOptions,
    resume: Option<String>,
    sources: Vec<(String, EventBody)>,
) -> u8 {
    drive(Options {
        json: request.json,
        ask: request.ask,
        keep: request.keep,
        tui: false,
        common: request.common,
        task: request.task,
        resume,
        id: options.planned_id.clone(),
        child: None,
        run_options: Some((options, sources)),
    })
    .await
}

/// `kelir` with no subcommand, or `kelir tui`: the interactive session.
pub async fn tui(args: crate::main_cli::TuiArgs) -> u8 {
    if !std::io::stdout().is_terminal() {
        eprintln!(
            "kelir: no terminal: `kelir` is the interactive session and needs a TTY \
             (use `kelir run <task>` or `kelir run --ask <question>` in a script)"
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
        id: None,
        child: None,
        run_options: None,
    })
    .await
}

/// `kelir child-host`: a child session's process.
pub async fn child_host() -> u8 {
    let handoff = match crate::features::children::read_handoff() {
        Ok(handoff) => handoff,
        Err(message) => {
            eprintln!("kelir: {message}");
            return 2;
        }
    };
    let brief = handoff.kind == crate::features::children::KIND_BRIEF;
    drive(Options {
        json: false,
        // The parent wants the answer on stdout, nothing else.
        ask: true,
        keep: false,
        tui: false,
        common: CommonArgs {
            model: handoff.model.clone(),
            provider: handoff.provider.clone(),
            cwd: Some(PathBuf::from(&handoff.cwd)),
            config: None,
            max_steps: None,
        },
        task: vec![handoff.task.clone()],
        // A brief is the parent's log, copied and cut: it continues that log
        // with the question.
        resume: brief.then(|| handoff.session.clone()),
        id: None,
        child: Some(handoff),
        run_options: None,
    })
    .await
}

/// `kelir view <path>`: queue an image for the model's next message.
pub async fn view(args: crate::main_cli::ViewArgs) -> u8 {
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

impl crate::features::mcp::McpCore for Session {
    fn record(&mut self, body: EventBody) -> Result<LogEvent, String> {
        Session::record(self, body)
    }
    fn sync(&mut self) {
        Session::sync(self);
    }
    fn disabled(&self) -> &BTreeSet<String> {
        &self.tools.disabled
    }
    fn live_calls(&self) -> &[String] {
        &self.tools.live_calls
    }
    fn turn(&self) -> u32 {
        self.run.turn
    }
    fn gates(&self) -> Option<Arc<crate::features::gates::Runner>> {
        self.features.gates.as_ref().and_then(|g| g.runner())
    }
    fn inbox(&self) -> Option<mpsc::UnboundedSender<Incoming>> {
        self.inbox.clone()
    }
}

impl crate::features::gates::GatesCore for Session {
    fn record(&mut self, body: EventBody) -> Result<LogEvent, String> {
        Session::record(self, body)
    }
    fn sync(&mut self) {
        Session::sync(self);
    }
    fn emit(&self, event: crate::tui::View) {
        Session::emit(self, event);
    }
    fn turn(&self) -> u32 {
        self.run.turn
    }
    fn tui(&self) -> bool {
        self.out.tui
    }
    fn task_from(&self) -> &'static str {
        Session::task_from(self)
    }
    fn gate_ctx(&self) -> crate::features::gates::GateCtx<'_> {
        Session::gate_ctx(self)
    }
    fn gates_config(&self) -> crate::features::gates::GatesConfig<'_> {
        crate::features::gates::GatesConfig {
            gates: &self.cfg.gates,
            project_gates: &self.cfg.project_gates,
            skills_dirs: &self.cfg.skills_dirs,
            trusted_projects: &self.cfg.trusted_projects,
            config_dir: Some(&self.log.config_dir),
            cwd: &self.ctx.cwd,
            shell: &self.cfg.shell,
            id: &self.log.id,
            dir: &self.log.dir,
        }
    }
    fn events(&self) -> &[LogEvent] {
        &self.log.events
    }
}

/// The fields a scheduled run hands to the loop, plus the resolved session id.
impl Session {
    /// Lend the MCP feature and the core to one operation without overlapping
    /// borrows: the feature is moved out for the duration and put straight back.
    pub(crate) fn mcp_do<R>(
        &mut self,
        f: impl FnOnce(&mut crate::features::mcp::Mcp, &mut Session) -> R,
    ) -> R {
        let mut mcp = self.features.mcp.take().expect("mcp feature");
        let result = f(&mut mcp, self);
        self.features.mcp = Some(mcp);
        result
    }

    /// Same borrow split for the gates feature as [`Session::mcp_do`].
    pub(crate) fn gates_do<R>(
        &mut self,
        f: impl FnOnce(&mut crate::features::gates::Gates, &mut Session) -> R,
    ) -> R {
        let mut gates = self.features.gates.take().expect("gates feature");
        let result = f(&mut gates, self);
        self.features.gates = Some(gates);
        result
    }

    /// Async counterpart to [`Session::gates_do`]: a closure returning a future
    /// that borrows its arguments cannot be named here, so call the feature
    /// method directly while it is taken out.
    pub(crate) async fn gates_prompt(
        &mut self,
        text: String,
        from: &str,
    ) -> Result<Option<String>, String> {
        let mut gates = self.features.gates.take().expect("gates feature");
        let result = gates.prompt(self, text, from).await;
        self.features.gates = Some(gates);
        result
    }

    pub(crate) async fn gates_call(
        &mut self,
        event: crate::features::gates::Event,
        tool: Option<&str>,
        payload: Value,
    ) -> Result<crate::features::gates::Verdict, String> {
        let mut gates = self.features.gates.take().expect("gates feature");
        let result = gates.gate(self, event, tool, payload).await;
        self.features.gates = Some(gates);
        result
    }

    pub(crate) async fn gates_start(&mut self) -> bool {
        let (provider, model, resumed) = (
            self.provider.provider_name.clone(),
            self.provider.model.clone(),
            self.run.resumed,
        );
        let mut gates = self.features.gates.take().expect("gates feature");
        let result = gates.start(self, &provider, &model, resumed).await;
        self.features.gates = Some(gates);
        result
    }

    pub(crate) async fn gates_end(&mut self, reason: &str) {
        let mut gates = self.features.gates.take().expect("gates feature");
        gates.end(self, reason).await;
        self.features.gates = Some(gates);
    }

    /// The date the session's frozen system prompt was rendered with (the
    /// `date:` line in its Environment block).
    pub(crate) fn rendered_date(&self) -> Option<String> {
        let text = self.log.events.iter().find_map(|event| {
            if let EventBody::SessionStarted { system, .. } = &event.body {
                system.text.resolve_text(&self.log.dir).ok()
            } else {
                None
            }
        })?;
        text.lines()
            .find_map(|line| line.strip_prefix("date: "))
            .map(str::trim)
            .filter(|date| {
                date.len() == 10 && date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-'
            })
            .map(str::to_string)
    }

    /// Record the sources a resumed session owes its context: the new date if
    /// the day changed (the frozen system prompt keeps the old one, and the
    /// provider's cache lives in that prefix), then any source a scheduled
    /// run passed in explicitly.
    pub(crate) fn record_plan_sources(&mut self, resumed: bool) -> Result<(), String> {
        let mut sources: Vec<EventBody> = Vec::new();
        if resumed
            && self.kids.root.is_none()
            && let Some(clock) = crate::features::names::namedrun::clock_source_from(
                &self.log.events,
                &today(),
                self.rendered_date().as_deref(),
            )
        {
            sources.push(clock);
        }
        sources.extend(self.run.run_sources.clone());
        for source in sources {
            self.record(source)?;
        }
        Ok(())
    }

    /// `model` swaps the provider/model for the rest of the session: a thin
    /// delegation, so a verb remains a core decision and the state lives in the feature.
    pub(crate) fn swap_model(&mut self, target: &str) -> Result<(), String> {
        let Session {
            log:
                Log {
                    writer,
                    blobs,
                    events,
                    dir,
                    id,
                    ..
                },
            provider:
                Provider {
                    model,
                    provider_name,
                    client,
                    active_provider,
                    governor,
                    ..
                },
            out: Out { ui, json, .. },
            cfg,
            ctx,
            tally,
            features,
            ..
        } = self;
        let mut cx = Cx {
            writer,
            blobs,
            events,
            ui,
            cfg,
            dir,
            id,
            model,
            provider_name,
            client,
            active_provider,
            governor,
            ctx,
            tally,
            json: *json,
        };
        features.models.swap_model(target, &mut cx)
    }
}
