//! The M0 loop: fold → request → stream → log → tools → repeat.
//!
//! The session process is the only writer of `events.jsonl`, and every
//! request is built from the fold, never from in-memory state.

use std::io::Write as _;
use std::path::{Path, PathBuf};
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
use crate::provider::{self, Client};
use crate::tools::{self, Ctx, ToolOutcome};

pub async fn run(args: RunArgs) -> u8 {
    drive(Options {
        json: args.json,
        ask: false,
        common: args.common,
    })
    .await
}

pub async fn ask(args: AskArgs) -> u8 {
    drive(Options {
        json: false,
        ask: true,
        common: args.common,
    })
    .await
}

struct Options {
    json: bool,
    ask: bool,
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
    json: bool,
    ask: bool,
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
    final_text: String,
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

    let code = match drive_turn(&mut session).await {
        Ok(()) => {
            let final_text = session.final_text.clone();
            let _ = session.record(EventBody::TurnEnded {
                outcome: "done".into(),
                final_text: final_text.clone().into(),
            });
            let _ = session.record(EventBody::SessionEnded {
                reason: "done".into(),
            });
            session.sync();
            if opts.ask && !final_text.is_empty() {
                println!("{final_text}");
            }
            0
        }
        Err(message) => {
            eprintln!("kelir: {message}");
            let _ = session.record(EventBody::TurnEnded {
                outcome: "error".into(),
                final_text: String::new().into(),
            });
            let _ = session.record(EventBody::SessionEnded {
                reason: "error".into(),
            });
            session.sync();
            1
        }
    };
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

    Ok(Prepared {
        json: opts.json,
        ask: opts.ask,
        cfg,
        provider_name,
        model,
        client,
        id,
        dir,
        task,
        cwd,
        system_text,
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
            final_text: String::new(),
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
        print_result_event(
            if exit_code == 0 { "done" } else { "error" },
            exit_code,
            &self.final_text,
            steps,
            Some((&self.id, &self.dir)),
            None,
        );
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

async fn drive_turn(session: &mut Session) -> Result<(), String> {
    let tool_specs = tools::specs();
    let mut step = 0u32;

    loop {
        step += 1;
        if step > session.cfg.max_steps {
            session.record(EventBody::Error {
                location: "loop".into(),
                message: format!("step limit {} reached", session.cfg.max_steps),
                retryable: false,
            })?;
            return Err(format!("step limit {} reached", session.cfg.max_steps));
        }

        let folded = crate::fold::fold(&session.events, &session.dir)?;
        let messages = folded.request_messages();
        let request = session.client.build(&session.model, &messages, &tool_specs);
        let request_text = serde_json::to_string(&request.body).unwrap_or_default();
        session.record(EventBody::StepStarted {
            step,
            provider: session.provider_name.clone(),
            model: session.model.clone(),
            request_hash: crate::util::fnv1a64_hex(request_text.as_bytes()),
        })?;

        let assembler = stream_with_retry(session, &request).await?;
        let calls = assembler.tool_calls();
        let text = assembler.text.clone();

        session.record(EventBody::Assistant {
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
            session.record(EventBody::Usage {
                step,
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                cached_tokens: usage.cached_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                cost: usage.cost,
                allowance: usage.allowance.clone(),
            })?;
        }
        session.sync();

        if !session.json && session.echo_assistant && !text.is_empty() {
            println!("{text}");
            let _ = std::io::stdout().flush();
        }

        if calls.is_empty() {
            session.final_text = text;
            return Ok(());
        }

        run_tools(session, step, &calls).await?;
    }
}

async fn stream_with_retry(
    session: &mut Session,
    request: &provider::BuiltRequest,
) -> Result<crate::stream::Assembler, String> {
    let mut attempt = 0u32;
    loop {
        match session.client.stream(request).await {
            Ok(assembler) => return Ok(assembler),
            Err(error) => {
                let will_retry = error.retryable && attempt + 1 < provider::MAX_ATTEMPTS;
                session.record(EventBody::Error {
                    location: "provider".into(),
                    message: error.message.clone(),
                    retryable: error.retryable,
                })?;
                if !will_retry {
                    return Err(error.message);
                }
                session.sync();
                tokio::time::sleep(provider::backoff(attempt)).await;
                attempt += 1;
            }
        }
    }
}

async fn run_tools(session: &mut Session, step: u32, calls: &[ToolCall]) -> Result<(), String> {
    // `tool_started` first (intent, in call order), then the output directory
    // name derives from that event's seq: monotonic and filesystem-safe. The
    // provider's call id stays a label in the events.
    let mut planned: Vec<(ToolCall, String)> = Vec::with_capacity(calls.len());
    for (index, call) in calls.iter().enumerate() {
        let started = session.record(EventBody::ToolStarted {
            call_id: call.id.clone(),
            name: call.name.clone(),
            input: Payload::value(call.args.clone()),
            pid: None,
        })?;
        planned.push((
            call.clone(),
            format!("{}-{}-{}", started.seq, step, index + 1),
        ));
    }
    session.sync();

    let ctx = session.ctx.clone();
    let (tx, mut rx) = mpsc::channel::<(usize, (ToolOutcome, Duration))>(planned.len().max(1) * 2);
    let mut handles = Vec::with_capacity(planned.len());
    for (index, (call, dir)) in planned.iter().enumerate() {
        let ctx = ctx.clone();
        let call = call.clone();
        let dir = dir.clone();
        let tx = tx.clone();
        handles.push(tokio::spawn(async move {
            let started = Instant::now();
            let outcome = tools::dispatch(&call.name, &call.args, &ctx, &dir).await;
            let _ = tx.send((index, (outcome, started.elapsed()))).await;
        }));
    }
    drop(tx);

    let mut slots: Vec<Option<(ToolOutcome, Duration)>> =
        (0..planned.len()).map(|_| None).collect();
    while let Some((index, item)) = rx.recv().await {
        if let Some(slot) = slots.get_mut(index) {
            *slot = Some(item);
        }
    }
    for handle in handles {
        let _ = handle.await;
    }

    // Results are committed in call order, never completion order. A call that
    // never settled (a panicked or cancelled task) still gets its event, so the
    // next request is well formed.
    let settled = settle_in_order(slots, |index| {
        (
            ToolOutcome::error(format!(
                "tool task for call {} did not settle (panicked or cancelled)",
                index + 1
            )),
            Duration::ZERO,
        )
    });

    for ((call, _dir), (outcome, elapsed)) in planned.iter().zip(settled) {
        session.record(EventBody::ToolSettled {
            call_id: call.id.clone(),
            outcome: outcome.outcome.clone(),
            exit_code: outcome.exit_code,
            bytes: outcome.bytes,
            lines: outcome.lines,
            out_path: outcome.out_path.clone(),
            spilled: outcome.spilled,
            result: outcome.result.clone().into(),
        })?;
        if !session.json {
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
    session.sync();
    Ok(())
}

/// Put finished calls back in call order and give every empty slot an error.
pub fn settle_in_order<T>(slots: Vec<Option<T>>, make_error: impl Fn(usize) -> T) -> Vec<T> {
    slots
        .into_iter()
        .enumerate()
        .map(|(index, slot)| slot.unwrap_or_else(|| make_error(index)))
        .collect()
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
    fn settled_results_are_given_in_call_order() {
        // The second call finished first; the third never settled.
        let slots = vec![Some("first".to_string()), None, Some("third".to_string())];
        let settled = settle_in_order(slots, |index| format!("error {index}"));
        assert_eq!(
            settled,
            vec![
                "first".to_string(),
                "error 1".to_string(),
                "third".to_string()
            ]
        );
    }

    #[test]
    fn every_call_settles_even_when_the_batch_is_empty() {
        let settled: Vec<u8> = settle_in_order(Vec::new(), |_| 0);
        assert!(settled.is_empty());
    }

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
}
