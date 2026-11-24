//! Scheduled runs: the flags a scheduler passes to `kelir run --name --single-flight`.
//! One run takes the name's lock, writes its STATUS beside the name, drives a named
//! session and records the outcome. kelir has no scheduler, no job file and no history.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::features::names::Target;
use crate::log::event::{EventBody, LogEvent};
use crate::main_cli::RunArgs;
use crate::sys::joblock::{self, Guard, Outcome};

/// Exit codes a scheduled run uses.
pub const EXIT_ERROR: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_SKIPPED: u8 = 75;
pub const EXIT_TIMEOUT: u8 = 124;

/// How long after the deadline's `stop` the process is forced out.
pub const FORCE_AFTER: Duration = Duration::from_secs(20);

pub const STATUS_JSON: &str = "status.json";
pub const LOCK: &str = "lock";

/// `90`, `90s`, `30m`, `12h`, `7d`, `2w`: a length of time. A bare number is
/// seconds.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, unit) = text.split_at(split);
    let count: u64 = digits
        .parse()
        .map_err(|_| format!("`{text}` is not a duration (try 90s, 30m, 12h, 7d)"))?;
    let seconds = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        other => {
            return Err(format!(
                "`{text}`: unknown unit `{other}` (use s, m, h, d or w)"
            ));
        }
    };
    count
        .checked_mul(seconds)
        .map(Duration::from_secs)
        .ok_or_else(|| format!("`{text}` is too long"))
}

/// The numbers of one run (not of the whole session).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Tokens {
    pub prompt: u64,
    pub completion: u64,
    pub cached: u64,
    pub total: u64,
}

/// `<state>/names/<name>.status.json`: the outcome of the last single-flight run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Status {
    pub name: String,
    pub session: Option<String>,
    pub started: String,
    pub finished: Option<String>,
    /// `running` while the run is in flight, then `done`, `error`, `stopped`,
    /// `timeout`, `skipped` or `paused`.
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub steps: u64,
    pub tokens: Tokens,
    pub cache_hit_ratio: Option<f64>,
    /// The first lines of the answer.
    pub answer: String,
    pub error: Option<String>,
    pub pid: u32,
}

pub fn status_path(name: &str) -> PathBuf {
    crate::features::names::dir().join(format!("{name}.status.json"))
}

pub fn lock_path(name: &str) -> PathBuf {
    crate::features::names::dir().join(format!("{name}.lock"))
}

/// Write STATUS atomically: a reader sees a whole old file or a whole new one.
pub fn write_status(status: &Status) -> Result<(), String> {
    let path = status_path(&status.name);
    let json = serde_json::to_string_pretty(status).map_err(|error| error.to_string())?;
    crate::sys::atomic::write_atomic(&path, json.as_bytes())
        .map_err(|error| format!("cannot write {}: {error}", crate::util::path_string(&path)))
}

pub fn read_status(name: &str) -> Result<Option<Status>, String> {
    match std::fs::read_to_string(status_path(name)) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| format!("status.json is unreadable ({error})")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read status.json: {error}")),
    }
}

pub fn head_of(answer: &str) -> String {
    let lines: Vec<&str> = answer.lines().take(12).collect();
    crate::util::truncate_chars(&lines.join("\n"), 1200)
}

/// The date the context last heard: the newest `clock` source, else the day the
/// session started.
pub fn last_announced_date_from(
    events: &[LogEvent],
    rendered_date: Option<&str>,
) -> Option<String> {
    let clock = events.iter().rev().find_map(|event| match &event.body {
        EventBody::Source { kind, name, .. } if kind == "clock" => Some(name.clone()),
        _ => None,
    });
    clock.or_else(|| {
        rendered_date.map(str::to_string).or_else(|| {
            events
                .iter()
                .find(|event| matches!(event.body, EventBody::SessionStarted { .. }))
                .map(|event| event.ts.chars().take(10).collect())
        })
    })
}

/// The tail `source` a resume across a date change adds, or `None` when the
/// context already knows today's date.
pub fn clock_source_from(
    events: &[LogEvent],
    today: &str,
    rendered_date: Option<&str>,
) -> Option<EventBody> {
    let last = last_announced_date_from(events, rendered_date)?;
    if last.as_str() >= today {
        return None;
    }
    Some(EventBody::Source {
        kind: "clock".into(),
        name: today.to_string(),
        text: Some(
            format!("it is now {today} (UTC date); the date this session last knew was {last}")
                .into(),
        ),
        path: None,
    })
}

/// What an old session leaves for its successor: the last compaction summary
/// when it has one, else the last answer.
pub fn carry_text(events: &[LogEvent], dir: &Path) -> Option<String> {
    let summary = events.iter().rev().find_map(|event| match &event.body {
        EventBody::Compacted { summary, .. } => summary.resolve_text(dir).ok(),
        _ => None,
    });
    let text = summary.filter(|text| !text.trim().is_empty()).or_else(|| {
        events.iter().rev().find_map(|event| match &event.body {
            EventBody::TurnEnded { final_text, .. } => final_text
                .resolve_text(dir)
                .ok()
                .filter(|text| !text.trim().is_empty()),
            _ => None,
        })
    })?;
    Some(crate::util::truncate_chars(&text, 6000))
}

// ---------------------------------------------------------------- the run

/// What the loop needs for one run. Passed down instead of a global plan:
/// each process owns exactly one run, and nothing else reads this.
#[derive(Debug, Default)]
pub struct RunOptions {
    pub planned_id: Option<String>,
    pub task_from: Option<&'static str>,
    pub deadline: Option<Duration>,
    pub step_cap: Option<u32>,
    /// This run's mutable state, shared only with its deadline watcher.
    pub state: Option<Arc<RunState>>,
}

/// The deadline watcher and the finalize path share this run's timeout flag.
/// One process owns one run; nothing is global.
#[derive(Debug)]
pub struct RunState {
    timed_out: AtomicBool,
    run: Mutex<Option<Run>>,
}

impl RunState {
    fn new(run: Run) -> Arc<Self> {
        Arc::new(Self {
            timed_out: AtomicBool::new(false),
            run: Mutex::new(Some(run)),
        })
    }

    pub fn note_timeout(&self) {
        self.timed_out.store(true, Ordering::SeqCst);
    }

    fn take(&self) -> Option<Run> {
        self.run
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
    }

    fn took_timeout(&self) -> bool {
        self.timed_out.swap(false, Ordering::SeqCst)
    }
}

/// Everything the end of a run needs, kept where the deadline watcher can
/// reach it too. Taken out (once) by [`finalize`].
#[derive(Debug)]
struct Run {
    name: String,
    started: String,
    session_id: String,
    start_seq: u64,
    timeout: Option<Duration>,
    guard: Option<Guard>,
}

fn session_events(id: &str) -> Vec<LogEvent> {
    crate::log::event::read_events(&crate::paths::session_dir(id).join("events.jsonl"))
        .map(|read| read.events)
        .unwrap_or_default()
}

fn last_seq(events: &[LogEvent]) -> u64 {
    events.last().map(|event| event.seq).unwrap_or(0)
}

fn base_status(name: &str, outcome: &str, session: String, started: String) -> Status {
    Status {
        name: name.to_string(),
        session: (!session.is_empty()).then_some(session),
        started,
        finished: None,
        outcome: outcome.to_string(),
        exit_code: None,
        steps: 0,
        tokens: Tokens::default(),
        cache_hit_ratio: None,
        answer: String::new(),
        error: None,
        pid: std::process::id(),
    }
}

/// Resolve a name to the session a run uses. `--fresh` rebinds it to a new id
/// and hands the old session's summary or answer to the new one as a source.
type ResolvedTarget = (Option<String>, Option<String>, Option<String>);

fn resolve_target(name: &str, fresh: bool) -> Result<ResolvedTarget, String> {
    let (resume, planned) = match crate::features::names::resolve_for_run(name)? {
        Target::Resume(id) => (Some(id), None),
        Target::New(id) => (None, Some(id)),
    };
    if !fresh {
        return Ok((resume, planned, None));
    }
    let Some(old) = resume else {
        return Ok((None, planned, None));
    };
    let events = session_events(&old);
    let carry = carry_text(&events, &crate::paths::session_dir(&old));
    let fresh_id = crate::features::names::new_session_id();
    crate::features::names::set(name, &fresh_id)?;
    Ok((None, Some(fresh_id), carry))
}

/// `kelir run --name X --single-flight`: one scheduled run, with a lock and a
/// STATUS file. Exit 75 if another run holds the lock, 124 at the deadline.
pub async fn run(args: RunArgs) -> u8 {
    let Some(name) = args.name.clone() else {
        return EXIT_USAGE;
    };
    if let Err(why) = crate::features::names::validate(&name) {
        eprintln!("kelir: run: {why}");
        return EXIT_USAGE;
    }
    let timeout = match args.timeout.as_deref().map(parse_duration) {
        Some(Ok(timeout)) => Some(timeout),
        Some(Err(message)) => {
            eprintln!("kelir: run: {message}");
            return EXIT_USAGE;
        }
        None => None,
    };
    let task = match (&args.task_file, &args.task) {
        (Some(path), _) => match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                eprintln!(
                    "kelir: run: cannot read {}: {error}",
                    crate::util::path_string(path)
                );
                return EXIT_USAGE;
            }
        },
        (None, words) if !words.is_empty() => words.join(" "),
        (None, _) => {
            let mut text = String::new();
            if let Err(error) =
                tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::stdin(), &mut text).await
            {
                eprintln!("kelir: run: cannot read the task from stdin: {error}");
                return EXIT_USAGE;
            }
            text
        }
    };
    if args.single_flight
        && let Err(error) = crate::paths::create_private_dir(&crate::features::names::dir())
    {
        eprintln!("kelir: run: cannot create the names directory: {error}");
        return EXIT_ERROR;
    }

    // ---- single flight
    let guard = if args.single_flight {
        let lock_path = lock_path(&name);
        let cmdline = std::env::args().collect::<Vec<_>>().join(" ");
        let acquired =
            tokio::task::spawn_blocking(move || joblock::try_acquire(&lock_path, &cmdline))
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))
                .and_then(|result| result);
        match acquired {
            Ok(Outcome::Held(guard)) => Some(guard),
            Ok(Outcome::Busy(holder)) => {
                let mut status =
                    base_status(&name, "skipped", String::new(), crate::log::event::now_ts());
                status.exit_code = Some(i32::from(EXIT_SKIPPED));
                status.finished = Some(crate::log::event::now_ts());
                status.error = Some(match holder.pid {
                    Some(pid) => format!("another run is in flight (pid {pid} holds the lock)"),
                    None if holder.reason.is_empty() => "another run is in flight".into(),
                    None => format!("another run is in flight ({})", holder.reason),
                });
                if let Err(message) = write_status(&status) {
                    eprintln!("kelir: run: {message}");
                }
                eprintln!(
                    "kelir: run `{name}`: skipped: another run is in flight{}",
                    holder
                        .pid
                        .map(|pid| format!(" (pid {pid})"))
                        .unwrap_or_default()
                );
                return EXIT_SKIPPED;
            }
            Err(error) => {
                eprintln!("kelir: run: cannot take the lock: {error}");
                return EXIT_ERROR;
            }
        }
    } else {
        // `--name` alone still refuses a live session through `resolve_for_run`.
        None
    };

    let started = crate::log::event::now_ts();
    let mut run = Run {
        name: name.clone(),
        started: started.clone(),
        session_id: String::new(),
        start_seq: 0,
        timeout,
        guard,
    };
    let (resume, planned_id, carry) = match resolve_target(&name, args.fresh) {
        Ok(target) => target,
        Err(message) => return fail_early(run, &message, EXIT_ERROR).await,
    };
    run.session_id = resume
        .clone()
        .or_else(|| planned_id.clone())
        .unwrap_or_default();
    run.start_seq = resume
        .as_ref()
        .map(|id| last_seq(&session_events(id)))
        .unwrap_or(0);

    let mut sources: Vec<(String, EventBody)> = Vec::new();
    if let Some(text) = carry {
        sources.push((
            "carry".into(),
            EventBody::Source {
                kind: "carry".into(),
                name: name.clone(),
                text: Some(
                    format!("[kelir] this session replaces the previous one for `{name}`. What it left behind:\n{text}").into(),
                ),
                path: None,
            },
        ));
    }
    if run.guard.is_some() {
        let status = base_status(&name, "running", run.session_id.clone(), started.clone());
        if let Err(message) = write_status(&status) {
            eprintln!("kelir: run: {message}");
        }
    }
    let state = RunState::new(run);
    let options = RunOptions {
        planned_id,
        task_from: if resume.is_some() { Some("job") } else { None },
        deadline: timeout,
        step_cap: args.common.max_steps.filter(|value| *value > 0),
        state: Some(state.clone()),
    };
    let code = crate::core::session::run_with_options(
        crate::core::session::RunRequest {
            common: args.common,
            json: args.json,
            keep: false,
            ask: !args.json,
            task: vec![task],
        },
        options,
        resume,
        sources,
    )
    .await;
    finalize(Some(state), Some(code), None).await
}

async fn fail_early(mut run: Run, message: &str, code: u8) -> u8 {
    eprintln!("kelir: run: {message}");
    if let Some(guard) = run.guard.as_mut() {
        let mut status = base_status(
            &run.name,
            "error",
            run.session_id.clone(),
            run.started.clone(),
        );
        status.finished = Some(crate::log::event::now_ts());
        status.exit_code = Some(i32::from(code));
        status.error = Some(message.to_string());
        if let Err(error) = write_status(&status) {
            eprintln!("kelir: run: {error}");
        }
        guard.release();
    }
    code
}

/// Write the run's end: STATUS and, for a scheduled run, release the lock.
/// Runs once per process (the second caller finds nothing).
pub async fn finalize(
    state: Option<Arc<RunState>>,
    drive_code: Option<u8>,
    early_error: Option<String>,
) -> u8 {
    let state_ref = state.clone();
    let run = match state.and_then(|state| state.take()) {
        Some(run) => run,
        None => return drive_code.unwrap_or(EXIT_ERROR),
    };
    let session_dir = crate::paths::session_dir(&run.session_id);
    let events = if run.session_id.is_empty() {
        Vec::new()
    } else {
        session_events(&run.session_id)
    };
    let fresh: Vec<&LogEvent> = events
        .iter()
        .filter(|event| event.seq > run.start_seq)
        .collect();
    let mut tally = crate::provider::usage::Tally::default();
    for event in &fresh {
        tally.observe(event.seq, &event.body);
    }
    let steps = fresh
        .iter()
        .filter(|event| matches!(event.body, EventBody::StepStarted { .. }))
        .count() as u64;
    let last_turn = fresh.iter().rev().find_map(|event| match &event.body {
        EventBody::TurnEnded {
            outcome,
            final_text,
            ..
        } => Some((
            outcome.clone(),
            final_text.resolve_text(&session_dir).unwrap_or_default(),
        )),
        _ => None,
    });
    let timed_out = state_ref.is_some_and(|state| state.took_timeout());
    let (outcome, exit) = match (&last_turn, early_error.is_some()) {
        (_, true) => ("error", drive_code.unwrap_or(EXIT_ERROR)),
        (Some((outcome, _)), _) if timed_out && outcome != "done" => ("timeout", EXIT_TIMEOUT),
        (None, _) if timed_out => ("timeout", EXIT_TIMEOUT),
        (Some((outcome, _)), _) => match outcome.as_str() {
            "done" => ("done", 0),
            "paused" => ("paused", 0),
            "stopped" => ("stopped", EXIT_ERROR),
            _ => ("error", EXIT_ERROR),
        },
        (None, _) => (
            "error",
            drive_code.filter(|code| *code != 0).unwrap_or(EXIT_ERROR),
        ),
    };
    let error = match outcome {
        "done" => None,
        "timeout" => Some(format!(
            "the run was stopped after its {} timeout",
            run.timeout
                .map(|value| format!("{}s", value.as_secs()))
                .unwrap_or_else(|| "configured".into())
        )),
        _ => early_error
            .or_else(|| tally.last_error.as_ref().map(|error| error.message.clone()))
            .or_else(|| (outcome == "paused").then(|| "the turn paused".to_string())),
    };
    let mut status = base_status(
        &run.name,
        outcome,
        run.session_id.clone(),
        run.started.clone(),
    );
    status.finished = Some(crate::log::event::now_ts());
    status.exit_code = Some(i32::from(exit));
    status.steps = steps;
    status.tokens = Tokens {
        prompt: tally.prompt,
        completion: tally.completion,
        cached: tally.cached,
        total: tally.prompt + tally.completion,
    };
    status.cache_hit_ratio = (tally.prompt > 0).then(|| tally.cached as f64 / tally.prompt as f64);
    status.answer = last_turn
        .as_ref()
        .map(|(_, text)| head_of(text))
        .unwrap_or_default();
    status.error = error;
    if run.guard.is_some()
        && let Err(message) = write_status(&status)
    {
        eprintln!("kelir: run: {message}");
    }
    if !run.session_id.is_empty() {
        crate::features::names::release_owner(&session_dir);
    }
    if let Some(mut guard) = run.guard {
        guard.release();
    }
    exit
}

/// The deadline's last resort: the loop did not end by itself. Record the
/// timeout, release the lock and leave. Tool-call trees follow their owner.
pub async fn force_timeout(state: Arc<RunState>) -> ! {
    let code = finalize(Some(state), None, None).await;
    std::process::exit(i32::from(if code == 0 { EXIT_TIMEOUT } else { code }))
}
