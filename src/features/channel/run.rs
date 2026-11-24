//! Channel mode: chat bridges through MCP, a durable spool, fallback replies.
//! A *channel server* is an MCP server with `channel = true`.
//! It sends inbound chat messages as `notifications/claude/channel`; kelir answers on the same
//! server with a configurable reply tool and tells it when a turn ends.
//! This module is the generic mechanism — a concrete bridge (Telegram or anything else) is

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::main_cli::RunArgs as RunCliArgs;

use crate::features::mcp::broker::Broker;

use super::*;

// ------------------------------------------------------------- channel run

/// The work item kept after handing a message to a session.
#[derive(Debug, Clone)]
struct Active {
    session: String,
    start_seq: u64,
}

/// The configured reply tool for a server, or the default.
fn reply_tool(broker: &Broker, server: &str) -> String {
    let value = broker.servers();
    value["servers"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["name"].as_str() == Some(server))
        .and_then(|entry| entry["reply_tool"].as_str())
        .unwrap_or(DEFAULT_REPLY_TOOL)
        .to_string()
}

/// Start a turn in the conversation's named session. A fresh session takes the envelope as
/// its task; a resumed one takes it as a new message.
async fn start_turn(
    common: &crate::main_cli::CommonArgs,
    message: Message,
) -> Result<Option<Active>, String> {
    let name = message.session_name();
    // A channel conversation is a named session, but its task is a remote user message, not a
    // job's: `from: channel` on the user event and `prompt_submit` gates on the envelope.
    let target = crate::features::names::resolve_for_run(&name)?;
    let (session, resume) = match target {
        crate::features::names::Target::Resume(id) => (id, true),
        crate::features::names::Target::New(id) => (id, false),
    };
    let dir = crate::paths::session_dir(&session);
    // `/stop` from chat maps to the `stop` verb (never a prompt): only the
    // session's own token, read from its dir, authorises it.
    if let Some(reason) = stop_command(&message.text) {
        let args = crate::core::verbs::cli_args(
            "stop",
            &[],
            &crate::core::verbs::CliFlags {
                reason: Some(reason),
                ..Default::default()
            },
        )?;
        let request = crate::ipc::Request::new("stop", args, "channel");
        let _ = crate::ipc::request_with_token_file_within(
            &dir,
            &session,
            &request,
            crate::features::children::FOREGROUND_WAIT,
        )
        .await;
        return Ok(None);
    }
    let start_seq = crate::log::event::read_events(&dir.join("events.jsonl"))
        .map(|read| read.events.len() as u64)
        .unwrap_or(0);
    let pointers = save_attachments(&message, &dir);
    let mut text = message.envelope();
    if !pointers.is_empty() {
        text.push_str(&format!("\n[attachments {}]\n", pointers.join(" ")));
    }
    let started = session.clone();
    let code = {
        let task = RunCliArgs {
            common: common.clone(),
            task: vec![text],
            ask: false,
            json: false,
            keep: false,
            resume: resume.then(|| session.clone()),
            child: false,
            background: false,
            // The id travels in the turn's own task-local slot.
            name: None,
            fresh: false,
            single_flight: false,
            timeout: None,
            task_file: None,
        };
        crate::features::channel::with_next_id(started, crate::core::session::run(task)).await
    };
    if code != 0 {
        return Err(format!("session {session} exited {code}"));
    }
    Ok(Some(Active { session, start_seq }))
}

fn turn_events(active: &Active) -> Vec<crate::log::event::LogEvent> {
    let path = crate::paths::session_dir(&active.session).join("events.jsonl");
    let read = crate::log::event::read_events(&path).unwrap_or_default();
    read.events
        .into_iter()
        .skip(active.start_seq as usize)
        .collect()
}

/// Rate limiter: a fixed window per conversation and one globally. Bursts of a thousand
/// messages stay bounded; excess messages are queued, not answered in parallel.
#[derive(Debug)]
pub struct RateLimits {
    per_conversation: u32,
    global: u32,
    window: Duration,
    pub(crate) started: Instant,
    conversation: HashMap<String, u32>,
    total: u32,
}

impl RateLimits {
    pub fn new(per_conversation: u32, global: u32, window: Duration) -> RateLimits {
        RateLimits {
            per_conversation,
            global,
            window,
            started: Instant::now(),
            conversation: HashMap::new(),
            total: 0,
        }
    }

    fn tick(&mut self) {
        if self.started.elapsed() >= self.window {
            self.started = Instant::now();
            self.conversation.clear();
            self.total = 0;
        }
    }

    /// Whether one message may start a turn now (and consumes a token).
    pub fn allow(&mut self, conversation: &str) -> bool {
        self.tick();
        let used = self
            .conversation
            .entry(conversation.to_string())
            .or_insert(0);
        if *used >= self.per_conversation || self.total >= self.global {
            return false;
        }
        *used += 1;
        self.total += 1;
        true
    }

    pub fn remaining(&self) -> u32 {
        self.global.saturating_sub(self.total)
    }
}

/// One conversation's work. A second message while a turn runs waits here; a
/// single-flight flag keeps two turns for one conversation from ever running.
#[derive(Debug, Default)]
pub struct Conversation {
    pub queue: VecDeque<Message>,
    pub running: bool,
    /// Messages whose final turn was logged but whose reply could not be sent
    /// yet (server unavailable): the fallback is retried once on the next turn.
    pub unsettled: VecDeque<String>,
}

/// The bounded set of conversations. `max_active` turns may run at once; the rest stay
/// queued.
#[derive(Debug, Default)]
pub struct Dispatcher {
    pub conversations: BTreeMap<String, Conversation>,
    active: usize,
    max_active: usize,
}

impl Dispatcher {
    pub fn new(max_active: usize) -> Dispatcher {
        Dispatcher {
            conversations: BTreeMap::new(),
            active: 0,
            max_active: max_active.max(1),
        }
    }

    /// Queue a message. Returns `false` when this conversation already has one
    /// running and the message has to wait (delivery `later`).
    pub fn push(&mut self, message: Message) -> bool {
        let entry = self
            .conversations
            .entry(message.conversation.clone())
            .or_default();
        let idle = !entry.running && entry.queue.is_empty();
        entry.queue.push_back(message);
        idle
    }

    /// Take the next message to run, respecting `max_active` and single flight.
    pub fn take_next(&mut self) -> Option<Message> {
        if self.active >= self.max_active {
            return None;
        }
        for entry in self.conversations.values_mut() {
            if !entry.running
                && let Some(message) = entry.queue.pop_front()
            {
                entry.running = true;
                self.active += 1;
                return Some(message);
            }
        }
        None
    }

    /// A turn ended: free its conversation and its slot.
    pub fn finish(&mut self, conversation: &str) {
        if let Some(entry) = self.conversations.get_mut(conversation) {
            entry.running = false;
        }
        self.active = self.active.saturating_sub(1);
    }

    pub fn active(&self) -> usize {
        self.active
    }

    /// Total queued messages, for `ctl status`'s queue depth.
    pub fn queued(&self) -> usize {
        self.conversations
            .values()
            .map(|entry| entry.queue.len())
            .sum()
    }

    pub fn size(&self) -> usize {
        self.conversations.len()
    }
}

// ------------------------------------------------------------------ run

/// Live counters for the driver's `Status` snapshot.
#[derive(Debug, Default)]
struct Counters {
    replies: u64,
    fallbacks: u64,
}

/// Write the status snapshot. One small atomic file; `kelir ctl status` and
/// tests read it. Never holds a secret (server names and counts only).
fn write_status(state: &Path, status: &Status) {
    let dir = state.join("spool");
    if crate::paths::create_private_dir(&dir).is_err() {
        return;
    }
    let bytes = serde_json::to_vec(status).unwrap_or_default();
    let _ = crate::sys::atomic::write_atomic(&crate::paths::channel_status_path_in(state), &bytes);
}

/// Everything the concurrent turns share. One thread (a `LocalSet`), so plain
/// `RefCell`s; no borrow is ever held across an `.await`.
struct Shared {
    common: crate::main_cli::CommonArgs,
    broker: Broker,
    cfg: Config,
    state: PathBuf,
    spools: std::cell::RefCell<BTreeMap<String, Spool>>,
    counters: std::cell::RefCell<Counters>,
    herdr: std::cell::RefCell<Option<crate::features::herdr::Herdr>>,
    dispatcher: std::cell::RefCell<Dispatcher>,
    inflight: std::cell::Cell<usize>,
}

impl Shared {
    fn snapshot(&self) -> Status {
        let dispatcher = self.dispatcher.borrow();
        let counters = self.counters.borrow();
        Status {
            conversations: dispatcher.size(),
            queue_depth: dispatcher.queued(),
            active: dispatcher.active(),
            spool_backlog: self
                .spools
                .borrow()
                .iter()
                .map(|(server, spool)| (server.clone(), spool.replay().len()))
                .collect(),
            replies: counters.replies,
            fallbacks: counters.fallbacks,
            runtime: "channel".into(),
        }
    }

    fn report(&self, state: crate::features::herdr::State) {
        if let Some(writer) = self.herdr.borrow_mut().as_mut() {
            writer.report(state);
        }
    }
}

fn fallback_text(cfg: &Config, outcome: &Outcome) -> String {
    if cfg.fallback != DEFAULT_FALLBACK {
        return cfg.fallback.clone();
    }
    match outcome {
        Outcome::Gated(_) => GATED_FALLBACK.into(),
        Outcome::Failed(reason)
            if reason.starts_with("turn timeout") || reason.starts_with("turn timed") =>
        {
            TIMEOUT_FALLBACK.into()
        }
        _ => DEFAULT_FALLBACK.into(),
    }
}

/// One inbound message: `claimed`, a turn, one reply, then `done` (always, including a handled
/// `/stop`).
async fn process(message: Message, shared: Rc<Shared>) {
    let conversation = message.conversation.clone();
    process_turn(message, &shared).await;
    shared.dispatcher.borrow_mut().finish(&conversation);
    if shared.dispatcher.borrow().active() == 0 {
        shared.report(crate::features::herdr::State::Idle);
    }
    write_status(&shared.state, &shared.snapshot());
}

async fn process_turn(message: Message, shared: &Shared) {
    let server = message.server.clone();
    let tool = reply_tool(&shared.broker, &server);
    {
        let mut spools = shared.spools.borrow_mut();
        let Some(spool) = spools.get_mut(&server) else {
            return;
        };
        if let Err(error) = spool.claim(&message.id) {
            eprintln!("kelir channel: spool claim: {error}");
            return;
        }
    }
    shared.report(crate::features::herdr::State::Working);
    let turn = start_turn(&shared.common, message.clone()).await;
    let final_outcome = match &turn {
        Err(error) => Outcome::Failed(error.clone()),
        Ok(None) => Outcome::Done("stopped".into()),
        Ok(Some(item)) => outcome(&turn_events(item)),
    };
    let text = match &final_outcome {
        Outcome::Done(text) => text.clone(),
        other => fallback_text(&shared.cfg, other),
    };
    let sent = reply(&shared.broker, &server, &tool, &message, &text).await;
    match sent {
        Ok(()) => shared.counters.borrow_mut().replies += 1,
        Err(error) => {
            shared.counters.borrow_mut().fallbacks += 1;
            eprintln!("kelir channel: {error}");
        }
    }
    turn_done(&shared.broker, &server, &message.conversation).await;
    let mut spools = shared.spools.borrow_mut();
    if let Some(spool) = spools.get_mut(&server) {
        if let Err(error) = spool.done(&message.id) {
            eprintln!("kelir channel: spool done: {error}");
        }
        if let Ok(true) = spool.compact(shared.cfg.spool_limit, &crate::log::event::now_ts())
            && let Ok(fresh) = Spool::open(&shared.state, &server)
        {
            // The compaction rewrite is durable; reopen so the handle follows
            // the new file.
            *spool = fresh;
        }
    }
}

/// `kelir channel`: run all configured channel servers until interrupted.
pub async fn run(args: crate::main_cli::ChannelArgs) -> u8 {
    // Turns run concurrently on one thread (their futures are not `Send`), hence a `LocalSet`.
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("kelir-channel".into())
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let code = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_stack_size(16 * 1024 * 1024)
                .build()
            {
                Ok(runtime) => {
                    runtime.block_on(tokio::task::LocalSet::new().run_until(drive(args)))
                }
                Err(error) => {
                    eprintln!("kelir channel: cannot start the runtime: {error}");
                    1
                }
            };
            let _ = tx.send(code);
        });
    if let Err(error) = spawned {
        eprintln!("kelir channel: cannot start the driver thread: {error}");
        return 1;
    }
    rx.await.unwrap_or(1)
}

async fn drive(args: crate::main_cli::ChannelArgs) -> u8 {
    crate::sys::signals::install();
    let cfg = match crate::config::load(args.common.config.as_deref()) {
        Ok(cfg) => cfg,
        Err(error) => {
            eprintln!("kelir channel: {error}");
            return 1;
        }
    };
    let cwd = match args
        .common
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
    {
        Some(cwd) => cwd,
        None => {
            eprintln!("kelir channel: cannot read the current directory");
            return 1;
        }
    };
    CHANNEL_MODE.store(true, std::sync::atomic::Ordering::SeqCst);
    let state = crate::paths::state_dir();
    let session_dir = state.join("channel");
    let _ = crate::paths::create_private_dir(&session_dir);
    let broker = Broker::new(cwd, session_dir, "channel".to_string(), None);
    let configured: Vec<String> = broker.servers()["servers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["channel"].as_bool() == Some(true))
        .filter_map(|entry| entry["name"].as_str().map(str::to_string))
        .collect();
    let servers: Vec<String> = if args.server.is_empty() {
        configured
    } else {
        args.server
            .iter()
            .filter(|name| configured.contains(name))
            .cloned()
            .collect()
    };
    if servers.is_empty() {
        eprintln!("kelir channel: no channel servers configured (channel = true in config.toml)");
        return 1;
    }
    let mut spools = BTreeMap::new();
    for server in &servers {
        match Spool::open(&state, server) {
            Ok(spool) => {
                spools.insert(server.clone(), spool);
            }
            Err(error) => {
                eprintln!("kelir channel: {error}");
                return 1;
            }
        }
    }
    let (tx, mut rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::features::mcp::rpc::Notification>();
    broker.set_channel_sink(tx);
    let mut herdr = crate::features::herdr::Herdr::detect(false);
    if let Some(writer) = herdr.as_mut() {
        writer.report(crate::features::herdr::State::Idle);
    }
    // Connect each channel server now: capability negotiation is what makes a
    // bridge start sending. Failures are logged, never fatal for other servers.
    let mut unavailable: BTreeSet<String> = BTreeSet::new();
    for server in &servers {
        if let Err(error) = broker.ensure(server).await {
            eprintln!(
                "kelir channel: server `{server}` is unavailable: {}",
                error.message()
            );
            unavailable.insert(server.clone());
        }
    }
    let mut last_retry = Instant::now();
    let shared = Rc::new(Shared {
        common: args.common.clone(),
        broker: broker.clone(),
        dispatcher: std::cell::RefCell::new(Dispatcher::new(cfg.channel.max_active)),
        cfg: cfg.channel.clone(),
        state: state.clone(),
        spools: std::cell::RefCell::new(spools),
        counters: std::cell::RefCell::new(Counters::default()),
        herdr: std::cell::RefCell::new(herdr),
        inflight: std::cell::Cell::new(0),
    });
    let mut limits = RateLimits::new(
        cfg.channel.per_conversation_rpm,
        cfg.channel.global_rpm,
        Duration::from_secs(60),
    );
    let mut answered = 0u32;
    let mut logged_drops: BTreeSet<String> = BTreeSet::new();
    let mut checked = tokio::time::interval(Duration::from_millis(100));
    let status = Status {
        runtime: "channel".into(),
        ..Status::default()
    };
    write_status(&state, &status);
    if args.status_once {
        println!("{}", serde_json::to_string(&status).unwrap_or_default());
    }
    loop {
        // A bridge that failed to start (or died) is reconnected every few
        // seconds; a failure is logged once until it comes back.
        if last_retry.elapsed() >= Duration::from_millis(500) {
            last_retry = Instant::now();
            for server in &servers {
                match broker.ensure(server).await {
                    Ok(_) => {
                        unavailable.remove(server);
                    }
                    Err(error) => {
                        if unavailable.insert(server.clone()) {
                            eprintln!(
                                "kelir channel: server `{server}` is unavailable: {}",
                                error.message()
                            );
                        }
                    }
                }
            }
        }
        tokio::select! {
            note = rx.recv() => {
                let Some(note) = note else { break };
                let message = match parse_notification(&note) {
                    Ok(message) => message,
                    Err(Drop::Other) => continue,
                    Err(reason) => {
                        // Once per dropped shape, never the payload: a bridge
                        // can send text containing secrets.
                        if logged_drops.insert(format!("{reason:?}")) {
                            eprintln!("kelir channel: {MALFORMED_NOTE} ({reason:?})");
                        }
                        continue;
                    }
                };
                let mut spools = shared.spools.borrow_mut();
                let Some(spool) = spools.get_mut(&message.server) else { continue };
                match spool.inbound(message.clone()) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => {
                        eprintln!("kelir channel: spool append: {error}");
                        continue;
                    }
                }
                drop(spools);
                // Rate-limited messages stay in the spool and are retried by
                // the replay below, never answered in parallel.
                if limits.allow(&message.conversation) {
                    shared.dispatcher.borrow_mut().push(message);
                }
            }
            _ = checked.tick() => {}
            _ = crate::sys::signals::interrupted() => break,
        }
        // Replay unfinished messages, then start what the caps allow.
        let mut pending: Vec<Message> = shared
            .spools
            .borrow()
            .values()
            .flat_map(|spool| spool.replay().iter().cloned())
            .collect();
        pending.sort_by(|a, b| a.id.cmp(&b.id));
        {
            let mut dispatcher = shared.dispatcher.borrow_mut();
            for message in pending {
                let entry = dispatcher
                    .conversations
                    .entry(message.conversation.clone())
                    .or_default();
                if entry.running || entry.queue.contains(&message) {
                    continue;
                }
                if !limits.allow(&message.conversation) {
                    continue;
                }
                entry.queue.push_back(message);
            }
        }
        while !args.max_messages.is_some_and(|max| answered >= max) {
            let Some(message) = shared.dispatcher.borrow_mut().take_next() else {
                break;
            };
            answered += 1;
            shared.inflight.set(shared.inflight.get() + 1);
            let task_shared = shared.clone();
            tokio::task::spawn_local(async move {
                process(message, task_shared.clone()).await;
                task_shared
                    .inflight
                    .set(task_shared.inflight.get().saturating_sub(1));
            });
        }
        if args.max_messages.is_some_and(|max| answered >= max) && shared.inflight.get() == 0 {
            return 0;
        }
    }
    shared.report(crate::features::herdr::State::Idle);
    0
}
