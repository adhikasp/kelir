//! The TUI's state. Every row on screen comes from the events applied here;
//! the transcript is owned by the app and rewrapped on resize, so the terminal
//! never drops or reflows a prompt or an answer.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use ratatui::text::Line;
use tokio::sync::{mpsc, oneshot};

use crate::activity::{self, Activity, Kind};
use crate::attachments;
use crate::commands::{self, Action, Parsed};
use crate::editor::{Chip, Editor, Key, Outcome};
use crate::herdr;
use crate::ipc::{Incoming, Request};
use crate::markdown::Stream;
use crate::theme::Theme;
use crate::tools_view::{self, CallLine};

use super::keys::Press;
use super::view;

use super::{Live, Phase, Status, View};

/// One transcript item. Source text is kept so a resize can reflow every row
/// from the event that made it: nothing is lost and nothing is written twice.
#[derive(Debug, Clone)]
pub enum Entry {
    /// Assistant prose, rendered as markdown at the current width.
    Markdown(String),
    /// The human's own words, in the user colour.
    User(String),
    Plain(String),
    Dim(String),
    Error(String),
    /// A settled slice of a long stream: source text plus its rows at `width`.
    /// Re-rendering on every delta would be quadratic; resize re-renders once.
    Settled {
        text: String,
        width: usize,
        lines: Vec<Line<'static>>,
    },
    /// A settled tool call (one line plus a failure tail).
    Call(CallLine),
}

impl Entry {
    fn text(&self) -> String {
        match self {
            Self::Markdown(text)
            | Self::User(text)
            | Self::Plain(text)
            | Self::Dim(text)
            | Self::Error(text) => text.clone(),
            Self::Settled { text, .. } => text.clone(),
            Self::Call(call) => call.head.clone(),
        }
    }
}

pub struct App {
    pub editor: Editor,
    pub theme: Theme,
    pub activity: Activity,
    pub status: Status,
    pub width: usize,
    pub height: usize,
    /// The transcript's source entries, in event order.
    entries: Vec<Entry>,
    /// The entries rendered at `width`: the viewport is a window over these rows.
    pub cached_rows: Vec<Line<'static>>,
    /// Rows of the block still streaming, rendered from `draft`. They replace
    /// the last markdown entry until the assistant message settles.
    draft: String,
    draft_entry: Option<usize>,
    /// The markdown splitter for the block currently streaming.
    stream: Stream,
    /// The transcript's top row; `usize::MAX` means "follow the bottom".
    scroll: Option<usize>,
    /// Rows added since the reader scrolled away from the bottom.
    pub pending_new: usize,
    /// How many rows `take_scroll` has already returned.
    taken_rows: usize,
    pub tail: VecDeque<String>,
    pub show_tail: bool,
    pub call_ordinal: u64,
    calls: HashMap<u64, CallLine>,
    /// The call intents already painted, keyed by id, so a settlement can reuse their names.
    call_starts: HashMap<String, (String, serde_json::Value)>,
    /// User prompts echoed locally and not yet matched to their log event.
    /// Matched by text, not by position: a gate or child message between the
    /// echo and its log event must not swallow the wrong row.
    own_user_echoes: Vec<String>,
    turn_steps: u32,
    turn_tokens: u64,
    /// Index of the paused/error summary row; it belongs to one turn only.
    summary_entry: Option<usize>,
    pub frame: u64,
    seen_images: HashMap<u64, String>,
    pub session_id: Option<String>,
    pub session_dir: Option<PathBuf>,
    absorbed: std::collections::HashSet<String>,
    pub note: String,
    retry_deadline: Option<Instant>,
    last_out: Option<String>,
    last_ctrl_c: Option<Instant>,
    status_records: Vec<String>,
    pub quit: bool,
    to_tui: mpsc::UnboundedSender<View>,
    verbs: mpsc::UnboundedSender<Incoming>,
    token: String,
}

impl App {
    pub fn new(
        width: usize,
        height: usize,
        to_tui: mpsc::UnboundedSender<View>,
        verbs: mpsc::UnboundedSender<Incoming>,
        token: String,
    ) -> Self {
        Self {
            editor: Editor::default(),
            theme: Theme::plain(),
            activity: Activity::idle(Instant::now()),
            status: Status::default(),
            width: width.max(1),
            height,
            entries: Vec::new(),
            cached_rows: Vec::new(),
            draft: String::new(),
            draft_entry: None,
            stream: Stream::new(),
            scroll: None,
            pending_new: 0,
            taken_rows: 0,
            tail: VecDeque::new(),
            show_tail: false,
            call_ordinal: 0,
            calls: HashMap::new(),
            call_starts: HashMap::new(),
            own_user_echoes: Vec::new(),
            turn_steps: 0,
            turn_tokens: 0,
            summary_entry: None,
            frame: 0,
            seen_images: HashMap::new(),
            session_id: None,
            session_dir: None,
            absorbed: std::collections::HashSet::new(),
            note: String::new(),
            retry_deadline: None,
            last_out: None,
            last_ctrl_c: None,
            status_records: Vec::new(),
            quit: false,
            to_tui,
            verbs,
            token,
        }
    }

    pub fn set_session_dir(&mut self, dir: PathBuf) {
        self.session_dir = Some(dir);
    }

    pub fn set_session_id(&mut self, id: String) {
        self.session_id = Some(id);
    }

    /// Set the title from the session log (a resumed session already has one).
    pub fn set_title(&mut self, title: String) {
        self.status.title = title.clone();
        self.status_records.push(herdr::title_record(&title));
    }

    pub fn theme(&self) -> Theme {
        self.theme
    }

    pub fn note(&self) -> &str {
        &self.note
    }

    pub fn take_status_records(&mut self) -> Vec<String> {
        std::mem::take(&mut self.status_records)
    }

    /// The view is owned now: a resize rebuilds every row from its entry and
    /// keeps the reader where they were. The first visible row is mapped from
    /// the old row count to the new one, so a wider (shorter) window neither
    /// clears a deliberate scroll nor leaves a stale absolute top.
    pub fn set_size(&mut self, width: usize, height: usize) {
        let width = width.max(1);
        if self.width == width && self.height == height {
            return;
        }
        let max = self
            .cached_rows
            .len()
            .saturating_sub(self.viewport_height());
        let followed = self.scroll.is_none();
        let first = if followed {
            None
        } else {
            Some(self.scroll.unwrap_or(0).min(max))
        };
        self.width = width;
        self.height = height;
        self.rewrap();
        if let Some(top) = first {
            let new_max = self
                .cached_rows
                .len()
                .saturating_sub(self.viewport_height());
            // Keep the same fraction of the transcript visible, not the same
            // numeric row: a narrower width produces more rows.
            let fraction = if max == 0 {
                0.0
            } else {
                top as f64 / max as f64
            };
            self.scroll = Some((fraction * new_max as f64) as usize);
        }
    }

    /// Rebuild every rendered row at the current width. The source entries are
    /// untouched, so this can be called on every resize without drift.
    pub fn rewrap(&mut self) {
        let taken = self.taken_rows;
        let mut rows = Vec::new();
        for entry in &self.entries {
            rows.extend(view::entry_lines(self, entry, self.width));
        }
        self.cached_rows = rows;
        self.taken_rows = taken.min(self.cached_rows.len());
    }
    fn append(&mut self, entry: Entry) {
        let was_scrolled_away = self.scroll.is_some() && !self.at_bottom(usize::MAX, self.height);
        if !was_scrolled_away {
            self.scroll = None;
            self.pending_new = 0;
        } else {
            self.pending_new += view::entry_lines(self, &entry, self.width).len();
        }
        self.entries.push(entry);
        let lines = view::entry_lines(self, self.entries.last().unwrap(), self.width);
        self.cached_rows.extend(lines);
    }

    /// Is the viewport showing its bottom row? `rows` may be `usize::MAX` to
    /// ask about the underlying transcript without a concrete viewport.
    pub fn at_bottom(&self, rows: usize, viewport: usize) -> bool {
        match self.scroll {
            None => true,
            Some(top) => {
                let rows = if rows == usize::MAX {
                    self.cached_rows.len()
                } else {
                    rows
                };
                top + viewport >= rows
            }
        }
    }

    /// The top row ratatui should show. Returns `0` when following the tail,
    /// so `Paragraph::scroll` does the work at its own viewport height.
    pub fn scroll_top(&self, rows: usize, viewport: usize) -> u16 {
        let Some(top) = self.scroll else {
            return rows.saturating_sub(viewport).min(u16::MAX as usize) as u16;
        };
        top.min(rows.saturating_sub(1)).min(u16::MAX as usize) as u16
    }

    /// The current logical scroll position, for tests and key handling.
    pub fn scroll_position(&self) -> Option<usize> {
        self.scroll
    }

    fn scroll_by(&mut self, delta: isize, viewport: usize) {
        let rows = self.cached_rows.len();
        let max = rows.saturating_sub(viewport);
        let current = self.scroll.unwrap_or(max);
        let next = (current as isize + delta).clamp(0, max as isize) as usize;
        self.scroll = (next < max).then_some(next);
        if next >= max {
            self.scroll = None;
            self.pending_new = 0;
        }
    }

    fn scroll_to(&mut self, top: usize, viewport: usize) {
        let max = self.cached_rows.len().saturating_sub(viewport);
        if top >= max {
            self.scroll = None;
            self.pending_new = 0;
        } else {
            self.scroll = Some(top);
        }
    }

    pub fn viewport_height(&self) -> usize {
        self.height.saturating_sub(2)
    }

    /// Compatible with `attach` and the row-level tests: the fresh rows since
    /// the last take, not a destructive read of the whole transcript.
    pub fn take_scroll(&mut self) -> Vec<Line<'static>> {
        let start = self.taken_rows.min(self.cached_rows.len());
        self.taken_rows = self.cached_rows.len();
        self.cached_rows[start..].to_vec()
    }

    /// The whole transcript as plain text, for export.
    pub fn transcript_text(&self) -> String {
        view::plain_text(&self.cached_rows)
    }

    /// The text of the last answer, for Ctrl-Y's OSC 52 copy.
    pub fn last_answer(&self) -> String {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| match entry {
                Entry::Markdown(text) if !text.trim().is_empty() => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Dump the transcript to a file in the session dir, returning its path.
    pub fn dump_transcript(&mut self) -> Option<PathBuf> {
        let dir = self.session_dir.clone()?;
        let path = dir.join("transcript.txt");
        std::fs::write(&path, self.transcript_text()).ok()?;
        self.note = format!("transcript saved: {}", path.display());
        Some(path)
    }

    /// The rows of the block still streaming, rendered from the draft text.
    pub fn live_view(&self) -> Vec<crate::markdown::Row> {
        if self.draft.trim().is_empty() {
            return Vec::new();
        }
        let layout = crate::markdown::Layout::with_glyphs(self.width, self.theme.glyphs);
        let mut rows = crate::markdown::open_rows(&self.draft, None, self.draft.len(), layout);
        if rows.is_empty() {
            rows = crate::markdown::render(&self.draft, layout);
        }
        rows
    }

    pub fn live_lines(&self) -> Vec<Line<'static>> {
        self.live_view()
            .iter()
            .map(|row| row.line(self.theme, &crate::markdown::NoHighlight))
            .collect()
    }

    pub fn wanted_live_rows(&self) -> u16 {
        self.live_view().len().min(3) as u16
    }

    pub fn push_text(&mut self, text: &str) {
        for line in text.lines() {
            self.append(Entry::Plain(line.to_string()));
        }
    }

    pub fn push_dim_text(&mut self, text: &str) {
        for line in text.lines() {
            self.append(Entry::Dim(line.to_string()));
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.own_user_echoes.push(text.to_string());
        self.push_user_row(text);
    }

    /// A user row from the log: unlike a local echo it must not raise the
    /// counter that suppresses the log copy (attach has no local echo).
    fn push_user_row(&mut self, text: &str) {
        for line in text.lines() {
            self.append(Entry::User(line.to_string()));
        }
    }

    pub fn push_lines(&mut self, lines: Vec<Line<'static>>) {
        for line in lines {
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            self.append(Entry::Dim(text));
        }
    }

    /// One blank row between two visual blocks, and never more than one.
    fn separate(&mut self) {
        if let Some(last) = self.entries.last()
            && !last.text().trim().is_empty()
        {
            self.append(Entry::Plain(String::new()));
        }
    }

    fn emit_markdown(&mut self, text: &str) {
        // The stream's tail was rendered as a draft entry; the finished text
        // replaces it rather than following it (or the answer appears twice).
        self.clear_draft();
        self.separate();
        let index = self.entries.len();
        self.entries.push(Entry::Markdown(text.to_string()));
        let lines = view::entry_lines(self, &self.entries[index], self.width);
        if self.scroll.is_some() {
            self.pending_new += lines.len();
        }
        self.cached_rows.extend(lines);
        self.draft.clear();
    }

    /// Apply one view event: a durable log fact or the few live-only facts.
    pub fn apply(&mut self, view: View) {
        match view {
            View::Live(live) => self.apply_live(live),
            View::Log(event) => self.apply_log(&event),
        }
    }

    fn apply_live(&mut self, live: Live) {
        let now = Instant::now();
        match live {
            Live::Delta(text) => {
                self.status.phase = Phase::Working;
                self.activity.text(now, text.chars().count());
                self.clear_draft();
                let settled_bytes = self.stream.delivered_bytes();
                for closed in self.stream.push_blocks(&text) {
                    if !closed.text.trim().is_empty() {
                        let remainder = closed.text.get(settled_bytes..).unwrap_or("");
                        if !remainder.trim().is_empty() {
                            self.separate();
                            self.entries.push(Entry::Markdown(remainder.to_string()));
                            let lines =
                                view::entry_lines(self, self.entries.last().unwrap(), self.width);
                            self.cached_rows.extend(lines);
                        }
                    }
                }
                let layout = crate::markdown::Layout::with_glyphs(self.width, self.theme.glyphs);
                let from = self.stream.delivered_bytes();
                let settled = self.stream.settle(layout, 3);
                if !settled.is_empty() {
                    let to = self.stream.delivered_bytes();
                    let text = self.stream.open().get(from..to).unwrap_or("").to_string();
                    let lines: Vec<Line<'static>> = settled
                        .iter()
                        .map(|row| row.line(self.theme, &crate::markdown::NoHighlight))
                        .collect();
                    self.pending_new += lines.len();
                    self.cached_rows.extend(lines.iter().cloned());
                    self.taken_rows = self.cached_rows.len();
                    self.entries.push(Entry::Settled {
                        text,
                        width: self.width,
                        lines,
                    });
                }
                let draft = self.stream.tail_remaining();
                self.set_draft(draft);
            }
            Live::Retrying {
                line,
                until,
                status,
                attempt,
            } => {
                let wait = until.saturating_duration_since(now);
                if self.activity.kind == Kind::RateLimited {
                    self.activity.tick_retry(wait);
                } else if status.is_some() || attempt > 0 {
                    let detail = status.map(|code| code.to_string()).unwrap_or_default();
                    self.activity.retry(now, wait, attempt, &detail);
                }
                if status.is_some() || attempt > 0 {
                    self.retry_deadline = Some(until);
                }
                self.status.retrying = Some((view::tui_wait_line(&line), until));
                self.note.clear();
            }
            Live::RetryingDone => {
                self.status.retrying = None;
                self.retry_deadline = None;
                self.note.clear();
                self.activity.retry_done(now);
            }
            Live::Reply(response) => {
                if response.ok {
                    let quiet = matches!(response.verb.as_deref(), Some("say") | Some("later"))
                        || (response.verb.as_deref() == Some("stop")
                            && response.applies.as_deref() == Some("dropped"));
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
                    self.note = crate::util::one_line_visible(&message, 60);
                }
            }
            Live::Herdr { state } => self.status_records.push(herdr::record(state)),
            Live::Quit => self.quit = true,
        }
    }

    fn apply_log(&mut self, event: &crate::event::LogEvent) {
        use crate::event::EventBody;
        let now = Instant::now();
        match &event.body {
            EventBody::SessionStarted {
                provider, model, ..
            } => {
                self.status.provider = provider.clone();
                self.status.model = model.clone();
            }
            EventBody::User { text, .. } => {
                let text = payload_text(text);
                if let Some(index) = self.own_user_echoes.iter().position(|echo| echo == &text) {
                    self.own_user_echoes.remove(index);
                } else if !text.is_empty() {
                    self.push_user_row(&text);
                }
            }
            EventBody::StepStarted {
                step,
                provider,
                model,
                ..
            } => {
                self.status.step = *step;
                self.status.provider = provider.clone();
                self.status.model = model.clone();
                self.status.turn_steps += 1;
                self.turn_steps = self.status.turn_steps;
                if let Some(index) = self.summary_entry.take()
                    && index < self.entries.len()
                {
                    self.entries.remove(index);
                    self.rewrap();
                }
                self.status.phase = Phase::Working;
                self.activity.step_started(now);
                self.retry_deadline = None;
            }
            EventBody::Assistant { text, usage, .. } => {
                let streamed = self.stream.finish().unwrap_or_default();
                let text = payload_text(text);
                let text = if streamed.trim().is_empty() {
                    text
                } else {
                    streamed
                };
                if !text.trim().is_empty() {
                    self.emit_markdown(&text);
                }
                self.stream = Stream::new();
                if let Some(usage) = usage {
                    self.add_usage(usage);
                }
            }
            EventBody::StepUsage(usage) => self.add_usage(usage),
            EventBody::CallStarted(start) if start.kind == "tool" => {
                let streamed = self.stream.finish().unwrap_or_default();
                if !streamed.trim().is_empty() {
                    self.emit_markdown(&streamed);
                }
                let input = start
                    .info
                    .input
                    .as_ref()
                    .map(payload_value)
                    .unwrap_or(serde_json::Value::Null);
                self.call_starts
                    .insert(start.id.clone(), (start.info.name.clone(), input.clone()));
                self.status.running += 1;
                self.activity
                    .tool_started(now, &view::call_label(&start.info.name, &input));
            }
            EventBody::CallSettled(settle) => {
                let (name, input) = self
                    .call_starts
                    .remove(&settle.id)
                    .unwrap_or_else(|| (settle.kind.clone(), serde_json::Value::Null));
                self.status.running = self.status.running.saturating_sub(1);
                self.activity.tool_settled(now);
                self.call_ordinal += 1;
                let tail = settle.tail.clone().unwrap_or_else(|| {
                    settle
                        .result
                        .as_ref()
                        .map(|value| crate::tui::last_line(&payload_text(value)))
                        .unwrap_or_default()
                });
                let call = CallLine {
                    ordinal: self.call_ordinal,
                    name: name.clone(),
                    head: view::call_head(&name, &input),
                    outcome: settle.outcome.clone(),
                    exit_code: settle.exit_code,
                    bytes: settle.bytes.unwrap_or_default(),
                    lines: settle.lines.unwrap_or_default(),
                    spilled: settle.spilled.unwrap_or_default(),
                    out_path: settle.out_path.clone().unwrap_or_default(),
                    tail,
                    elapsed: None,
                };
                self.last_out = Some(call.out_path.clone()).filter(|path| !path.is_empty());
                self.calls.insert(call.ordinal, call.clone());
                self.append(Entry::Call(call));
            }
            EventBody::TurnEnded {
                outcome,
                final_text,
                ..
            } => {
                let streamed = self.stream.finish().unwrap_or_default();
                if !streamed.trim().is_empty() {
                    self.emit_markdown(&streamed);
                }
                self.stream = Stream::new();
                self.activity.turn_ended(now, outcome);
                let final_text = payload_text(final_text);
                if outcome == "paused"
                    && let Some(reason) = view::pause_reason(&final_text)
                {
                    self.activity.paused(now, &reason);
                }
                self.status.phase = match outcome.as_str() {
                    "done" => Phase::Done,
                    "paused" => Phase::Paused,
                    "error" => Phase::Error,
                    _ => Phase::Idle,
                };
                self.status.running = 0;
                self.status.retrying = None;
                self.note.clear();
                self.turn_summary(outcome);
            }
            EventBody::SessionTitle { title } => self.set_title(title.clone()),
            EventBody::Error { message, .. } => {
                self.status.phase = Phase::Error;
                self.activity
                    .failed(now, &crate::util::one_line_visible(message, 60));
                self.append(Entry::Error(format!("kelir: {message}")));
            }
            EventBody::Note(note) => {
                let text = if note.text.contains("say stop") {
                    view::tui_wait_line(&note.text)
                } else {
                    note.text.clone()
                };
                self.push_text(&text);
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
                self.push_text(&format!("· {kind} {name}: {detail}"));
            }
            EventBody::Verb {
                name,
                from,
                boundary,
                error,
                ..
            } => {
                if boundary == "rejected" {
                    let message = error.clone().unwrap_or_else(|| format!("{name} refused"));
                    self.status.phase = Phase::Error;
                    self.activity
                        .failed(now, &crate::util::one_line_visible(&message, 60));
                    self.append(Entry::Error(format!("kelir: {message}")));
                } else if name != "status" && name != "observe" {
                    self.push_text(&format!("» {name} ({from}) · {boundary}"));
                }
            }
            EventBody::SessionEnded { reason } => {
                self.push_text(&format!("session ended: {reason}"));
            }
            _ => {}
        }
    }

    fn add_usage(&mut self, usage: &crate::event::Usage) {
        self.status.prompt_tokens += usage.prompt_tokens;
        self.status.last_prompt_tokens = usage.prompt_tokens;
        self.status.completion_tokens += usage.completion_tokens;
        self.status.cached_tokens += usage.cached_tokens;
        self.turn_tokens += usage.prompt_tokens + usage.completion_tokens;
    }

    /// The one line for turns that need a human: nothing after a `done` turn.
    fn turn_summary(&mut self, outcome: &str) {
        if outcome != "done" {
            self.separate();
            let steps = self.turn_steps.max(self.status.turn_steps);
            let summary = activity::summary_line(self.theme, outcome, steps, self.turn_tokens);
            let text: String = summary
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            self.append(Entry::Dim(text));
            self.summary_entry = Some(self.entries.len().saturating_sub(1));
        }
        self.turn_steps = 0;
        self.turn_tokens = 0;
    }

    fn clear_draft(&mut self) {
        if let Some(index) = self.draft_entry.take()
            && index < self.entries.len()
        {
            let old_draft = std::mem::take(&mut self.draft);
            let rows = view::entry_lines(self, &Entry::Markdown(old_draft), self.width).len();
            self.entries.remove(index);
            let keep = self.cached_rows.len().saturating_sub(rows);
            self.cached_rows.truncate(keep);
        }
        self.draft.clear();
    }

    fn set_draft(&mut self, text: String) {
        if self.draft == text && self.draft_entry.is_some() {
            return;
        }
        self.clear_draft();
        if !text.trim().is_empty() {
            self.separate();
            self.draft_entry = Some(self.entries.len());
            self.entries.push(Entry::Markdown(text.clone()));
            let rows =
                view::entry_lines(self, &self.entries[self.draft_entry.unwrap()], self.width);
            self.cached_rows.extend(rows);
        }
        self.draft = text;
    }

    pub fn press(&mut self, press: Press) {
        match press {
            Press::Edit(key) => {
                let typed = matches!(key, Key::Char(c) if !c.is_control());
                match self.editor.handle(key) {
                    Outcome::Submitted(text) => self.submit(&text),
                    Outcome::ChipRemoved(chip) => self.discard_chip(&chip),
                    Outcome::Edited | Outcome::Ignored => {
                        if typed {
                            self.adopt_typed_image();
                        }
                    }
                }
            }
            Press::Backspace => match self.editor.handle(Key::RemoveChip) {
                Outcome::ChipRemoved(chip) => self.discard_chip(&chip),
                _ => match self.editor.handle(Key::Backspace) {
                    Outcome::ChipRemoved(chip) => self.discard_chip(&chip),
                    Outcome::Submitted(text) => self.submit(&text),
                    Outcome::Edited | Outcome::Ignored => {}
                },
            },
            Press::CtrlC => self.ctrl_c(),
            Press::ToggleTail => self.toggle_tail(),
            Press::Stop => {
                if self.activity.kind.working() {
                    self.push_text("» stop (Esc)");
                    self.dispatch(&Action::Stop(None));
                }
            }
            Press::Redraw => {}
            Press::PageUp => {
                let viewport = self.viewport_height();
                self.scroll_by(-(viewport.max(1) as isize), viewport);
            }
            Press::PageDown => {
                let viewport = self.viewport_height();
                self.scroll_by(viewport.max(1) as isize, viewport);
            }
            Press::Top => self.scroll_to(0, self.viewport_height()),
            Press::Bottom => {
                self.scroll = None;
                self.pending_new = 0;
            }
            Press::CopyLast => {
                let answer = self.last_answer();
                if !answer.is_empty() {
                    let encoded = osc52(&answer);
                    self.status_records.push(encoded);
                    self.note = "last answer copied".to_string();
                } else {
                    self.note = "no answer yet".to_string();
                }
            }
            Press::DumpTranscript => {
                if self.dump_transcript().is_none() {
                    self.note = "no session dir to save the transcript".to_string();
                }
            }
        }
    }

    /// Take the image path that was just typed at the cursor and turn it into
    /// a chip: the file is saved into the session, the path text removed.
    fn adopt_typed_image(&mut self) {
        if self.session_dir.is_none() {
            return;
        }
        let Some(line) = self.editor.lines().get(self.editor.cursor().0) else {
            return;
        };
        let col = self.editor.cursor().1;
        let typed: String = line.chars().take(col).collect();
        let Some(candidate) = attachments::path_tail(typed.trim_end()) else {
            return;
        };
        let Some(paste) = attachments::pasted_image(&candidate) else {
            return;
        };
        if self.absorbed.contains(&candidate) {
            return;
        }
        let Some(saved) = self.save_image(&paste.path) else {
            return;
        };
        if self.editor.take_word_before_cursor().is_none() {
            return;
        }
        self.absorbed.insert(candidate);
        let chip = Chip::new(saved.seq, &saved.rel, &saved.pointer);
        self.press(Press::Edit(Key::Chip(chip)));
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(paste) = attachments::pasted_image(text)
            && let Some(saved) = self.save_image(&paste.path)
        {
            self.press(Press::Edit(Key::Chip(Chip::new(
                saved.seq,
                &saved.rel,
                &saved.pointer,
            ))));
            return;
        }
        self.press(Press::Edit(Key::Paste(text.to_string())));
    }

    fn save_image(&mut self, path: &std::path::Path) -> Option<attachments::Saved> {
        let dir = self.session_dir.clone()?;
        if self.editor.chips().len() >= attachments::MAX_PER_PROMPT {
            self.note = format!("at most {} images per prompt", attachments::MAX_PER_PROMPT);
            return None;
        }
        let seq = (self.editor.chips().len() as u32 + 1).max(1);
        match attachments::save(&dir, seq, path, &mut self.seen_images) {
            Ok(saved) => Some(saved),
            Err(message) => {
                self.note = crate::util::one_line_visible(&message, 60);
                None
            }
        }
    }

    fn discard_chip(&mut self, chip: &Chip) {
        if self
            .editor
            .chips()
            .iter()
            .any(|other| other.rel == chip.rel)
        {
            return;
        }
        if let Some(dir) = &self.session_dir {
            let path = dir.join(&chip.rel);
            let _ = std::fs::remove_file(path);
            self.seen_images.retain(|_, rel| rel != &chip.rel);
            let stem = chip
                .rel
                .rsplit('/')
                .next()
                .and_then(|name| name.split_once('-'))
                .map(|(_, rest)| rest.to_string())
                .unwrap_or_default();
            self.absorbed.retain(|text| {
                !text.contains(&chip.rel) && (stem.is_empty() || !text.contains(&stem))
            });
        }
    }

    pub fn submit(&mut self, line: &str) {
        let chips = self.editor.take_chips();
        let mut text = line.to_string();
        for chip in &chips {
            text = text.replace(&chip.label, &chip.pointer);
        }
        text = self.absorb_image_paths(&text);
        match commands::parse(&text) {
            Ok(Parsed::Action(action)) => {
                self.separate();
                match &action {
                    Action::Submit(_) | Action::SayNow(_) => self.push_user(&action.line()),
                    _ => self.push_text(&action.line()),
                }
                self.dispatch(&action);
            }
            Ok(Parsed::Note(note)) => self.push_text(&note),
            Ok(Parsed::View(view)) => match view {
                commands::View::Expand(ordinal) => self.expand(ordinal),
            },
            Err(message) => {
                if !message.is_empty() {
                    self.push_text(&format!("kelir: {message}"));
                }
            }
        }
    }

    pub fn absorb_image_paths(&mut self, text: &str) -> String {
        if self.session_dir.is_none() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text.to_string();
        while let Some((start, matched)) = next_path(&rest) {
            out.push_str(&rest[..start]);
            let reusable = self.absorbed.contains(&matched);
            match attachments::pasted_image(&matched) {
                Some(paste) if !reusable => match self.save_image(&paste.path) {
                    Some(saved) => {
                        self.absorbed.insert(matched.clone());
                        out.push_str(&saved.pointer);
                    }
                    None => out.push_str(&matched),
                },
                _ => out.push_str(&matched),
            }
            rest = rest[start + matched.len()..].to_string();
        }
        out.push_str(&rest);
        out
    }

    pub fn dispatch(&mut self, action: &Action) {
        let request =
            Request::new(action.verb(), action.args(), "tui").with_token(Some(self.token.clone()));
        let (reply, answer) = oneshot::channel();
        if self
            .verbs
            .send(Incoming {
                request,
                reply,
                action: None,
            })
            .is_err()
        {
            self.quit = true;
            return;
        }
        let to_tui = self.to_tui.clone();
        tokio::spawn(async move {
            if let Ok(response) = answer.await {
                let _ = to_tui.send(View::Live(Live::Reply(response)));
            }
        });
    }

    fn ctrl_c(&mut self) {
        let now = Instant::now();
        let twice = self
            .last_ctrl_c
            .map(|last| now.duration_since(last) <= view::DOUBLE_CTRL_C)
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

    pub fn toggle_tail(&mut self) {
        if self.show_tail {
            self.show_tail = false;
            return;
        }
        let text = match &self.last_out {
            Some(path) => match view::read_tail(std::path::Path::new(path), view::TAIL_BYTES) {
                Ok(text) => text,
                Err(error) => format!("cannot read {path}: {error}"),
            },
            None => "no tool output yet".to_string(),
        };
        self.tail = crate::scrollback::wrap(&text, self.width).into();
        self.show_tail = true;
    }

    pub fn last_call(&self) -> Option<&CallLine> {
        self.calls.values().max_by_key(|call| call.ordinal)
    }

    fn read_output(&self, call: &CallLine) -> String {
        if call.out_path.is_empty() {
            return String::new();
        }
        view::read_tail(std::path::Path::new(&call.out_path), view::TAIL_BYTES).unwrap_or_default()
    }

    pub fn expand(&mut self, which: Option<u64>) {
        let ordinal = match which {
            Some(ordinal) => ordinal,
            None => match self.calls.keys().max() {
                Some(ordinal) => *ordinal,
                None => {
                    self.push_text("kelir: no tool call to expand yet");
                    return;
                }
            },
        };
        let Some(call) = self.calls.get(&ordinal).cloned() else {
            self.push_text(&format!("kelir: no call #{ordinal}"));
            return;
        };
        let text = self.read_output(&call);
        let lines = call.expanded(self.theme, self.width, &text, tools_view::EXPAND_ROWS);
        self.push_lines(lines);
        self.push_text("  (a committed line cannot unfold: /expand appends the output instead)");
    }

    /// Advance the spinner while a working state lasts.
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.activity.kind.working() {
            return false;
        }
        self.frame = self.frame.wrapping_add(1);
        if self.activity.kind == Kind::RateLimited
            && let Some(deadline) = self.retry_deadline
        {
            self.activity
                .tick_retry(deadline.saturating_duration_since(now));
        }
        true
    }
}

/// The next quoted or bare path-shaped token in `text`, as `(start, token)`.
fn next_path(text: &str) -> Option<(usize, String)> {
    let mut best: Option<(usize, String)> = None;
    for quote in ['"', '\''] {
        if let Some(open) = text.find(quote) {
            let rest = &text[open + quote.len_utf8()..];
            if let Some(close) = rest.find(quote) {
                let inner = &rest[..close];
                if attachments::extension_ok_str(inner) {
                    best = Some((open, format!("{quote}{inner}{quote}")));
                    break;
                }
            }
        }
    }
    if best.is_none() {
        let mut offset = 0usize;
        for token in text.split_inclusive(char::is_whitespace) {
            let word = token.trim_end();
            if attachments::extension_ok_str(word) {
                best = Some((offset, word.to_string()));
                break;
            }
            offset += token.len();
        }
    }
    best
}

fn osc52(text: &str) -> String {
    // OSC 52: put text on the host clipboard without taking the mouse or
    // adding a dependency. Base64 is small enough to inline here.
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::from("\x1b]52;c;");
    for chunk in text.as_bytes().chunks(3) {
        let n = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        out.push(ALPHABET[(n[0] >> 2) as usize] as char);
        out.push(ALPHABET[(((n[0] & 3) << 4) | (n[1] >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(((n[1] & 15) << 2) | (n[2] >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n[2] & 63) as usize] as char
        } else {
            '='
        });
    }
    out.push_str("\x1b\\");
    out
}

fn payload_text(value: &crate::event::Payload) -> String {
    match value {
        crate::event::Payload::Inline(value) => match value {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        },
        crate::event::Payload::Blob(reference) => reference.preview.clone(),
    }
}

fn payload_value(value: &crate::event::Payload) -> serde_json::Value {
    match value {
        crate::event::Payload::Inline(inner) => inner.clone(),
        crate::event::Payload::Blob(reference) => {
            serde_json::json!({"path": reference.path, "bytes": reference.bytes})
        }
    }
}
