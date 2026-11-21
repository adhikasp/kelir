//! `kelir job run|status|history`: one run of a job, from the lock to the notify hook.
//! A run is: take the job's lock (or exit 75), settle the named session it resumes (or start one,
//! or start a fresh one when `new_session_every` says so), write STATUS as `running`, drive the
//! session loop with a plan the loop reads, then write STATUS for what happened, append the
//! history line, run the notify hooks and release the lock.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};

use crate::cli::{CommonArgs, JobHistoryArgs, JobRunArgs, JobStatusArgs};
use crate::event::{EventBody, LogEvent};
use crate::joblock::{self, Guard, Outcome};
use crate::jobs::{self, Plan, PlanSource, Status, Tokens};
use crate::names::{self, Target};

/// Exit codes of `kelir job run`.
pub const EXIT_ERROR: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_SKIPPED: u8 = 75;
pub const EXIT_TIMEOUT: u8 = 124;

pub const NOTIFY_TIMEOUT: Duration = Duration::from_secs(30);
/// How long after the deadline's `stop` the process is forced out.
pub const FORCE_AFTER: Duration = Duration::from_secs(20);

/// Everything the end of a run needs, kept where the deadline watcher can reach
/// it too. Taken out (once) by [`finalize`].
struct Run {
    job: String,
    run: String,
    dir: PathBuf,
    started: String,
    session_id: String,
    session_name: String,
    start_seq: u64,
    schedule: Option<String>,
    notify: Vec<String>,
    cwd: PathBuf,
    shell: Vec<String>,
    timeout: Option<Duration>,
    guard: Guard,
}

static ACTIVE: Mutex<Option<Run>> = Mutex::new(None);

fn session_events(id: &str) -> Vec<LogEvent> {
    crate::event::read_events(&crate::paths::session_dir(id).join("events.jsonl"))
        .map(|read| read.events)
        .unwrap_or_default()
}

fn last_seq(events: &[LogEvent]) -> u64 {
    events.last().map(|event| event.seq).unwrap_or(0)
}

fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    }
}

// ---------------------------------------------------------------- job run

pub async fn run(args: JobRunArgs) -> u8 {
    let name = args.name.clone();
    if let Err(why) = names::validate(&name) {
        eprintln!("kelir: job: {why}");
        return EXIT_USAGE;
    }
    let mut cwd = match &args.common.cwd {
        Some(path) => absolute(path.clone()),
        None => match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(error) => {
                eprintln!("kelir: job: cannot read the current directory: {error}");
                return EXIT_USAGE;
            }
        },
    };
    let defs = match jobs::load(&cwd) {
        Ok(defs) => defs,
        Err(message) => {
            eprintln!("kelir: job: {message}");
            return EXIT_USAGE;
        }
    };
    let Some(def) = defs.get(&name).cloned() else {
        let known: Vec<&str> = defs.keys().map(String::as_str).collect();
        eprintln!(
            "kelir: job: no job named `{name}` in jobs.toml (known: {})",
            if known.is_empty() {
                "none".to_string()
            } else {
                known.join(", ")
            }
        );
        return EXIT_USAGE;
    };
    if args.common.cwd.is_none()
        && let Some(own) = &def.cwd
    {
        cwd = own.clone();
    }
    let task = match def.task_text() {
        Ok(task) => task,
        Err(message) => {
            eprintln!("kelir: job: {message}");
            return EXIT_USAGE;
        }
    };
    let cfg = crate::config::load(args.common.config.as_deref());
    let dir = jobs::job_dir(&name);
    if let Err(error) = crate::paths::create_private_dir(&dir.join(jobs::NOTES)) {
        eprintln!(
            "kelir: job: cannot create {}: {error}",
            crate::util::path_string(&dir)
        );
        return EXIT_ERROR;
    }

    // ---- single flight
    let lock_path = dir.join(jobs::LOCK);
    let cmdline = std::env::args().collect::<Vec<_>>().join(" ");
    let wait = Duration::from_secs(args.wait.unwrap_or(0));
    let acquired =
        tokio::task::spawn_blocking(move || joblock::acquire(&lock_path, &cmdline, wait))
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))
            .and_then(|result| result);
    let guard = match acquired {
        Ok(Outcome::Held(guard)) => guard,
        Ok(Outcome::Busy(holder)) => {
            let detail = holder
                .pid
                .map(|pid| format!(" (pid {pid} since {})", holder.started))
                .unwrap_or_else(|| {
                    if holder.reason.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", holder.reason)
                    }
                });
            let mut record = jobs::skipped_record(&name, holder.pid);
            if holder.pid.is_none() && !holder.reason.is_empty() {
                record["error"] = json!(format!(
                    "another run of this job holds the lock: {}",
                    holder.reason
                ));
            }
            jobs::append_run(&dir, &record);
            eprintln!(
                "kelir: job `{name}` is already running{}; this run is skipped",
                detail
            );
            return EXIT_SKIPPED;
        }
        Err(error) => {
            eprintln!("kelir: job: cannot take the lock: {error}");
            return EXIT_ERROR;
        }
    };

    // A previous run that never finished its STATUS died: say so in the history.
    if let Ok(Some(previous)) = jobs::read_status(&dir)
        && previous.outcome == "running"
    {
        jobs::append_run(
            &dir,
            &json!({
                "run": previous.run, "job": name, "outcome": "error",
                "started": previous.started, "finished": crate::event::now_ts(),
                "exit_code": null, "session": previous.session,
                "error": format!("the run's process (pid {}) disappeared without finishing", previous.pid),
            }),
        );
    }

    let run_id = format!("run_{}", ulid::Ulid::generate().to_string().to_lowercase());
    let started = crate::event::now_ts();
    let shell = cfg
        .as_ref()
        .map(|cfg| cfg.shell.clone())
        .unwrap_or_default();
    let trusted = match (&cfg, def.origin) {
        (_, jobs::Origin::Machine) => true,
        (Ok(cfg), jobs::Origin::Project) => {
            crate::gates::project_trusted(&cwd, &cfg.trusted_projects)
        }
        (Err(_), jobs::Origin::Project) => false,
    };
    let mut run = Run {
        job: name.clone(),
        run: run_id.clone(),
        dir: dir.clone(),
        started: started.clone(),
        session_id: String::new(),
        session_name: def.session.clone(),
        start_seq: 0,
        schedule: def.schedule.clone(),
        notify: if trusted {
            def.notify.clone()
        } else {
            Vec::new()
        },
        cwd: cwd.clone(),
        shell,
        timeout: def.timeout,
        guard,
    };
    if !trusted && !def.notify.is_empty() {
        eprintln!(
            "kelir: job `{name}`: notify hooks from the project's jobs.toml are not run: \
             this project is not trusted (add it to `trusted_projects` in the machine config, \
             or set KELIR_TRUST_PROJECT=1)"
        );
    }

    // ---- the session this run resumes, or starts
    let mut sources: Vec<PlanSource> = Vec::new();
    let target = names::resolve_for_run(&def.session);
    let (resume, planned): (Option<String>, Option<String>) = match target {
        Err(message) => return fail_early(run, &message, EXIT_ERROR).await,
        Ok(Target::New(id)) => (None, Some(id)),
        Ok(Target::Resume(id)) => {
            let events = session_events(&id);
            let age = jobs::age_of(&events, time::OffsetDateTime::now_utc());
            let tally = crate::spend::Tally::from_events(&events, crate::spend::Warn::default());
            let model = events.iter().rev().find_map(|event| match &event.body {
                EventBody::StepStarted { model, .. } => Some(model.clone()),
                EventBody::SessionStarted { model, .. } => Some(model.clone()),
                _ => None,
            });
            let window = window_for(cfg.as_ref().ok(), model.as_deref(), &events);
            match jobs::should_rotate(
                def.rotate,
                age,
                tally.last.as_ref().map(|l| l.prompt),
                window,
            ) {
                None => (Some(id), None),
                Some(why) => {
                    let fresh = names::new_session_id();
                    if let Err(message) = names::set(&def.session, &fresh) {
                        return fail_early(run, &message, EXIT_ERROR).await;
                    }
                    if let Some(text) = jobs::carry_text(&events, &crate::paths::session_dir(&id)) {
                        sources.push(PlanSource {
                            kind: "carry".into(),
                            name: id.clone(),
                            text: format!(
                                "[kelir] this session replaces `{id}` (new_session_every: {}). \
                                 What it left behind:\n{text}",
                                match why {
                                    jobs::RotateWhy::Age => "the old session was too old",
                                    jobs::RotateWhy::Context => "its context was large",
                                }
                            ),
                        });
                    }
                    (None, Some(fresh))
                }
            }
        }
    };
    let session_id = resume
        .clone()
        .or_else(|| planned.clone())
        .unwrap_or_default();
    run.session_id = session_id.clone();
    run.start_seq = if resume.is_some() {
        last_seq(&session_events(&session_id))
    } else {
        0
    };
    sources.push(PlanSource {
        kind: "job".into(),
        name: name.clone(),
        text: format!(
            "[kelir job] this run is job `{name}` ({run_id}). Status: {}; run history: {}; \
             notes for your later runs (read and write them with bash): {}",
            crate::util::path_string(&dir.join(jobs::STATUS_JSON)),
            crate::util::path_string(&dir.join(jobs::RUNS)),
            crate::util::path_string(&dir.join(jobs::NOTES)),
        ),
    });
    // The effective step cap for this run: the CLI flag wins over jobs.toml.
    let step_cap = args.common.max_steps.or(def.max_steps).filter(|n| *n > 0);
    if !jobs::install(Plan {
        planned_id: planned,
        task_from: Some("job"),
        on_new: true,
        sources,
        deadline: def.timeout,
        job_step_cap: step_cap,
        ..Plan::default()
    }) {
        return fail_early(
            run,
            "a run is already installed in this process",
            EXIT_ERROR,
        )
        .await;
    }

    let running = base_status(&run, "running");
    if let Err(message) = jobs::write_status(&run.dir, &running) {
        eprintln!("kelir: job: {message}");
    }
    *ACTIVE.lock().unwrap_or_else(|p| p.into_inner()) = Some(run);

    let common = CommonArgs {
        model: args.common.model.clone().or(def.model.clone()),
        provider: args.common.provider.clone().or(def.provider.clone()),
        cwd: Some(cwd),
        config: args.common.config.clone(),
        max_steps: step_cap,
    };
    let code = crate::session::run_as_job(common, task, resume).await;
    finalize(Some(code), None).await
}

/// The window a session's context is measured against: a configured one, the
/// provider's cached list, else the default.
fn window_for(
    cfg: Option<&crate::config::Config>,
    model: Option<&str>,
    events: &[LogEvent],
) -> u64 {
    let provider = events.iter().find_map(|event| match &event.body {
        EventBody::SessionStarted { provider, .. } => Some(provider.clone()),
        _ => None,
    });
    let (Some(cfg), Some(model)) = (cfg, model) else {
        return crate::compact::DEFAULT_WINDOW;
    };
    let cache = provider
        .and_then(|provider| {
            cfg.provider(&provider)
                .ok()
                .map(|entry| crate::models::endpoint_id(&provider, &entry.base_url))
        })
        .and_then(|endpoint| crate::models::load(&endpoint));
    crate::models::resolve(cfg.override_window(model), cache.as_ref(), false, model).0
}

fn base_status(run: &Run, outcome: &str) -> Status {
    Status {
        schema: 1,
        job: run.job.clone(),
        session: (!run.session_id.is_empty()).then(|| run.session_id.clone()),
        session_name: Some(run.session_name.clone()),
        run: run.run.clone(),
        started: run.started.clone(),
        finished: None,
        outcome: outcome.to_string(),
        exit_code: None,
        steps: 0,
        tokens: Tokens::default(),
        cost_usd: None,
        cost_source: None,
        cache_hit_ratio: None,
        answer: String::new(),
        error: None,
        schedule: run.schedule.clone(),
        pid: std::process::id(),
        notify_errors: Vec::new(),
    }
}

async fn fail_early(run: Run, message: &str, code: u8) -> u8 {
    eprintln!("kelir: job: {message}");
    *ACTIVE.lock().unwrap_or_else(|p| p.into_inner()) = Some(run);
    finalize(Some(code), Some(message.to_string())).await
}

// -------------------------------------------------------------- finalize

/// Write the run's end: STATUS, the history line, the notify hooks, then
/// release the lock. Runs once per process (the second caller finds nothing).
pub async fn finalize(drive_code: Option<u8>, early_error: Option<String>) -> u8 {
    let taken = ACTIVE.lock().unwrap_or_else(|p| p.into_inner()).take();
    let Some(mut run) = taken else {
        return drive_code.unwrap_or(EXIT_ERROR);
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
    let mut tally = crate::spend::Tally::default();
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
    let timed_out = jobs::timed_out();
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
            drive_code.filter(|c| *c != 0).unwrap_or(EXIT_ERROR),
        ),
    };
    let planned_error =
        jobs::plan().and_then(|plan| plan.error.lock().ok().and_then(|e| e.clone()));
    let error = match outcome {
        "done" => None,
        "timeout" => Some(format!(
            "the run was stopped after its {} timeout",
            run.timeout
                .map(|t| format!("{}s", t.as_secs()))
                .unwrap_or_else(|| "configured".into())
        )),
        _ => early_error
            .or(planned_error)
            .or_else(|| tally.last_error.as_ref().map(|e| e.message.clone()))
            .or_else(|| (outcome == "paused").then(|| "the turn paused".to_string())),
    };
    let known_cost = tally.by_provider + tally.by_table > 0;
    let mut status = base_status(&run, outcome);
    status.finished = Some(crate::event::now_ts());
    status.exit_code = Some(i32::from(exit));
    status.steps = steps;
    status.tokens = Tokens {
        prompt: tally.prompt,
        completion: tally.completion,
        cached: tally.cached,
        total: tally.prompt + tally.completion,
    };
    status.cost_usd = known_cost.then_some(tally.cost);
    status.cost_source = known_cost.then(|| tally.cost_source().to_string());
    status.cache_hit_ratio = (tally.prompt > 0).then(|| tally.cached as f64 / tally.prompt as f64);
    status.answer = last_turn
        .as_ref()
        .map(|(_, text)| jobs::head_of(text))
        .unwrap_or_default();
    status.error = error;

    // The history line first: it is append-only and survives a failed STATUS.
    jobs::append_run(
        &run.dir,
        &json!({
            "run": status.run, "job": status.job, "session": status.session,
            "session_name": status.session_name, "started": status.started,
            "finished": status.finished, "outcome": status.outcome,
            "exit_code": status.exit_code, "steps": status.steps,
            "tokens": status.tokens, "cost_usd": status.cost_usd,
            "cache_hit_ratio": status.cache_hit_ratio,
            "answer": crate::util::truncate_chars(status.answer.lines().next().unwrap_or(""), 200),
            "error": status.error,
        }),
    );
    if let Err(message) = jobs::write_status(&run.dir, &status) {
        eprintln!("kelir: job: {message}");
    }
    // Hooks run with the lock still held, so a next run cannot rewrite the
    // STATUS a hook is about to read.
    let errors = crate::notify::run_hooks(
        &run.notify,
        &run.shell,
        &run.cwd,
        &run.dir,
        &[
            ("KELIR_JOB", status.job.as_str()),
            ("KELIR_JOB_OUTCOME", status.outcome.as_str()),
            ("KELIR_JOB_RUN", status.run.as_str()),
            ("KELIR_JOB_SESSION", status.session.as_deref().unwrap_or("")),
            (
                "KELIR_JOB_STATUS",
                &crate::util::path_string(&run.dir.join(jobs::STATUS_JSON)),
            ),
        ],
    )
    .await;
    if !errors.is_empty() {
        status.notify_errors = errors;
        let _ = jobs::write_status(&run.dir, &status);
    }
    // The waiter on the job lock can resolve the name the instant that lock is released.
    if !run.session_id.is_empty() {
        crate::names::release_owner(&crate::paths::session_dir(&run.session_id));
    }
    run.guard.release();
    exit
}

/// The deadline's last resort: the loop did not end by itself. Record the
/// timeout, release the lock and leave. Tool-call trees follow their owner.
pub async fn force_timeout() -> ! {
    let code = finalize(None, None).await;
    std::process::exit(i32::from(if code == 0 { EXIT_TIMEOUT } else { code }))
}

// ------------------------------------------------------------ job status

fn live(dir: &Path) -> bool {
    joblock::live_holder(&dir.join(jobs::LOCK)).is_some()
}

fn status_value(status: &Status, dir: &Path) -> Value {
    let mut value = serde_json::to_value(status).unwrap_or(Value::Null);
    let is_live = live(dir);
    if let Some(map) = value.as_object_mut() {
        map.insert("live".into(), json!(is_live));
        // `running` with no process behind it: the run died.
        map.insert(
            "stale".into(),
            json!(status.outcome == "running" && !is_live),
        );
        map.insert(
            "paths".into(),
            json!({
                "status": crate::util::path_string(&dir.join(jobs::STATUS_JSON)),
                "history": crate::util::path_string(&dir.join(jobs::RUNS)),
                "notes": crate::util::path_string(&dir.join(jobs::NOTES)),
            }),
        );
    }
    value
}

pub fn status(args: JobStatusArgs) -> u8 {
    if let Some(name) = &args.name {
        if let Err(why) = names::validate(name) {
            eprintln!("kelir: job: {why}");
            return EXIT_USAGE;
        }
        let dir = jobs::job_dir(name);
        return match jobs::read_status(&dir) {
            Err(message) => {
                eprintln!("kelir: job `{name}`: {message}");
                EXIT_ERROR
            }
            Ok(None) => {
                eprintln!("kelir: job `{name}` has not run (no {})", jobs::STATUS_JSON);
                EXIT_ERROR
            }
            Ok(Some(status)) => {
                if args.json {
                    println!("{}", status_value(&status, &dir));
                } else {
                    print!("{}", jobs::render_md(&status));
                    if status.outcome == "running" && !live(&dir) {
                        println!(
                            "\nnote: this run is marked running but its process is gone; it died \
                             without finishing"
                        );
                    }
                }
                0
            }
        };
    }
    // Every job that has a directory, plus any with a status.
    let mut rows: Vec<(String, Result<Option<Status>, String>)> = Vec::new();
    for entry in std::fs::read_dir(jobs::jobs_dir())
        .into_iter()
        .flatten()
        .flatten()
    {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if names::validate(&name).is_err() || !entry.path().is_dir() {
            continue;
        }
        rows.push((name.clone(), jobs::read_status(&entry.path())));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    if args.json {
        let out: Vec<Value> = rows
            .iter()
            .map(|(name, status)| match status {
                Ok(Some(status)) => status_value(status, &jobs::job_dir(name)),
                Ok(None) => json!({"job": name, "outcome": null}),
                Err(message) => json!({"job": name, "error": message}),
            })
            .collect();
        println!("{}", Value::Array(out));
        return 0;
    }
    if rows.is_empty() {
        println!("no job has run yet");
        return 0;
    }
    println!(
        "{:<20} {:<9} {:<20} {:>9}",
        "JOB", "OUTCOME", "FINISHED", "COST"
    );
    for (name, status) in rows {
        match status {
            Ok(Some(status)) => println!(
                "{:<20} {:<9} {:<20} {:>9}",
                name,
                if status.outcome == "running" && !live(&jobs::job_dir(&name)) {
                    "crashed".to_string()
                } else {
                    status.outcome.clone()
                },
                status
                    .finished
                    .as_deref()
                    .unwrap_or("-")
                    .chars()
                    .take(19)
                    .collect::<String>(),
                status
                    .cost_usd
                    .map_or("-".to_string(), |cost| format!("${cost:.4}"))
            ),
            Ok(None) => println!("{name:<20} {:<9}", "-"),
            Err(message) => println!("{name:<20} unreadable: {message}"),
        }
    }
    0
}

pub fn history(args: JobHistoryArgs) -> u8 {
    if let Err(why) = names::validate(&args.name) {
        eprintln!("kelir: job: {why}");
        return EXIT_USAGE;
    }
    let rows = jobs::history(&jobs::job_dir(&args.name), args.last.max(1));
    if args.json {
        println!("{}", Value::Array(rows));
        return 0;
    }
    if rows.is_empty() {
        println!("job `{}` has no runs yet", args.name);
        return 0;
    }
    println!(
        "{:<30} {:<9} {:<20} {:>5} {:>9}",
        "RUN", "OUTCOME", "FINISHED", "STEPS", "COST"
    );
    for row in rows {
        println!(
            "{:<30} {:<9} {:<20} {:>5} {:>9}",
            row["run"].as_str().unwrap_or("-"),
            row["outcome"].as_str().unwrap_or("-"),
            row["finished"]
                .as_str()
                .unwrap_or("-")
                .chars()
                .take(19)
                .collect::<String>(),
            row["steps"]
                .as_u64()
                .map_or("-".to_string(), |steps| steps.to_string()),
            row["cost_usd"]
                .as_f64()
                .map_or("-".to_string(), |cost| format!("${cost:.4}"))
        );
    }
    0
}

// ------------------------------------------------------------ run --name

/// `kelir run --name <name>`: settle which session the run uses and install the plan.
pub fn plan_for_name(name: &str) -> Result<Option<String>, String> {
    let (resume, plan) = match names::resolve_for_run(name)? {
        Target::Resume(id) => (
            Some(id),
            Plan {
                task_from: Some("job"),
                ..Plan::default()
            },
        ),
        Target::New(id) => (
            None,
            Plan {
                planned_id: Some(id),
                ..Plan::default()
            },
        ),
    };
    if !jobs::install(plan) {
        return Err("a run is already installed in this process".into());
    }
    Ok(resume)
}
