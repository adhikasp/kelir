//! Replay the event log through the TUI's row model.
//! The same [`crate::tui::UiEvent`] the session sends to the TUI is produced here from a log line,
//! then applied to a real [`crate::tui::App`].
//! That is how the viewer shows the state row, collapsed tool calls, markdown and outcome lines
//! without a second renderer.

use std::collections::VecDeque;

use ratatui::text::Line;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::event::{EventBody, LogEvent};
use crate::ipc::Incoming;
use crate::tui::{App, UiEvent};

/// How many rendered rows the viewer keeps. A 100k-event log still costs this
/// much memory, not a row per event.
pub const BACKLOG_ROWS: usize = 2000;

/// A viewer's state: one App, one bounded scrollback.
pub struct Viewer {
    pub app: App,
    /// The rows already shown (bounded).
    rows: VecDeque<Line<'static>>,
    plain: Vec<String>,
    /// Which events were applied (for tests and diagnostics).
    pub applied: u64,
    width: usize,
    height: usize,
}

impl Viewer {
    pub fn new(width: usize, height: usize) -> Self {
        let (to_tui, _from) = mpsc::unbounded_channel::<UiEvent>();
        let (to_verbs, _incoming) = mpsc::unbounded_channel::<Incoming>();
        let app = App::new(width, height, to_tui, to_verbs, String::new());
        Self {
            app,
            rows: VecDeque::with_capacity(BACKLOG_ROWS),
            plain: Vec::new(),
            applied: 0,
            width,
            height,
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn apply(&mut self, event: &LogEvent) -> Vec<Line<'static>> {
        self.applied += 1;
        for ui in from_event(event) {
            self.app.apply(ui);
        }
        let fresh = self.app.take_scroll();
        for line in &fresh {
            self.rows.push_back(line.clone());
        }
        while self.rows.len() > BACKLOG_ROWS {
            self.rows.pop_front();
        }
        fresh
    }

    pub fn rows(&self) -> Vec<Line<'static>> {
        self.rows.iter().cloned().collect()
    }

    /// The plain text of the backlog: one string per row.
    pub fn plain_lines(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    pub fn push_plain(&mut self, fresh: &[Line<'static>]) {
        for line in fresh {
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            if text.trim().is_empty()
                && self.plain.last().is_some_and(|last| last.trim().is_empty())
            {
                continue;
            }
            self.plain.push(text);
        }
        while self.plain.len() > BACKLOG_ROWS {
            self.plain.remove(0);
        }
    }

    pub fn take_plain(&mut self) -> Vec<String> {
        std::mem::take(&mut self.plain)
    }

    /// The viewer's one-line state, from the same status the TUI uses.
    pub fn status_line(&self) -> String {
        crate::tui::status_text(&self.app.status)
    }
}

/// Convert one log event into the UI events the TUI's row model consumes.
pub fn from_event(event: &LogEvent) -> Vec<UiEvent> {
    let mut out = Vec::new();
    match &event.body {
        EventBody::StepStarted {
            step,
            provider,
            model,
            ..
        } => out.push(UiEvent::Step {
            step: *step,
            provider: provider.clone(),
            model: model.clone(),
            turn_steps: *step,
            limit: None,
        }),
        EventBody::SessionTitle { title } => {
            out.push(UiEvent::Title(title.clone()));
        }
        EventBody::Assistant { text, .. } => {
            let text = payload_text(text);
            if !text.is_empty() {
                out.push(UiEvent::Assistant { text });
            }
        }
        EventBody::Usage {
            prompt_tokens,
            completion_tokens,
            cached_tokens,
            cost,
            ..
        } => out.push(UiEvent::Usage {
            prompt: *prompt_tokens,
            completion: *completion_tokens,
            cached: *cached_tokens,
            cost: *cost,
        }),
        EventBody::ToolStarted { name, input, .. } => out.push(UiEvent::ToolStarted {
            name: name.clone(),
            input: payload_value(input),
        }),
        EventBody::ToolSettled {
            outcome,
            exit_code,
            bytes,
            lines,
            out_path,
            spilled,
            ..
        } => {
            let tail = if *outcome == "ok" && exit_code.unwrap_or(0) == 0 {
                String::new()
            } else {
                read_tail(out_path, 4096).unwrap_or_default()
            };
            out.push(UiEvent::ToolSettled {
                name: String::new(),
                input: Value::Null,
                outcome: outcome.clone(),
                exit_code: *exit_code,
                elapsed_ms: 0,
                bytes: *bytes,
                lines: *lines,
                spilled: *spilled,
                out_path: out_path.clone(),
                tail,
            });
        }
        EventBody::User { text, from, .. } => {
            let text = payload_text(text);
            if !text.is_empty() {
                out.push(UiEvent::Info(format!("› {text}")));
            }
            let _ = from;
        }
        EventBody::Source {
            kind,
            name,
            text,
            path,
            ..
        } => {
            let detail = text
                .as_ref()
                .map(payload_text)
                .or_else(|| path.clone())
                .unwrap_or_default();
            out.push(UiEvent::Info(format!("· {kind} {name}: {detail}")));
        }
        EventBody::TurnEnded {
            outcome,
            final_text,
            ..
        } => out.push(UiEvent::TurnEnded {
            outcome: outcome.clone(),
            final_text: payload_text(final_text),
        }),
        EventBody::Error { message, .. } => out.push(UiEvent::Error(message.clone())),
        EventBody::Warning { message, .. } => out.push(UiEvent::Info(message.clone())),
        EventBody::SessionEnded { reason } => {
            out.push(UiEvent::Info(format!("session ended: {reason}")));
        }
        EventBody::Verb {
            name,
            from,
            boundary,
            error,
            ..
        } => {
            if boundary == "rejected" {
                out.push(UiEvent::Error(
                    error.clone().unwrap_or_else(|| format!("{name} refused")),
                ));
            } else if name != "status" && name != "observe" {
                out.push(UiEvent::Info(format!("» {name} ({from}) · {boundary}")));
            }
        }
        EventBody::BgSettled { bg_id, state, .. } => {
            out.push(UiEvent::Info(format!("job {bg_id} {state}")));
        }
        EventBody::ChildSettled {
            child_id, outcome, ..
        } => {
            out.push(UiEvent::Info(format!("child {child_id} {outcome}")));
        }
        // Compaction, gates and retries have no row in the live TUI either.
        _ => {}
    }
    out
}

fn payload_text(value: &crate::event::Payload) -> String {
    match value {
        crate::event::Payload::Inline(Value::String(text)) => text.clone(),
        crate::event::Payload::Inline(other) => other.to_string(),
        crate::event::Payload::Blob(reference) => reference.preview.clone(),
    }
}

fn payload_value(value: &crate::event::Payload) -> Value {
    match value {
        crate::event::Payload::Inline(inner) => inner.clone(),
        crate::event::Payload::Blob(reference) => {
            serde_json::json!({"path": reference.path, "bytes": reference.bytes})
        }
    }
}

fn read_tail(path: &str, max: usize) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let text = crate::tui::read_tail(std::path::Path::new(path), max).ok()?;
    let line = crate::tui::last_line(&text);
    (!line.is_empty()).then_some(line)
}

/// Parse one line into a log event. Bad lines are skipped, never half-applied.
pub fn parse_line(line: &str) -> Option<LogEvent> {
    serde_json::from_str(line).ok()
}

// The `kelir attach` command: read-only by default, optional token-gated
// steering behind `--steer`.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::AsyncBufReadExt as _;

use crate::cli::AttachArgs;

/// How often the viewer polls for new log lines and socket liveness.
const POLL: Duration = Duration::from_millis(150);
/// Lines rendered per poll. Bounded so a 100k-event log is streamed, not
/// loaded into one `Vec<String>`.
const DRAIN_LINES: usize = 2048;

/// Resolve a session id, prefix, `latest` or an name into a session dir.
fn resolve(spec: &str) -> Result<(String, PathBuf), String> {
    // A name is checked first: it is a small file under `<state>/names/`, and
    // a name can be any of the characters a session id prefix uses.
    if crate::names::validate(spec).is_ok()
        && let Some(entry) = crate::names::read(spec)?
    {
        let dir = crate::paths::session_dir(&entry.session);
        if dir.join("events.jsonl").is_file() || dir.exists() {
            return Ok((entry.session, dir));
        }
    }
    let candidates = crate::paths::session_candidates(spec)?;
    if candidates.len() > 1 {
        return Err(format!(
            "`{spec}` matches {} sessions; give more of the id",
            candidates.len()
        ));
    }
    let id = candidates
        .into_iter()
        .next()
        .ok_or_else(|| format!("no session matches `{spec}`"))?;
    let dir = crate::paths::session_dir(&id);
    Ok((id, dir))
}

pub async fn run(args: AttachArgs) -> u8 {
    // `--steer` only reads stdin inside the follow loop. Refuse the
    // combination up front instead of silently ignoring the flag.
    if args.steer && !args.follow {
        eprintln!(
            "kelir: attach: --steer requires --follow (a replay-only viewer cannot send commands)"
        );
        return 2;
    }
    let (id, dir) = match resolve(&args.session) {
        Ok(found) => found,
        Err(error) => {
            eprintln!("kelir: attach: {error}");
            return 2;
        }
    };
    let log_path = dir.join("events.jsonl");
    if !log_path.is_file() {
        eprintln!(
            "kelir: attach: session {id} has no log at {}",
            log_path.display()
        );
        return 2;
    }
    let (width, height) = terminal_size();
    let mut viewer = Viewer::new(width, height);
    let (mut tail, available) = match crate::tail::Tail::from_last_lines(&log_path, args.since) {
        Ok(found) => found,
        Err(error) => {
            eprintln!("kelir: attach: cannot read the log: {error}");
            return 2;
        }
    };
    let _ = available;

    // Replay what is there now. `--raw` prints the original NDJSON (no row model at all); the
    // other modes feed the same App the TUI uses.
    if drain(&mut tail, &log_path, &mut viewer, args.raw, args.plain).is_none() {
        return 1;
    }
    if !args.follow {
        print_final(&viewer, args.raw, args.plain);
        return 0;
    }

    // `--steer` reads one command per line from stdin. Read-only is the
    // default: without the flag nothing is ever sent over the socket.
    let mut steering = if args.steer {
        Some(tokio::io::BufReader::new(tokio::io::stdin()).lines())
    } else {
        None
    };
    let mut ended = false;
    loop {
        tokio::select! {
            line = async {
                let result: std::io::Result<Option<String>> = match steering.as_mut() {
                    Some(lines) => lines.next_line().await,
                    None => std::future::pending().await,
                };
                result.unwrap_or(None)
            } => {
                match line {
                    Some(command) => steer(&id, &dir, command.trim()),
                    None => steering = None,
                }
            }
            _ = tokio::time::sleep(POLL) => {}
        }
        if let Some(found) = drain(&mut tail, &log_path, &mut viewer, args.raw, args.plain) {
            ended |= found;
        } else {
            return 1;
        }
        // Liveness: an `owner` file with a dead pid means the session exited without writing
        // `session_ended` (a crash). Stop following then: the log has nothing more to say.
        let live = crate::names::holder(&dir).is_some();
        if ended || !live {
            // One last read: events can land between the poll and the liveness
            // check.
            if drain(&mut tail, &log_path, &mut viewer, args.raw, args.plain).is_none() {
                return 1;
            }
            break;
        }
    }
    print_final(&viewer, args.raw, args.plain);
    0
}

/// Read and render every line currently in the log, in bounded pages. The tail cursor
/// advances per page, so memory stays at one page plus the viewer's bounded backlog.
fn drain(
    tail: &mut crate::tail::Tail,
    path: &std::path::Path,
    viewer: &mut Viewer,
    raw: bool,
    plain: bool,
) -> Option<bool> {
    let mut ended = false;
    loop {
        let lines = match tail.poll_capped(path, DRAIN_LINES) {
            Ok(lines) => lines,
            Err(error) => {
                eprintln!("kelir: attach: cannot read the log: {error}");
                return None;
            }
        };
        if lines.is_empty() {
            return Some(ended);
        }
        if !render_lines(viewer, &lines, raw, plain) {
            return None;
        }
        for line in &lines {
            if let Some(event) = crate::attach::parse_line(line)
                && matches!(event.body, crate::event::EventBody::SessionEnded { .. })
            {
                ended = true;
            }
        }
    }
}

fn render_lines(viewer: &mut Viewer, lines: &[String], raw: bool, plain: bool) -> bool {
    let mut out = std::io::stdout().lock();
    for line in lines {
        if raw {
            // The raw event, exactly as it was written. A torn or malformed
            // line is skipped; nothing is invented.
            if crate::attach::parse_line(line).is_none() {
                continue;
            }
            if writeln!(out, "{line}").is_err() {
                return false;
            }
            continue;
        }
        let Some(event) = crate::attach::parse_line(line) else {
            continue;
        };
        let fresh = viewer.apply(&event);
        if plain {
            viewer.push_plain(&fresh);
            for text in viewer.take_plain() {
                if writeln!(out, "{text}").is_err() {
                    return false;
                }
            }
        } else {
            // Styled rows, one per scrollback line, with colour stripped only when the terminal is not a TTY.
            for row in &fresh {
                let text: String = row.spans.iter().map(|span| span.content.as_ref()).collect();
                if writeln!(out, "{text}").is_err() {
                    return false;
                }
            }
        }
    }
    true
}

fn print_final(viewer: &Viewer, raw: bool, plain: bool) {
    if raw || plain {
        return;
    }
    println!("{}", viewer.status_line());
}

fn steer(id: &str, dir: &std::path::Path, command: &str) {
    let mut words = command.split_whitespace();
    let verb = words.next().unwrap_or("");
    if verb.is_empty() || verb.starts_with('#') {
        return;
    }
    if !matches!(verb, "say" | "stop") {
        eprintln!("kelir: attach: only `say <text>` and `stop` may be sent; got `{verb}`");
        return;
    }
    let args = match verb {
        "say" => {
            let text = command.strip_prefix("say").unwrap_or("").trim();
            if text.is_empty() {
                eprintln!("kelir: attach: `say` needs text");
                return;
            }
            serde_json::json!({"text": text})
        }
        _ => serde_json::json!({}),
    };
    let request = crate::ipc::Request::new(verb, args, "attach");
    let dir = dir.to_path_buf();
    let id = id.to_string();
    let verb = verb.to_string();
    tokio::spawn(async move {
        match crate::ipc::request_with_token_file(&dir, &id, &request).await {
            Ok(response) if response.ok => {}
            Ok(response) => eprintln!(
                "kelir: attach: {verb} refused: {}",
                response
                    .error
                    .unwrap_or_else(|| "the session said no".into())
            ),
            Err(error) => eprintln!("kelir: attach: {verb} failed: {error}"),
        }
    });
}

fn terminal_size() -> (usize, usize) {
    let (width, height) = crossterm_width_height();
    (width.clamp(20, 400), height.clamp(5, 200))
}

fn crossterm_width_height() -> (usize, usize) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE,
        };
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
            if GetConsoleScreenBufferInfo(handle, &mut info) != 0 {
                return (
                    (info.srWindow.Right - info.srWindow.Left + 1) as usize,
                    (info.srWindow.Bottom - info.srWindow.Top + 1) as usize,
                );
            }
        }
    }
    #[cfg(unix)]
    {
        if let Ok((width, height)) = ratatui::crossterm::terminal::size() {
            return (width as usize, height as usize);
        }
    }
    (80, 24)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Payload;
    use serde_json::json;

    fn event(body: EventBody) -> LogEvent {
        LogEvent::new(1, body)
    }

    #[test]
    fn tool_calls_use_the_same_head_model_as_the_tui() {
        let input = json!({"command": "cargo test", "description": "run the suite"});
        let head = crate::tui::call_head("bash", &input);
        assert_eq!(head, "run the suite");
        let call = crate::tools_view::CallLine {
            ordinal: 1,
            name: "bash".into(),
            head,
            outcome: "ok".into(),
            exit_code: Some(0),
            bytes: 0,
            lines: 0,
            spilled: false,
            out_path: String::new(),
            tail: String::new(),
            elapsed: Some(std::time::Duration::from_secs(1)),
        };
        let line = call.line(crate::theme::Theme::plain(), 80);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("run the suite"), "{text}");
    }

    #[test]
    fn every_event_kind_maps_without_panicking() {
        let bodies = vec![
            EventBody::User {
                text: Payload::text("hi"),
                from: "cli".into(),
                delivery: "now".into(),
                attachments: Vec::new(),
            },
            EventBody::StepStarted {
                step: 1,
                provider: "p".into(),
                model: "m".into(),
                request_hash: "h".into(),
            },
            EventBody::Assistant {
                step: 1,
                text: Payload::text("hello"),
                reasoning: None,
                tool_calls: Vec::new(),
                finish_reason: None,
            },
            EventBody::Usage {
                step: 1,
                prompt_tokens: 10,
                completion_tokens: 2,
                cached_tokens: 1,
                cache_write_tokens: 0,
                cost: 0.0,
                cost_source: None,
                allowance: None,
                kind: None,
            },
            EventBody::TurnEnded {
                outcome: "done".into(),
                reason: None,
                final_text: Payload::text("done"),
            },
            EventBody::SessionTitle {
                title: "a title".into(),
            },
        ];
        let mut viewer = Viewer::new(80, 24);
        for body in bodies {
            let _ = viewer.apply(&event(body));
        }
        assert_eq!(viewer.applied, 6);
        assert!(viewer.status_line().contains("tok in"));
        assert!(!viewer.plain_lines().is_empty());
    }
}
