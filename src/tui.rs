//! The inline TUI: a small editor plus a status line, with finished turns
//! pushed into the terminal's scrollback.
//!
//! Two things keep it small. The viewport is fixed ([`VIEWPORT_HEIGHT`] rows at
//! the bottom of the screen) and only it is repainted; everything a turn
//! produces is written *above* it with ratatui's `insert_before`, so the
//! terminal keeps it and the user can scroll back. And every action the TUI
//! takes is a [`crate::commands::Action`], which is one verb sent through the
//! same [`crate::ipc::Incoming`] channel the session socket feeds — the TUI has
//! no private path into the session.

use std::collections::VecDeque;
use std::io::Write as _;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget as _};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::commands::{self, Action, Parsed};
use crate::editor::{Editor, Key, Outcome};
use crate::ipc::{Incoming, Request, Response};
use crate::scrollback::{self, Live};

/// Rows the inline viewport occupies: four live rows, three editor rows and the
/// status line.
pub const VIEWPORT_HEIGHT: u16 = 8;
/// A second Ctrl-C within this window quits instead of stopping.
pub const DOUBLE_CTRL_C: Duration = Duration::from_millis(1000);
/// How much of a call's output the tail view shows.
pub const TAIL_BYTES: usize = 2048;

// ------------------------------------------------------------------ events

/// What the session tells the UI. One direction only: the UI never reaches
/// into the session.
#[derive(Debug, Clone)]
pub enum UiEvent {
    TurnStarted {
        turn: u32,
        provider: String,
        model: String,
    },
    Step {
        step: u32,
        provider: String,
        model: String,
    },
    /// A piece of streamed assistant text.
    Delta(String),
    Usage {
        prompt: u64,
        completion: u64,
        cached: u64,
        cost: f64,
    },
    /// One assistant message is complete: commit the streamed text.
    Assistant {
        text: String,
    },
    ToolStarted {
        name: String,
        input: Value,
    },
    ToolSettled {
        name: String,
        input: Value,
        outcome: String,
        exit_code: Option<i32>,
        bytes: u64,
        lines: u64,
        spilled: bool,
        out_path: String,
    },
    /// Something the session wants said but which is not an error.
    Info(String),
    Error(String),
    /// The answer to a verb the TUI sent.
    Reply(Response),
    TurnEnded {
        outcome: String,
        final_text: String,
    },
    /// The session is gone; draw once more and restore the terminal.
    Quit,
}

// ------------------------------------------------------------------ status

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Working,
    Done,
    Error,
}

impl Phase {
    pub fn word(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Working => "working",
            Phase::Done => "done",
            Phase::Error => "error",
        }
    }
}

/// The status line's data: model, step, tokens, cost, cache ratio, calls.
#[derive(Debug, Clone)]
pub struct Status {
    pub provider: String,
    pub model: String,
    pub turn: u32,
    pub step: u32,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
    pub running: usize,
    pub phase: Phase,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            provider: String::new(),
            model: String::new(),
            turn: 0,
            step: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost: 0.0,
            running: 0,
            phase: Phase::Idle,
        }
    }
}

fn tokens(count: u64) -> String {
    if count < 1000 {
        count.to_string()
    } else {
        format!("{:.1}k", count as f64 / 1000.0)
    }
}

/// The whole status line, as one string (the renderer clips it to the width).
pub fn status_text(status: &Status) -> String {
    let mut parts = vec![format!("kelir {}", status.phase.word())];
    if !status.model.is_empty() {
        parts.push(status.model.clone());
    }
    parts.push(format!("step {}", status.step));
    parts.push(format!(
        "{} tok",
        tokens(status.prompt_tokens + status.completion_tokens)
    ));
    if status.cached_tokens > 0 && status.prompt_tokens > 0 {
        let ratio = 100.0 * status.cached_tokens as f64 / status.prompt_tokens as f64;
        parts.push(format!("{ratio:.0}% cache"));
    }
    if status.cost > 0.0 {
        parts.push(format!("${:.4}", status.cost));
    }
    if status.running > 0 {
        parts.push(format!("{} running", status.running));
    }
    parts.join(" · ")
}

/// Clip to `width` columns, with an ellipsis when something was cut.
pub fn fit(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if scrollback::display_width(text) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for c in text.chars() {
        let w = scrollback::char_width(c);
        if used + w > width.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

// ------------------------------------------------------------------ blocks

/// `$ cargo test` / `edit src/a.rs`: the compact one-line identity of a call.
pub fn call_head(name: &str, input: &Value) -> String {
    match name {
        "bash" => match input.get("command").and_then(Value::as_str) {
            Some(command) => format!("$ {}", one_line(command)),
            None => "$ bash".to_string(),
        },
        "edit" => match input.get("path").and_then(Value::as_str) {
            Some(path) => format!("edit {path}"),
            None => "edit".to_string(),
        },
        other => format!("{other} {}", one_line(&input.to_string())),
    }
}

/// The settled form: the call, its outcome, its size, and whether the model saw
/// a notice instead of the output.
pub fn call_result(
    name: &str,
    input: &Value,
    outcome: &str,
    exit_code: Option<i32>,
    bytes: u64,
    lines: u64,
    spilled: bool,
) -> String {
    let mut parts = Vec::new();
    match outcome {
        "ok" => {
            if let Some(code) = exit_code {
                parts.push(format!("exit {code}"));
            }
        }
        other => parts.push(other.to_string()),
    }
    parts.push(format!("{bytes} B"));
    if lines > 1 {
        parts.push(format!("{lines} lines"));
    }
    if spilled {
        parts.push("spilled".to_string());
    }
    format!("{}  ({})", call_head(name, input), parts.join(" · "))
}

fn one_line(text: &str) -> String {
    crate::util::truncate_chars(&text.replace('\n', " ↵ "), 60)
}

/// The last `max` bytes of a call's output, as text. A tail that starts
/// mid-line begins at the next line, so the view reads as whole lines.
pub fn tail_of(bytes: &[u8], max: usize) -> String {
    let start = bytes.len().saturating_sub(max);
    let text = String::from_utf8_lossy(&bytes[start..]).into_owned();
    if start == 0 {
        return text;
    }
    match text.find('\n') {
        Some(index) => text[index + 1..].to_string(),
        None => text,
    }
}

// --------------------------------------------------------------------- app

pub struct App {
    pub editor: Editor,
    pub live: Live,
    pub status: Status,
    pub width: usize,
    pub height: usize,
    /// Rows ready to be written above the viewport.
    pub scroll: Vec<String>,
    /// The output tail view (Ctrl-O).
    pub tail: VecDeque<String>,
    pub show_tail: bool,
    /// The one-line note in the live area (`$ cmd`, an error, …).
    note: String,
    last_out: Option<String>,
    last_ctrl_c: Option<Instant>,
    pub quit: bool,
    to_tui: mpsc::UnboundedSender<UiEvent>,
    verbs: mpsc::UnboundedSender<Incoming>,
}

impl App {
    pub fn new(
        width: usize,
        height: usize,
        to_tui: mpsc::UnboundedSender<UiEvent>,
        verbs: mpsc::UnboundedSender<Incoming>,
    ) -> Self {
        let width = width.max(1);
        Self {
            editor: Editor::default(),
            live: Live::new(width, live_rows(width, height)),
            status: Status::default(),
            width,
            height,
            scroll: Vec::new(),
            tail: VecDeque::new(),
            show_tail: false,
            note: String::new(),
            last_out: None,
            last_ctrl_c: None,
            quit: false,
            to_tui,
            verbs,
        }
    }

    pub fn set_size(&mut self, width: usize, height: usize) {
        let width = width.max(1);
        if self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        self.live.resize(width, live_rows(width, height));
    }

    /// Rows to write above the viewport, in order: the streamed text that no
    /// longer fits, then anything queued.
    pub fn take_scroll(&mut self) -> Vec<String> {
        let mut rows = self.live.due();
        if !self.scroll.is_empty() {
            rows.append(&mut self.scroll);
        }
        rows
    }

    /// Wrap `text` and queue it for the scrollback.
    pub fn push_text(&mut self, text: &str) {
        for row in scrollback::wrap(text, self.width) {
            self.scroll.push(row);
        }
    }

    /// Commit the streamed message: whatever is left of it, exactly once.
    pub fn commit_live(&mut self, fallback: &str) {
        let mut rows = self.live.due();
        rows.extend(self.live.finish());
        if rows.iter().all(|row| row.is_empty()) && !fallback.trim().is_empty() {
            rows = scrollback::wrap(fallback, self.width);
        }
        if rows.is_empty() {
            return;
        }
        self.scroll.extend(rows);
        self.scroll.push(String::new());
    }

    pub fn apply(&mut self, event: UiEvent) {
        match event {
            UiEvent::TurnStarted {
                turn,
                provider,
                model,
            } => {
                self.status.turn = turn;
                self.status.provider = provider;
                self.status.model = model;
                self.status.phase = Phase::Working;
                self.note.clear();
            }
            UiEvent::Step {
                step,
                provider,
                model,
            } => {
                self.status.step = step;
                self.status.provider = provider;
                self.status.model = model;
            }
            UiEvent::Delta(text) => self.live.push(&text),
            UiEvent::Usage {
                prompt,
                completion,
                cached,
                cost,
            } => {
                self.status.prompt_tokens += prompt;
                self.status.completion_tokens += completion;
                self.status.cached_tokens += cached;
                self.status.cost += cost;
            }
            UiEvent::Assistant { text } => self.commit_live(&text),
            UiEvent::ToolStarted { name, input } => {
                self.status.running += 1;
                self.note = call_head(&name, &input);
            }
            UiEvent::ToolSettled {
                name,
                input,
                outcome,
                exit_code,
                bytes,
                lines,
                spilled,
                out_path,
            } => {
                self.status.running = self.status.running.saturating_sub(1);
                self.note.clear();
                let line = call_result(&name, &input, &outcome, exit_code, bytes, lines, spilled);
                self.push_text(&line);
                if !out_path.is_empty() {
                    self.last_out = Some(out_path);
                }
            }
            UiEvent::Info(text) => self.push_text(&text),
            UiEvent::Error(text) => {
                self.status.phase = Phase::Error;
                self.note = one_line(&text);
                self.push_text(&format!("kelir: {text}"));
            }
            UiEvent::Reply(response) => {
                if response.ok {
                    // A `say`/`later` was already echoed as the user's line.
                    let quiet = matches!(response.verb.as_deref(), Some("say") | Some("later"));
                    if !quiet {
                        let verb = response.verb.unwrap_or_else(|| "verb".into());
                        let applies = response.applies.unwrap_or_else(|| "queued".into());
                        self.push_text(&format!("» {verb}: {applies}"));
                    }
                } else {
                    let message = response
                        .error
                        .unwrap_or_else(|| "the session refused the verb".into());
                    self.push_text(&format!("kelir: {message}"));
                    self.note = one_line(&message);
                }
            }
            UiEvent::TurnEnded { outcome, .. } => {
                self.status.phase = match outcome.as_str() {
                    "done" => Phase::Done,
                    "error" => Phase::Error,
                    _ => Phase::Idle,
                };
                self.status.running = 0;
                self.note.clear();
            }
            UiEvent::Quit => self.quit = true,
        }
    }

    /// One key press.
    pub fn press(&mut self, press: Press) {
        match press {
            Press::Edit(key) => {
                if let Outcome::Submitted(text) = self.editor.handle(key) {
                    self.submit(&text);
                }
            }
            Press::CtrlC => self.ctrl_c(),
            Press::ToggleTail => self.toggle_tail(),
            Press::Redraw => {}
        }
    }

    /// Parse one submitted line and send what it asks for.
    pub fn submit(&mut self, line: &str) {
        match commands::parse(line) {
            Ok(Parsed::Action(action)) => {
                self.push_text(&action.line());
                self.dispatch(&action);
            }
            Ok(Parsed::Note(note)) => self.push_text(&note),
            Err(message) => {
                if !message.is_empty() {
                    self.push_text(&format!("kelir: {message}"));
                }
            }
        }
    }

    /// Send one action through the session's verb channel — the same channel
    /// the socket feeds, logged and answered the same way.
    pub fn dispatch(&mut self, action: &Action) {
        let request = Request::new(action.verb(), action.args(), "tui");
        let (reply, answer) = oneshot::channel();
        if self.verbs.send(Incoming { request, reply }).is_err() {
            // The session is gone; there is nothing left to drive.
            self.quit = true;
            return;
        }
        let to_tui = self.to_tui.clone();
        tokio::spawn(async move {
            if let Ok(response) = answer.await {
                let _ = to_tui.send(UiEvent::Reply(response));
            }
        });
    }

    fn ctrl_c(&mut self) {
        let now = Instant::now();
        let twice = self
            .last_ctrl_c
            .map(|last| now.duration_since(last) <= DOUBLE_CTRL_C)
            .unwrap_or(false);
        if twice {
            self.last_ctrl_c = None;
            self.push_text("» quit (second Ctrl-C)");
            self.dispatch(&Action::Quit);
        } else {
            self.last_ctrl_c = Some(now);
            self.push_text("» stop (Ctrl-C again within a second to quit)");
            self.dispatch(&Action::Stop(None));
        }
    }

    fn toggle_tail(&mut self) {
        if self.show_tail {
            self.show_tail = false;
            return;
        }
        let text = match &self.last_out {
            Some(path) => match std::fs::read(path) {
                Ok(bytes) => tail_of(&bytes, TAIL_BYTES),
                Err(error) => format!("cannot read {path}: {error}"),
            },
            None => "no tool output yet".to_string(),
        };
        self.tail = scrollback::wrap(&text, self.width).into();
        self.show_tail = true;
    }

    pub fn note(&self) -> &str {
        &self.note
    }
}

// ------------------------------------------------------------------- input

/// A key press the TUI acts on (already translated from a terminal event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    Edit(Key),
    CtrlC,
    ToggleTail,
    Redraw,
}

/// Translate one key event. `None` means "nothing to do".
pub fn translate(key: KeyEvent) -> Option<Press> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Char('c') if ctrl => Some(Press::CtrlC),
        KeyCode::Char('o') if ctrl => Some(Press::ToggleTail),
        KeyCode::Char('l') if ctrl => Some(Press::Redraw),
        KeyCode::Char('j') if ctrl => Some(Press::Edit(Key::Newline)),
        KeyCode::Char('k') if ctrl => Some(Press::Edit(Key::KillToEnd)),
        KeyCode::Char('u') if ctrl => Some(Press::Edit(Key::Clear)),
        KeyCode::Char(c) if !ctrl && !alt => Some(Press::Edit(Key::Char(c))),
        // Enter sends; Alt-Enter (and Shift-Enter where the terminal reports
        // it) starts a new line instead.
        KeyCode::Enter if alt || shift => Some(Press::Edit(Key::Newline)),
        KeyCode::Enter => Some(Press::Edit(Key::Enter)),
        KeyCode::Backspace => Some(Press::Edit(Key::Backspace)),
        KeyCode::Delete => Some(Press::Edit(Key::Delete)),
        KeyCode::Left => Some(Press::Edit(Key::Left)),
        KeyCode::Right => Some(Press::Edit(Key::Right)),
        KeyCode::Up => Some(Press::Edit(Key::Up)),
        KeyCode::Down => Some(Press::Edit(Key::Down)),
        KeyCode::Home => Some(Press::Edit(Key::Home)),
        KeyCode::End => Some(Press::Edit(Key::End)),
        _ => None,
    }
}

// ------------------------------------------------------------------ layout

#[derive(Debug, Clone, Copy)]
pub struct Areas {
    pub live: Rect,
    pub editor: Rect,
    pub status: Rect,
}

/// Four live rows, three editor rows, one status row — fewer when the screen
/// is shorter than the viewport (the editor is the last thing to go).
pub fn layout(area: Rect) -> Areas {
    if area.height == 0 {
        return Areas {
            live: area,
            editor: area,
            status: area,
        };
    }
    let status = Rect {
        x: area.x,
        y: area.y + area.height - 1,
        width: area.width,
        height: 1,
    };
    let editor_height = 3.min(area.height.saturating_sub(1));
    let editor = Rect {
        x: area.x,
        y: status.y - editor_height,
        width: area.width,
        height: editor_height,
    };
    let live_height = area.height.saturating_sub(1 + editor_height);
    let live = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: live_height,
    };
    Areas {
        live,
        editor,
        status,
    }
}

fn live_rows(width: usize, height: usize) -> usize {
    let areas = layout(Rect::new(0, 0, width as u16, height as u16));
    (areas.live.height as usize).max(1)
}

// -------------------------------------------------------------------- draw

/// Paint the viewport. Everything else goes through `insert_before`.
pub fn render(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    app.set_size(area.width as usize, area.height as usize);
    let areas = layout(area);
    if app.show_tail {
        render_tail(app, frame, area, areas.status);
    } else {
        render_live(app, frame, areas.live);
        render_editor(app, frame, area, areas.editor);
    }
    render_status(app, frame, areas.status);
}

/// The streamed text (or the note) in the live area.
fn render_live(app: &App, frame: &mut Frame, area: Rect) {
    let view = app.live.view();
    let rows = if view.is_empty() && !app.note.is_empty() {
        vec![app.note.clone()]
    } else {
        view
    };
    let rows = last_rows(&rows, area.height as usize);
    frame.render_widget(
        Paragraph::new(rows.into_iter().map(Line::from).collect::<Vec<_>>()),
        area,
    );
}

/// The editor: the window of rows that keeps the cursor visible, plus the
/// cursor itself, counted in display columns.
fn render_editor(app: &App, frame: &mut Frame, screen: Rect, area: Rect) {
    let (first, visible) = app.editor.window(area.height as usize);
    let lines = app.editor.lines();
    let mut text = Vec::new();
    for (index, line) in lines.iter().enumerate().skip(first).take(visible) {
        let prefix = if index == 0 { "› " } else { "  " };
        text.push(Line::from(format!("{prefix}{line}")));
    }
    frame.render_widget(Paragraph::new(text), area);
    if area.height == 0 {
        return;
    }
    let (row, col) = app.editor.cursor();
    let before: String = lines
        .get(row)
        .map(|line| line.chars().take(col).collect())
        .unwrap_or_default();
    let column = 2 + scrollback::display_width(&before);
    let x = (area.x as usize + column).min(screen.width.saturating_sub(1) as usize) as u16;
    let local = row
        .saturating_sub(first)
        .min(area.height.saturating_sub(1) as usize);
    frame.set_cursor_position(Position::new(x, area.y + local as u16));
}

/// The output tail (Ctrl-O): the whole area above the status line.
fn render_tail(app: &App, frame: &mut Frame, screen: Rect, status: Rect) {
    let area = Rect {
        x: screen.x,
        y: screen.y,
        width: screen.width,
        height: status.y.saturating_sub(screen.y),
    };
    let tail: Vec<String> = app.tail.iter().cloned().collect();
    let rows = last_rows(&tail, area.height as usize);
    frame.render_widget(
        Paragraph::new(rows.into_iter().map(Line::from).collect::<Vec<_>>())
            .style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn render_status(app: &App, frame: &mut Frame, area: Rect) {
    let phase_style = match app.status.phase {
        Phase::Working => Style::default().fg(Color::Cyan),
        Phase::Error => Style::default().fg(Color::Red),
        Phase::Done => Style::default().fg(Color::Green),
        Phase::Idle => Style::default().fg(Color::DarkGray),
    };
    let mut text = status_text(&app.status);
    if app.show_tail {
        text = format!("tail (Ctrl-O closes) · {text}");
    }
    let text = fit(&text, area.width as usize);
    frame.render_widget(
        Paragraph::new(Line::from(text)).style(phase_style.add_modifier(Modifier::BOLD)),
        area,
    );
}

fn last_rows(rows: &[String], height: usize) -> Vec<String> {
    if height == 0 || rows.is_empty() {
        return Vec::new();
    }
    let start = rows.len().saturating_sub(height);
    rows[start..].to_vec()
}

// -------------------------------------------------------------------- loop

/// Everything the run loop needs to paint one inline view.
pub struct Tui {
    pub app: App,
    terminal: ratatui::DefaultTerminal,
    events: mpsc::UnboundedReceiver<UiEvent>,
    keys: mpsc::UnboundedReceiver<Event>,
}

enum Step {
    Ui(UiEvent),
    Key(Event),
}

/// Take the terminal (raw mode, panic hook, inline viewport), the keyboard, and
/// the session's event stream. The caller has already checked that stdout is a
/// terminal.
pub fn start(
    events: mpsc::UnboundedReceiver<UiEvent>,
    to_tui: mpsc::UnboundedSender<UiEvent>,
    verbs: mpsc::UnboundedSender<Incoming>,
    banner: &[String],
) -> std::io::Result<Tui> {
    let (width, height) = ratatui::crossterm::terminal::size().unwrap_or((80, VIEWPORT_HEIGHT));
    let terminal = ratatui::try_init_with_options(ratatui::TerminalOptions {
        viewport: ratatui::Viewport::Inline(VIEWPORT_HEIGHT.min(height)),
    })?;
    let mut app = App::new(
        width as usize,
        height.min(VIEWPORT_HEIGHT) as usize,
        to_tui,
        verbs,
    );
    for line in banner {
        app.push_text(line);
    }
    let (key_tx, keys) = mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        // Blocks until the process exits or the terminal goes away: this read
        // is the only terminal input, and it never runs on the async loop.
        loop {
            match ratatui::crossterm::event::read() {
                Ok(event) => {
                    let keep = match &event {
                        Event::Key(key) => key.kind != KeyEventKind::Release,
                        Event::Paste(_) | Event::Resize(_, _) => true,
                        _ => false,
                    };
                    if keep && key_tx.send(event).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });
    Ok(Tui {
        app,
        terminal,
        events,
        keys,
    })
}

impl Tui {
    /// Draw, then wait for the next key or session event. Returns when the
    /// session says goodbye.
    pub async fn run(mut self) -> std::io::Result<()> {
        loop {
            self.flush_scroll()?;
            self.draw()?;
            if self.app.quit {
                break;
            }
            let step: Option<Step> = tokio::select! {
                event = self.events.recv() => event.map(Step::Ui),
                event = self.keys.recv() => event.map(Step::Key),
            };
            match step {
                Some(Step::Ui(event)) => self.drain(event),
                Some(Step::Key(event)) => {
                    self.on_terminal(event);
                    while let Ok(event) = self.keys.try_recv() {
                        self.on_terminal(event);
                    }
                }
                None => self.app.quit = true,
            }
        }
        self.finish();
        Ok(())
    }

    /// Everything already queued is applied before the next paint, so a burst
    /// of streamed deltas costs one frame, not one frame each.
    fn drain(&mut self, first: UiEvent) {
        self.app.apply(first);
        while let Ok(event) = self.events.try_recv() {
            self.app.apply(event);
        }
        while let Ok(event) = self.keys.try_recv() {
            self.on_terminal(event);
        }
    }

    fn on_terminal(&mut self, event: Event) {
        match event {
            Event::Key(key) => {
                if let Some(press) = translate(key) {
                    self.app.press(press);
                }
            }
            Event::Paste(text) => {
                self.app.press(Press::Edit(Key::Paste(text)));
            }
            Event::Resize(width, _) => {
                let height = self.app.height;
                self.app.set_size(width as usize, height);
            }
            _ => {}
        }
    }

    fn flush_scroll(&mut self) -> std::io::Result<()> {
        let rows = self.app.take_scroll();
        if rows.is_empty() {
            return Ok(());
        }
        let rows = tail_rows(rows);
        let height = rows.len() as u16;
        let lines: Vec<Line> = rows.into_iter().map(Line::from).collect();
        self.terminal.insert_before(height, |buffer| {
            Paragraph::new(lines).render(buffer.area, buffer);
        })
    }

    fn draw(&mut self) -> std::io::Result<()> {
        let app = &mut self.app;
        self.terminal.draw(|frame| render(app, frame))?;
        Ok(())
    }

    /// Leave the terminal usable: raw mode off, and the shell's prompt on a
    /// fresh line below where the viewport was.
    fn finish(&mut self) {
        let _ = ratatui::try_restore();
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\r\n");
        let _ = stdout.flush();
    }
}

fn tail_rows(rows: Vec<String>) -> Vec<String> {
    if rows.len() <= u16::MAX as usize {
        return rows;
    }
    rows[rows.len() - u16::MAX as usize..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(ok: bool, verb: &str) -> Response {
        if ok {
            Response::ok(verb, 3, "queued", json!({}))
        } else {
            Response::error("nothing to stop")
        }
    }

    fn app() -> App {
        app_of_width(60)
    }

    fn app_of_width(width: usize) -> App {
        let (to_tui, _rx) = mpsc::unbounded_channel();
        let (verbs, _vrx) = mpsc::unbounded_channel();
        App::new(width, VIEWPORT_HEIGHT as usize, to_tui, verbs)
    }

    #[test]
    fn the_status_line_names_model_step_tokens_cost_and_calls() {
        let status = Status {
            provider: "openrouter".into(),
            model: "deepseek/deepseek-chat".into(),
            turn: 1,
            step: 3,
            prompt_tokens: 1200,
            completion_tokens: 340,
            cached_tokens: 900,
            cost: 0.00042,
            running: 2,
            phase: Phase::Working,
        };
        assert_eq!(
            status_text(&status),
            "kelir working · deepseek/deepseek-chat · step 3 · 1.5k tok · 75% cache · $0.0004 · 2 running"
        );
        // Nothing measured yet: no cache, no cost, no calls.
        assert_eq!(
            status_text(&Status::default()),
            "kelir idle · step 0 · 0 tok"
        );
    }

    #[test]
    fn a_long_status_line_is_clipped_not_wrapped() {
        let text = fit("kelir working · a-very-long-model-name · step 12", 20);
        assert!(scrollback::display_width(&text) <= 20, "{text}");
        assert!(text.ends_with('…'), "{text}");
        assert_eq!(fit("short", 20), "short");
        assert_eq!(fit("anything", 0), "");
    }

    #[test]
    fn the_viewport_is_four_live_rows_three_editor_rows_and_a_status_line() {
        let areas = layout(Rect::new(0, 0, 80, 8));
        assert_eq!(areas.live, Rect::new(0, 0, 80, 4));
        assert_eq!(areas.editor, Rect::new(0, 4, 80, 3));
        assert_eq!(areas.status, Rect::new(0, 7, 80, 1));
        // A short terminal keeps the status line and squeezes the rest.
        let areas = layout(Rect::new(0, 0, 20, 3));
        assert_eq!(areas.status.height, 1);
        assert_eq!(areas.editor, Rect::new(0, 0, 20, 2));
        assert_eq!(areas.live.height, 0);
    }

    #[test]
    fn a_call_is_one_compact_line() {
        assert_eq!(
            call_head("bash", &json!({"command": "cargo test --all"})),
            "$ cargo test --all"
        );
        assert_eq!(call_head("bash", &json!({"command": "a\nb"})), "$ a ↵ b");
        assert_eq!(
            call_head("edit", &json!({"path": "src/a.rs"})),
            "edit src/a.rs"
        );
        assert_eq!(
            call_result(
                "bash",
                &json!({"command": "ls"}),
                "ok",
                Some(0),
                42,
                3,
                false
            ),
            "$ ls  (exit 0 · 42 B · 3 lines)"
        );
        assert_eq!(
            call_result(
                "bash",
                &json!({"command": "ls"}),
                "stopped",
                None,
                42,
                1,
                false
            ),
            "$ ls  (stopped · 42 B)"
        );
        assert_eq!(
            call_result(
                "bash",
                &json!({"command": "ls"}),
                "ok",
                Some(0),
                12,
                1,
                true
            ),
            "$ ls  (exit 0 · 12 B · spilled)"
        );
    }

    #[test]
    fn a_tail_view_starts_at_a_line_boundary() {
        let bytes = b"one\ntwo\nthree\n";
        assert_eq!(tail_of(bytes, 100), "one\ntwo\nthree\n");
        assert_eq!(tail_of(bytes, 10), "three\n");
        // Not a boundary in sight: the whole (short) tail is shown.
        assert_eq!(tail_of(b"0123456789abcdef", 8), "89abcdef");
    }

    #[test]
    fn streamed_text_is_committed_once_and_the_rest_stays_live() {
        let mut app = app_of_width(12);
        app.apply(UiEvent::TurnStarted {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
        });
        assert_eq!(app.status.phase, Phase::Working);
        let mut typed = String::new();
        for index in 1..=40 {
            let word = format!("w{index}");
            typed.push_str(&word);
            typed.push(' ');
            app.apply(UiEvent::Delta(format!("{word} ")));
        }
        let scrolled = app.take_scroll();
        assert!(!scrolled.is_empty(), "the live area keeps only the tail");
        assert!(!app.live.view().is_empty());
        app.apply(UiEvent::Assistant {
            text: typed.clone(),
        });
        let mut all = scrolled;
        all.extend(app.take_scroll());
        while all.last().is_some_and(String::is_empty) {
            all.pop();
        }
        assert_eq!(all, scrollback::wrap(typed.trim_end(), 12));
    }

    #[test]
    fn tool_events_move_the_running_count_and_leave_a_line() {
        let mut app = app();
        app.apply(UiEvent::ToolStarted {
            name: "bash".into(),
            input: json!({"command": "sleep 1"}),
        });
        assert_eq!(app.status.running, 1);
        assert_eq!(app.note(), "$ sleep 1");
        app.apply(UiEvent::ToolSettled {
            name: "bash".into(),
            input: json!({"command": "sleep 1"}),
            outcome: "ok".into(),
            exit_code: Some(0),
            bytes: 0,
            lines: 0,
            spilled: false,
            out_path: "state/commands/1-1-1/out".into(),
        });
        assert_eq!(app.status.running, 0);
        assert_eq!(app.note(), "");
        assert_eq!(
            app.take_scroll(),
            vec!["$ sleep 1  (exit 0 · 0 B)".to_string()]
        );
        assert!(app.last_out.is_some(), "the tail view has a file to read");
    }

    #[test]
    fn usage_adds_up_across_steps_and_failures_are_errors() {
        let mut app = app();
        for _ in 0..2 {
            app.apply(UiEvent::Usage {
                prompt: 100,
                completion: 10,
                cached: 50,
                cost: 0.001,
            });
        }
        assert_eq!(app.status.prompt_tokens, 200);
        assert!((app.status.cost - 0.002).abs() < 1e-9);
        app.apply(UiEvent::Error("provider said no".into()));
        assert_eq!(app.status.phase, Phase::Error);
        app.apply(UiEvent::TurnEnded {
            outcome: "error".into(),
            final_text: String::new(),
        });
        assert_eq!(app.status.phase, Phase::Error);
        app.apply(UiEvent::TurnEnded {
            outcome: "stopped".into(),
            final_text: String::new(),
        });
        assert_eq!(app.status.phase, Phase::Idle);
    }

    #[test]
    fn a_verb_reply_is_quiet_for_say_and_loud_for_a_refusal() {
        let mut app = app();
        app.apply(UiEvent::Reply(response(true, "say")));
        assert!(app.take_scroll().is_empty(), "a say is already echoed");
        app.apply(UiEvent::Reply(response(true, "model")));
        assert_eq!(app.take_scroll(), vec!["» model: queued".to_string()]);
        app.apply(UiEvent::Reply(response(false, "stop")));
        assert_eq!(
            app.take_scroll(),
            vec!["kelir: nothing to stop".to_string()]
        );
        assert_eq!(app.note(), "nothing to stop");
    }

    #[tokio::test]
    async fn submitting_a_line_sends_the_verb_and_echoes_it() {
        let (to_tui, _rx) = mpsc::unbounded_channel();
        let (verbs, mut vrx) = mpsc::unbounded_channel();
        let mut app = App::new(60, 8, to_tui, verbs);

        app.submit("fix the test");
        let incoming = vrx.try_recv().expect("a say was sent");
        assert_eq!(incoming.request.verb, "say");
        assert_eq!(incoming.request.args, json!({"text": "fix the test"}));
        assert_eq!(incoming.request.from, "tui");
        assert_eq!(app.take_scroll(), vec!["› fix the test".to_string()]);

        app.submit("/stop wrong dir");
        let incoming = vrx.try_recv().expect("a stop was sent");
        assert_eq!(incoming.request.verb, "stop");
        assert_eq!(incoming.request.args, json!({"reason": "wrong dir"}));

        // A local note sends nothing.
        app.submit("/help");
        assert!(vrx.try_recv().is_err());
    }

    #[tokio::test]
    async fn ctrl_c_stops_and_a_second_within_a_second_quits() {
        let (to_tui, _rx) = mpsc::unbounded_channel();
        let (verbs, mut vrx) = mpsc::unbounded_channel();
        let mut app = App::new(60, 8, to_tui, verbs);
        app.press(Press::CtrlC);
        assert_eq!(vrx.try_recv().unwrap().request.verb, "stop");
        app.press(Press::CtrlC);
        assert_eq!(vrx.try_recv().unwrap().request.verb, "end");
        assert!(!app.quit, "quitting waits for the session to end");
    }

    #[test]
    fn keys_translate_to_editing_and_control_actions() {
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        assert_eq!(
            translate(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Press::CtrlC)
        );
        assert_eq!(
            translate(key(KeyCode::Char('x'), KeyModifiers::NONE)),
            Some(Press::Edit(Key::Char('x')))
        );
        assert_eq!(
            translate(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Press::Edit(Key::Enter))
        );
        assert_eq!(
            translate(key(KeyCode::Enter, KeyModifiers::ALT)),
            Some(Press::Edit(Key::Newline))
        );
        assert_eq!(
            translate(key(KeyCode::Enter, KeyModifiers::SHIFT)),
            Some(Press::Edit(Key::Newline))
        );
        assert_eq!(
            translate(key(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            Some(Press::Edit(Key::Newline))
        );
        assert_eq!(
            translate(key(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            Some(Press::ToggleTail)
        );
        assert_eq!(translate(key(KeyCode::Tab, KeyModifiers::NONE)), None);
        let mut release = key(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(translate(release), None);
    }
}
