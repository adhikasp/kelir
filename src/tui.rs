//! The inline TUI: live rows, a state row, the editor, a facts row, and
//! finished turns pushed into the terminal's scrollback.
//!
//! Three things keep it small and predictable. The viewport is fixed
//! ([`VIEWPORT_HEIGHT`] rows at the bottom of the screen) and only it is
//! repainted; everything a turn produces is written *above* it with ratatui's
//! `insert_before`, so the terminal keeps it and the user can scroll back. Every
//! action the TUI takes is a [`crate::commands::Action`], which is one verb sent
//! through the same [`crate::ipc::Incoming`] channel the session socket feeds —
//! the TUI has no private path into the session. And every style comes from
//! [`crate::theme::Theme`], so `NO_COLOR`, the ASCII glyph set and the 16-colour
//! rule are enforced in one place.
//!
//! The rows, top to bottom (TUI-v2 §7):
//!
//! ```text
//!   (scrollback: the conversation so far)
//!   live rows   (3): the tail of the streaming answer, or a running call
//!   state row   (1): ● idle · ready   /  ⠹ thinking 12s · waiting …  /  …
//!   editor      (3): › your message
//!   facts row   (1): kelir · model · step · tok · cache · $
//! ```

use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::crossterm::cursor::{MoveTo, Show};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::terminal::{Clear, ClearType};
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget as _};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::activity::{self, Activity, Kind};
use crate::attachments;
use crate::commands::{self, Action, Parsed};
use crate::editor::{Chip, Editor, Key, Outcome};
use crate::herdr::{self, State as HerdrState};
use crate::ipc::{Incoming, Request, Response};
use crate::markdown::{self, Layout, NoHighlight, Row};
use crate::scrollback;
use crate::theme::Theme;

/// The editor prompt's width (`› `), which the cursor's column includes.
pub const PROMPT_WIDTH: usize = 2;
use crate::tools_view::{self, CallLine, Group, Policy};

/// Rows the inline viewport occupies: three live rows, the state row, three
/// editor rows and the facts row.
pub const VIEWPORT_HEIGHT: u16 = 8;
/// A second Ctrl-C within this window quits instead of stopping.
pub const DOUBLE_CTRL_C: Duration = Duration::from_millis(1000);
/// How much of a call's output the tail view shows.
pub const TAIL_BYTES: usize = 2048;
/// The spinner changes this often while working; the elapsed text once a
/// second. Nothing ticks while idle.
pub const SPINNER_TICK: Duration = Duration::from_millis(100);
/// Frames are coalesced to at most one per this interval.
pub const FRAME_MIN: Duration = Duration::from_millis(33);

// ------------------------------------------------------------------ events

/// What the session tells the UI. One direction only: the UI never reaches
/// into the session.
#[derive(Debug, Clone)]
pub enum UiEvent {
    TurnStarted {
        turn: u32,
        provider: String,
        model: String,
        /// The turn's step cap, when one is set (`None` = no limit).
        limit: Option<u32>,
    },
    Step {
        step: u32,
        provider: String,
        model: String,
        /// The steps taken in the current turn, and the turn's cap when one is
        /// set (`None` = no limit, the default).
        turn_steps: u32,
        limit: Option<u32>,
    },
    /// An explicit cap was reached: the turn stops here, the session stays up.
    Paused {
        steps: u32,
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
        /// How long the call took, when the session measured it.
        elapsed_ms: u64,
        bytes: u64,
        lines: u64,
        spilled: bool,
        out_path: String,
        /// The last non-empty line of the call's output (a failed call's tail).
        tail: String,
    },
    /// A retryable provider error is being waited out (STEER-5's countdown).
    Retry {
        /// How long the wait is.
        retry_in_ms: u64,
        attempt: u32,
        /// `429`, `503`, `network`: a short reason, never a body.
        detail: String,
    },
    /// An image landed at the tail (a `kelir view` from a tool call).
    Attachment {
        /// `attachments/1-x.png`.
        rel: String,
        bytes: u64,
    },
    /// Something the session wants said but which is not an error.
    Info(String),
    Error(String),
    /// The answer to a verb the TUI sent.
    Reply(Response),
    /// Herdr's pane status. The session sends it here instead of writing it to
    /// stdout, so the TUI task is the only writer while the viewport is up.
    Herdr {
        state: HerdrState,
    },
    TurnEnded {
        outcome: String,
        final_text: String,
    },
    /// The session is gone; draw once more and restore the terminal.
    Quit,
}

// ------------------------------------------------------------------- facts

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Working,
    Done,
    /// An explicit `max_steps` cap ended the turn; the session is still up.
    Paused,
    Error,
}

impl Phase {
    pub fn word(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Working => "working",
            Phase::Done => "done",
            Phase::Paused => "paused",
            Phase::Error => "error",
        }
    }

    /// The state row derives from this: one source, two consumers (the row and
    /// Herdr's OSC word), so they cannot drift.
    pub fn activity_kind(self) -> Kind {
        match self {
            Phase::Idle | Phase::Done => Kind::Idle,
            Phase::Working => Kind::Thinking,
            Phase::Paused => Kind::Paused,
            Phase::Error => Kind::Error,
        }
    }
}

/// The facts row's data: model, step, tokens, cost, cache ratio.
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
    /// Steps taken in the turn now running, and the turn's cap when one is set
    /// (`None` = no limit, the default). Shown only when a cap exists: a
    /// counter with no limit is noise.
    pub turn_steps: u32,
    pub limit: Option<u32>,
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
            turn_steps: 0,
            limit: None,
            phase: Phase::Idle,
        }
    }
}

/// Token counts as humans read them: `999`, `1.5k`, `12.1k`.
pub fn tokens(count: u64) -> String {
    if count < 1000 {
        count.to_string()
    } else {
        format!("{:.1}k", count as f64 / 1000.0)
    }
}

/// The facts row, as one string (the renderer clips it to the width).
pub fn status_text(status: &Status) -> String {
    // One state, one word: `done` reads as `idle` here (the state row's kind for
    // it is idle too, so the two rows cannot disagree), while `paused` and
    // `error` keep their own word because they are not idle (S5). The summary
    // line above the row already carries the outcome.
    let word = match status.phase {
        Phase::Done => Phase::Idle.word(),
        other => other.word(),
    };
    let mut parts = vec![format!("kelir {word}")];
    if !status.model.is_empty() {
        parts.push(status.model.clone());
    }
    match status.limit {
        // An explicit cap: the count that matters is the turn's, against it.
        Some(limit) => parts.push(format!("step {}/{limit}", status.turn_steps)),
        None => parts.push(format!("step {}", status.step)),
    }
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
    parts.join(" · ")
}

/// The facts row trimmed for a narrow pane: the least useful facts go first
/// (cache %, then step, then cost), the model and the token count stay longest.
pub fn facts_for_width(status: &Status, width: usize) -> String {
    let full = status_text(status);
    if scrollback::display_width(&full) <= width {
        return full;
    }
    // The design's truncation order, from the right end: cache %, then step,
    // then cost, then the model. Tokens and the phase word stay longest.
    let attempts = [
        Status {
            cached_tokens: 0,
            ..status.clone()
        },
        Status {
            cached_tokens: 0,
            turn_steps: 0,
            limit: None,
            ..status.clone()
        },
        Status {
            cached_tokens: 0,
            turn_steps: 0,
            limit: None,
            cost: 0.0,
            ..status.clone()
        },
        Status {
            cached_tokens: 0,
            turn_steps: 0,
            limit: None,
            cost: 0.0,
            model: String::new(),
            ..status.clone()
        },
    ];
    for candidate in attempts {
        let text = status_text(&candidate);
        if scrollback::display_width(&text) <= width {
            return text;
        }
    }
    fit(
        &format!(
            "{} · {} tok",
            status.phase.word(),
            tokens(status.prompt_tokens + status.completion_tokens)
        ),
        width,
    )
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

fn one_line(text: &str) -> String {
    crate::util::truncate_chars(&text.replace('\n', " ↵ "), 60)
}

/// The session's one-line label for a running call, which the state row shows:
/// the tool's name and its own first line of input, never the raw JSON (a long
/// argument blob in a status row is noise).
pub fn call_label(name: &str, input: &Value) -> String {
    let detail = input
        .get("command")
        .and_then(Value::as_str)
        .or_else(|| input.get("path").and_then(Value::as_str))
        .map(|text| text.lines().next().unwrap_or("").trim().to_string())
        .unwrap_or_default();
    if detail.is_empty() {
        name.to_string()
    } else {
        format!("{name}: {}", crate::util::truncate_chars(&detail, 60))
    }
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

/// Read only the end of a call's output file: a 200 MB log costs one `seek`
/// and `max` bytes, not a 200 MB allocation.
pub fn read_tail(path: &std::path::Path, max: usize) -> std::io::Result<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    // One byte more than asked: `tail_of` then knows the window started
    // mid-file and drops the partial first line.
    let window = max as u64 + 1;
    let start = len.saturating_sub(window);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes)?;
    Ok(tail_of(&bytes, max))
}

/// The last non-empty line of a call's output, for a failed call's tail row.
pub fn last_line(text: &str) -> String {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| crate::util::truncate_chars(line.trim(), 200))
        .unwrap_or_default()
}

// --------------------------------------------------------------------- app

/// One block of a streamed message: its text and how much of it is already in
/// the scrollback.
#[derive(Debug, Clone)]
struct Block {
    text: String,
    /// How many of the block's rendered rows are out.
    committed: usize,
    /// The blank row that separates this block from the one before it is out.
    separated: bool,
    /// The next block may continue this one: the block ends inside a list item
    /// that CommonMark keeps open across the blank line.
    joins: bool,
}

/// The message being streamed, committed a block at a time.
///
/// Two invariants this type exists for:
///
/// - **Every rendered row reaches the scrollback exactly once, in the order the
///   model produced it.** Only the last block can still change (the open tail is
///   re-rendered while it grows); every block before it is final and its rows go
///   out at once. That is why a long fence streams into the scrollback instead
///   of becoming a three-row ticker, why the previous block's rows cannot vanish
///   while a later one streams (`S1`), and why prose can never be written before
///   the tool lines that preceded it (`B1`: the group closes first).
/// - **A loose list item keeps its continuation inside it** (`S7`): a block that
///   a following, indented block continues is joined to it and rendered as one,
///   exactly as the one-shot render reads the same text.
#[derive(Debug, Default)]
struct Message {
    blocks: Vec<Block>,
}

/// Does this block start indented, so a list item open before it continues into
/// it? (CommonMark: a continuation must be indented to the item's content.)
fn continues_item(block: &str) -> bool {
    let first = block
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    first.starts_with("  ") || first.as_bytes().first() == Some(&9u8)
}

impl Message {
    fn is_empty(&self) -> bool {
        self.blocks.iter().all(|block| block.text.trim().is_empty())
    }

    /// A closed block arrived. It is joined to the previous block when CommonMark
    /// reads it as that block's continuation, and kept as its own block
    /// otherwise.
    fn push_block(&mut self, text: &str) {
        if let Some(last) = self.blocks.last_mut()
            && last.committed == 0
            && !last.separated
            && last.joins
            && continues_item(text)
        {
            last.text = format!(
                "{}

{text}",
                last.text
            );
            last.joins = markdown::ends_in_item(&last.text);
            return;
        }
        self.blocks.push(Block {
            text: text.to_string(),
            committed: 0,
            separated: false,
            joins: markdown::ends_in_item(text),
        });
    }

    /// The rows of block `index` that are not out yet: the separator, then the
    /// text. `None` renders the open tail instead of the block's own text.
    fn rows_after(&self, index: usize, text: &str, layout: Layout) -> Vec<Row> {
        let block = &self.blocks[index];
        let rows = markdown::render_block(text, layout);
        let mut out = Vec::new();
        if !block.separated && index > 0 && !rows.is_empty() {
            out.push(Row::default());
        }
        out.extend(rows[block.committed.min(rows.len())..].to_vec());
        out
    }

    /// Mark block `index` delivered, including its separator.
    fn mark(&mut self, index: usize, count: usize) {
        let block = &mut self.blocks[index];
        block.separated = true;
        block.committed = count;
    }

    /// Every block is decided: nothing held may be joined to what comes next.
    /// Called when something other than prose interrupts the message (a tool
    /// call), so a held list block cannot swallow the sentence after it.
    fn settle(&mut self) {
        for block in &mut self.blocks {
            block.joins = false;
        }
    }

    /// The rows of every final block: all but the last, plus the last one when
    /// nothing is streaming any more. Handed out once each.
    fn settled_rows(&mut self, layout: Layout) -> Vec<Row> {
        let last = self.blocks.len().saturating_sub(1);
        let mut out = Vec::new();
        for index in 0..self.blocks.len() {
            // Every closed block is final, except the last one when a following
            // block may continue it (a loose list item): its rows are held until
            // the next block decides.
            if index == last && self.blocks[index].joins {
                break;
            }
            let text = self.blocks[index].text.clone();
            let rows = self.rows_after(index, &text, layout);
            let total = markdown::render_block(&text, layout).len();
            if rows.is_empty() {
                continue;
            }
            out.extend(rows);
            self.mark(index, total);
        }
        out
    }

    /// The live rows: what is not in the scrollback yet, at most `height` of
    /// them — the last block's remaining rows, then the open tail.
    fn live_rows(&self, layout: Layout, tail: &str, height: usize) -> Vec<Row> {
        let mut rows = match self.blocks.len().checked_sub(1) {
            Some(index) => {
                let text = self.blocks[index].text.clone();
                self.rows_after(index, &text, layout)
            }
            None => Vec::new(),
        };
        if !tail.trim().is_empty() {
            if !rows.is_empty() {
                rows.push(Row::default());
            }
            rows.extend(markdown::render_block(tail, layout));
        }
        let start = rows.len().saturating_sub(height);
        rows.drain(..start);
        rows
    }

    /// The stream is over: each block's remaining rows, once, then nothing.
    fn finish(&mut self, layout: Layout) -> Vec<Row> {
        let rows = self.settled_rows(layout);
        self.blocks.clear();
        rows
    }
}

pub struct App {
    pub editor: Editor,
    pub theme: Theme,
    /// The state machine the state row renders.
    pub activity: Activity,
    pub status: Status,
    pub width: usize,
    pub height: usize,
    /// Rows ready to be written above the viewport, styled.
    pub scroll: Vec<Line<'static>>,
    /// Whether a message row has reached the scrollback in this message (so the
    /// blank row after it is not written twice).
    committed_message: bool,
    /// The streamed message, committed a block at a time as the blocks close:
    /// the text of every block, its rendered rows, and how many of those rows
    /// are already in the scrollback. Every row is written exactly once and
    /// nothing is held back, so no row can vanish while a later block streams
    /// (the B1/S1 class) and a long block can no longer become a ticker.
    message: Message,
    stream: markdown::Stream,
    /// The output tail view (Ctrl-O).
    pub tail: VecDeque<String>,
    pub show_tail: bool,
    /// The open group of settled calls.
    pub group: Group,
    /// Whether settled calls print their output too.
    pub policy: Policy,
    /// The session-wide call ordinal: what `/expand n` names.
    pub call_ordinal: u64,
    /// Every settled call of this session, by ordinal, for `/expand`.
    calls: HashMap<u64, CallLine>,
    /// Per-role totals of the turn now running, for the summary line.
    turn_started: Option<Instant>,
    turn_steps: u32,
    turn_tokens: u64,
    turn_cost: f64,
    /// The spinner frame, advanced by the working-state tick.
    frame: u64,
    /// Images saved from pastes, by content hash: a second paste of the same
    /// bytes reuses the file.
    seen_images: HashMap<u64, String>,
    /// The session dir, for `attachments/`; `None` in a unit test with no
    /// session (pastes then stay text).
    session_dir: Option<std::path::PathBuf>,
    /// The image paths already turned into pointers at submit, so a line
    /// submitted twice (history) does not save the same file twice.
    absorbed: std::collections::HashSet<String>,
    /// A note the state row shows (`saving image…`), cleared by the next event.
    pub note: String,
    /// When the current retry wait ends, so the countdown is read from the
    /// wall clock instead of subtracting from a stale snapshot.
    retry_deadline: Option<Instant>,
    last_out: Option<String>,
    last_ctrl_c: Option<Instant>,
    /// OSC 7501 records waiting to be written between two paints.
    status_records: Vec<String>,
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
            theme: Theme::plain(),
            activity: Activity::idle(Instant::now()),
            status: Status::default(),
            width,
            height,
            scroll: Vec::new(),
            committed_message: false,
            message: Message::default(),
            stream: markdown::Stream::new(),
            tail: VecDeque::new(),
            show_tail: false,
            group: Group::default(),
            policy: Policy::default(),
            call_ordinal: 0,
            calls: HashMap::new(),
            turn_started: None,
            turn_steps: 0,
            turn_tokens: 0,
            turn_cost: 0.0,
            frame: 0,
            seen_images: HashMap::new(),
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
        }
    }

    /// The session dir (for `attachments/`), once the session exists.
    pub fn set_session_dir(&mut self, dir: std::path::PathBuf) {
        self.session_dir = Some(dir);
    }

    pub fn theme(&self) -> Theme {
        self.theme
    }

    pub fn set_size(&mut self, width: usize, height: usize) {
        let width = width.max(1);
        if self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        // Committed scrollback cannot reflow (the terminal owns those rows):
        // only what has not been committed yet is re-rendered, which is what
        // keeps a resize from emitting a row twice.
        self.rerender_pending();
    }

    fn layout(&self) -> Layout {
        Layout::with_glyphs(self.width, self.theme.glyphs)
    }

    fn live_height(&self) -> usize {
        layout_of(self.width, self.height).live.height as usize
    }

    /// Rows to write above the viewport, in order.
    pub fn take_scroll(&mut self) -> Vec<Line<'static>> {
        std::mem::take(&mut self.scroll)
    }

    /// Herdr records queued for the next write between two paints.
    pub fn take_status_records(&mut self) -> Vec<String> {
        std::mem::take(&mut self.status_records)
    }

    pub fn note(&self) -> &str {
        &self.note
    }

    /// The live rows: what is still open. With nothing streaming that is the
    /// open tool group (below); otherwise it is the tail of the message — every
    /// row the streamed text has produced that is not in the scrollback yet.
    /// Nothing is held back for a later frame (`S1`).
    pub fn live_view(&self) -> Vec<Row> {
        let tail = self.stream.tail();
        self.message
            .live_rows(self.layout(), &tail, self.live_height())
    }

    /// The live area as ratatui lines: the open call group (when nothing is
    /// streaming) or the tail of the streamed message.
    pub fn live_lines(&self) -> Vec<Line<'static>> {
        let theme = self.theme;
        let rows = self.live_view();
        if !rows.is_empty() {
            return rows
                .iter()
                .map(|row| row.line(theme, &NoHighlight))
                .collect();
        }
        self.group.preview(theme, self.width, self.live_height())
    }

    /// Queue plain text for the scrollback (banner, notes, verb replies).
    pub fn push_text(&mut self, text: &str) {
        for row in scrollback::wrap(text, self.width) {
            self.scroll.push(Line::from(row));
        }
    }

    /// Queue styled rows (a tool line, a summary).
    pub fn push_lines(&mut self, lines: Vec<Line<'static>>) {
        self.scroll.extend(lines);
    }

    /// Commit every row that can no longer change and every finished block:
    /// the open tail is re-rendered as it grows, so only the rows before it are
    /// stable. Called after every delta and every block, so the scrollback
    /// always holds everything the streamed text has produced up to its last
    /// open block.
    fn commit_settled(&mut self) {
        let rows = self.message.settled_rows(self.layout());
        if rows.is_empty() {
            return;
        }
        let theme = self.theme;
        for row in rows {
            self.scroll.push(row.line(theme, &NoHighlight));
        }
        self.committed_message = true;
    }

    /// A resize re-renders what is still open for the new width. What the
    /// scrollback already has is not re-rendered: the terminal owns those rows
    /// and rewriting them would duplicate them.
    fn rerender_pending(&mut self) {
        // Nothing to do: the open tail is rendered from its text, the closed
        // blocks are already committed, and what is held (one block, until a
        // following block decides whether it continues it) is committed by the
        // next delta, at the new width.
    }

    /// A tool call interrupts the message: the text that had streamed before it
    /// is a finished block and goes into the scrollback *now*, so the call's
    /// line lands after it and before whatever the model says next (`B1`).
    /// Returns whether there was prose to flush.
    fn interrupt_prose(&mut self) -> bool {
        // Only prose in flight needs flushing; an empty stream must not reset
        // the splitter's fence/table state mid-block.
        if self.stream.is_empty() {
            self.message.settle();
            self.commit_settled();
            return false;
        }
        let block = self.stream.finish();
        let flushed = block.is_some();
        if let Some(block) = block {
            self.message.push_block(&block);
        }
        // A call is not a continuation: nothing in the message may swallow what
        // follows one, so the held block is decided here.
        self.message.settle();
        self.commit_settled();
        flushed
    }

    /// A complete block arrived: it is held (it may still be continued by the
    /// next block), and everything before it is committed.
    fn absorb_block(&mut self, block: &str) {
        self.message.push_block(block);
        self.commit_settled();
    }

    /// Commit the streamed message: whatever is left of it, exactly once, then
    /// one blank row.
    pub fn commit_live(&mut self, fallback: &str) {
        if let Some(block) = self.stream.finish() {
            self.message.push_block(&block);
        }
        if self.message.is_empty() && !fallback.trim().is_empty() {
            self.message.push_block(fallback);
        }
        let rows = self.message.finish(self.layout());
        let theme = self.theme;
        for row in &rows {
            self.scroll.push(row.line(theme, &NoHighlight));
        }
        self.stream = markdown::Stream::new();
        // One blank row after a message, never two at the tail of the
        // scrollback (a message that was fully committed already has it).
        let announced = !rows.is_empty() || self.committed_message;
        if announced && !self.scroll.last().is_some_and(|line| line.spans.is_empty()) {
            self.scroll.push(Line::from(""));
        }
        self.committed_message = false;
    }

    /// The turn's own totals, for the one-line summary.
    fn turn_summary(&mut self, outcome: &str) {
        let elapsed = self
            .turn_started
            .take()
            .map(|started| started.elapsed())
            .unwrap_or_default();
        let steps = self.turn_steps.max(self.status.turn_steps);
        let summary = activity::summary_line(
            self.theme,
            outcome,
            steps,
            self.turn_tokens,
            self.turn_cost,
            elapsed,
        );
        self.scroll.push(summary);
        self.scroll.push(Line::from(""));
        self.turn_steps = 0;
        self.turn_tokens = 0;
        self.turn_cost = 0.0;
    }

    /// Close the open tool group, if any: its rows go to the scrollback.
    /// Returns whether there was one.
    fn close_group(&mut self) -> bool {
        if self.group.is_empty() {
            return false;
        }
        let lines = self.group.close(self.theme, self.width);
        self.scroll.extend(lines);
        true
    }

    /// One blank row between two visual blocks, and never more than one (the
    /// design note's spacing rule, review N1): a prompt echo starts its own
    /// block, as does prose after tool lines. Nothing is written at the top of
    /// an empty scrollback.
    fn separate(&mut self) {
        let blank =
            |line: &Line<'static>| line.spans.iter().all(|span| span.content.trim().is_empty());
        match self.scroll.last() {
            Some(last) if !blank(last) => self.scroll.push(Line::from("")),
            _ => {}
        }
    }

    pub fn apply(&mut self, event: UiEvent) {
        let now = Instant::now();
        match event {
            UiEvent::TurnStarted {
                turn,
                provider,
                model,
                limit,
            } => {
                self.status.turn = turn;
                self.status.provider = provider;
                self.status.model = model;
                self.status.phase = Phase::Working;
                // A new turn counts its own steps (an explicit cap is per turn).
                self.status.turn_steps = 0;
                self.status.limit = limit;
                self.turn_started = Some(now);
                self.turn_steps = 0;
                self.turn_tokens = 0;
                self.turn_cost = 0.0;
                self.note.clear();
                self.activity.step_started(now);
            }
            UiEvent::Step {
                step,
                provider,
                model,
                turn_steps,
                limit,
            } => {
                self.status.step = step;
                self.status.provider = provider;
                self.status.model = model;
                self.status.turn_steps = turn_steps;
                self.status.limit = limit;
                self.turn_steps = turn_steps;
                self.status.phase = Phase::Working;
                self.activity.step_started(now);
                self.retry_deadline = None;
            }
            UiEvent::Paused { steps } => {
                self.status.phase = Phase::Paused;
                self.status.running = 0;
                self.status.turn_steps = steps;
                self.turn_steps = steps;
                self.activity
                    .paused(now, &format!("step cap {steps} reached"));
                self.push_text(&format!(
                    "turn paused after {steps} steps; say continue to resume"
                ));
            }
            UiEvent::Delta(text) => {
                // Prose follows tool lines: the open group closes before the
                // first row of it is queued, so the transcript's order is the
                // order the events happened in (`B1`), with one blank row
                // between the two blocks. A group of calls settled without
                // prose between them stays open here, so consecutive calls
                // still fold. (The stream was flushed when the call started:
                // see `interrupt_prose`.)
                if self.close_group() && !self.scroll.is_empty() {
                    self.separate();
                }
                self.status.phase = Phase::Working;
                self.activity.text(now, text.chars().count());
                for block in self.stream.push(&text) {
                    self.absorb_block(&block);
                }
            }
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
                self.turn_tokens += prompt + completion;
                self.turn_cost += cost;
            }
            UiEvent::Assistant { text } => {
                // The group closes first: its lines describe calls that ran
                // before this message (`B1`).
                if self.close_group() && !self.scroll.is_empty() {
                    self.separate();
                }
                self.commit_live(&text);
            }
            UiEvent::ToolStarted { name, input } => {
                // A call is the one thing that cannot be part of a paragraph:
                // the prose that announced it is finished and goes into the
                // scrollback now, so the call's line lands after it and before
                // whatever the model says next (`B1`).
                if self.interrupt_prose() && !self.scroll.is_empty() {
                    self.separate();
                }
                self.status.running += 1;
                self.activity.tool_started(now, &call_label(&name, &input));
            }
            UiEvent::ToolSettled {
                name,
                input,
                outcome,
                exit_code,
                elapsed_ms,
                bytes,
                lines,
                spilled,
                out_path,
                tail,
            } => {
                // A session that resumes mid-turn can settle a call whose start
                // this process never saw: the same rule holds, flush first.
                if self.interrupt_prose() && !self.scroll.is_empty() {
                    self.separate();
                }
                self.status.running = self.status.running.saturating_sub(1);
                self.activity.tool_settled(now);
                self.call_ordinal += 1;
                let call = CallLine {
                    ordinal: self.call_ordinal,
                    name: name.clone(),
                    head: call_head(&name, &input),
                    outcome,
                    exit_code,
                    bytes,
                    lines,
                    spilled,
                    out_path: out_path.clone(),
                    tail,
                    elapsed: Some(Duration::from_millis(elapsed_ms)),
                };
                self.last_out = Some(out_path).filter(|path| !path.is_empty());
                self.calls.insert(call.ordinal, call.clone());
                match self.policy {
                    Policy::Collapsed => self.group.push(call),
                    Policy::Full => {
                        // Every call prints its output block too.
                        let mut lines = self.group.close(self.theme, self.width);
                        lines.push(call.line(self.theme, self.width));
                        if call.failed()
                            && let Some(tail) = call.tail_line(self.theme, self.width)
                        {
                            lines.push(tail);
                        }
                        lines.extend(call.expanded(
                            self.theme,
                            self.width,
                            &self.read_output(&call),
                            tools_view::EXPAND_ROWS,
                        ));
                        self.scroll.extend(lines);
                    }
                }
            }
            UiEvent::Retry {
                retry_in_ms,
                attempt,
                detail,
            } => {
                let wait = Duration::from_millis(retry_in_ms);
                if self.activity.kind == Kind::RateLimited {
                    self.activity.tick_retry(wait);
                } else {
                    self.activity.retry(now, wait, attempt, &detail);
                }
                self.retry_deadline = Some(now + wait);
            }
            UiEvent::Attachment { rel, bytes } => {
                self.push_lines(vec![Line::from(Span::styled(
                    format!("  view {rel} · {}", attachments::bytes_label(bytes)),
                    self.theme.dim(),
                ))]);
            }
            UiEvent::Info(text) => {
                self.push_text(&text);
                self.note.clear();
            }
            UiEvent::Error(text) => {
                self.status.phase = Phase::Error;
                self.activity.failed(now, &one_line(&text));
                self.push_text(&format!("kelir: {text}"));
            }
            UiEvent::Reply(response) => {
                if response.ok {
                    // A `say`/`later` was already echoed as the user's line,
                    // and an idle `stop` was already answered on screen as
                    // dropped; neither needs a second line.
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
                    self.note = one_line(&message);
                }
            }
            UiEvent::TurnEnded { outcome, .. } => {
                self.close_group();
                self.commit_live("");
                self.activity.turn_ended(now, &outcome);
                self.status.phase = match outcome.as_str() {
                    "done" => Phase::Done,
                    "paused" => Phase::Paused,
                    "error" => Phase::Error,
                    _ => Phase::Idle,
                };
                self.status.running = 0;
                self.note.clear();
                self.turn_summary(&outcome);
            }
            UiEvent::Herdr { state } => self.status_records.push(herdr::record(state)),
            UiEvent::Quit => self.quit = true,
        }
    }

    fn read_output(&self, call: &CallLine) -> String {
        if call.out_path.is_empty() {
            return String::new();
        }
        read_tail(std::path::Path::new(&call.out_path), TAIL_BYTES).unwrap_or_default()
    }

    /// One key press.
    pub fn press(&mut self, press: Press) {
        match press {
            Press::Edit(key) => {
                let typed = matches!(key, Key::Char(c) if !c.is_control());
                match self.editor.handle(key) {
                    Outcome::Submitted(text) => self.submit(&text),
                    Outcome::ChipRemoved(chip) => self.discard_chip(&chip),
                    Outcome::Edited | Outcome::Ignored => {
                        // A terminal whose bracketed paste arrives as keystrokes
                        // (Windows: crossterm decodes `ESC[200~` only on Unix)
                        // has just typed a path character by character. The
                        // moment a complete image path ends at the cursor it
                        // becomes the same chip the paste would have made (S8),
                        // so the chip is what the user sees and edits on every
                        // platform, not only on Unix.
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
                // Esc while working is the same verb as the first Ctrl-C.
                if self.activity.kind.working() {
                    self.push_text("» stop (Esc)");
                    self.dispatch(&Action::Stop(None));
                }
            }
            Press::Redraw => {}
        }
    }

    /// Take the image path that was just typed (or pasted as keystrokes) at the
    /// cursor and turn it into a chip: the file is saved into the session, the
    /// path text is removed from the buffer and `[image N]` takes its place.
    ///
    /// The whole run of text ending at the cursor is considered, not just the
    /// last character: a path can arrive in pieces (a drag and drop types it in
    /// one burst, a slow paste in several), and a partial path is simply not a
    /// file yet, so nothing happens until it is.
    fn adopt_typed_image(&mut self) {
        if self.session_dir.is_none() {
            return;
        }
        let Some(line) = self.editor.lines().get(self.editor.cursor().0) else {
            return;
        };
        let col = self.editor.cursor().1;
        let typed: String = line.chars().take(col).collect();
        let typed = typed.trim_end();
        let Some(candidate) = attachments::path_tail(typed) else {
            return;
        };
        // Only a complete, existing image: `attachments::pasted_image` is the
        // same rule the paste takes, so both ingress paths agree.
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

    /// A bracket paste: an image *path* becomes a chip, anything else is text.
    ///
    /// The image work (read, hash, copy) is a few hundred kilobytes of file
    /// I/O; it happens here, on the input path, and the chip appears when it is
    /// done. Never the path text itself: the prompt must carry the pointer.
    pub fn paste(&mut self, text: &str) {
        if let Some(paste) = attachments::pasted_image(text)
            && let Some(saved) = self.save_image(&paste.path)
        {
            let chip = Chip::new(saved.seq, &saved.rel, &saved.pointer);
            self.press(Press::Edit(Key::Chip(chip)));
            return;
        }
        self.press(Press::Edit(Key::Paste(text.to_string())));
    }

    /// Copy a pasted image into the session's attachments and describe it.
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
                self.note = one_line(&message);
                None
            }
        }
    }

    /// A chip was deleted before it was sent: so is its file.
    fn discard_chip(&mut self, chip: &Chip) {
        // Only when no other chip in the buffer still points at the same file
        // (a deduped paste shares one).
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
            // The file is gone: a later paste of the same bytes must save again.
            self.seen_images.retain(|_, rel| rel != &chip.rel);
        }
    }

    /// Parse one submitted line and send what it asks for. Chips become pointer
    /// text here: the editor showed `[image 1]`, the model sees the pointer.
    ///
    /// A line that contains an image *path* (a drag and drop, or a paste on a
    /// terminal whose bracketed paste arrives as keystrokes — crossterm 0.29
    /// only decodes `ESC[200~` on Unix) gets the same treatment: the file is
    /// saved and the path is replaced with the pointer. So a dropped path is a
    /// pointer on every platform, never a raw path and never bytes.
    pub fn submit(&mut self, line: &str) {
        let chips = self.editor.take_chips();
        let mut text = line.to_string();
        for chip in &chips {
            text = text.replace(&chip.label, &chip.pointer);
        }
        text = self.absorb_image_paths(&text);
        match commands::parse(&text) {
            Ok(Parsed::Action(action)) => {
                self.close_group();
                self.group = Group::default();
                // The echo starts a block of its own (spacing rule).
                self.separate();
                self.push_text(&action.line());
                self.dispatch(&action);
            }
            Ok(Parsed::Note(note)) => self.push_text(&note),
            Ok(Parsed::View(view)) => match view {
                commands::View::Full => self.set_policy(Policy::Full),
                commands::View::Expand(ordinal) => self.expand(ordinal),
                // A command named `/collapse` collapses; it never expands
                // (`S6`). Applying it twice changes nothing.
                commands::View::Collapse => self.set_policy(Policy::Collapsed),
            },
            Err(message) => {
                if !message.is_empty() {
                    self.push_text(&format!("kelir: {message}"));
                }
            }
        }
    }

    /// Replace every existing image path in `text` with a pointer, saving the
    /// file into the session first. Words that are not images are left alone.
    /// Replace every existing image path in `text` with a pointer, saving the
    /// file into the session first. A path may be quoted (`"C:\… b.png"`, what
    /// Explorer's drag and drop and a clipboard paste produce) or bare; words
    /// that are not images are left exactly as they were.
    pub fn absorb_image_paths(&mut self, text: &str) -> String {
        if self.session_dir.is_none() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text.to_string();
        while let Some(candidate) = next_path(&rest) {
            let (start, matched) = candidate;
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

    /// Ctrl-O: the last call's output tail takes the whole area above the facts
    /// row, and the editor is out of the way while it is open.
    pub fn toggle_tail(&mut self) {
        if self.show_tail {
            self.show_tail = false;
            return;
        }
        let text = match &self.last_out {
            Some(path) => match read_tail(std::path::Path::new(path), TAIL_BYTES) {
                Ok(text) => text,
                Err(error) => format!("cannot read {path}: {error}"),
            },
            None => "no tool output yet".to_string(),
        };
        self.tail = scrollback::wrap(&text, self.width).into();
        self.show_tail = true;
    }

    /// The most recent settled call, which the Ctrl-O overlay's header names.
    pub fn last_call(&self) -> Option<&CallLine> {
        self.calls.values().max_by_key(|call| call.ordinal)
    }

    /// `/expand [n|last]`: append one call's output to the scrollback.
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
        self.scroll.extend(lines);
        self.push_text("  (a committed line cannot unfold: /expand appends the output instead)");
    }

    /// `/collapse` and `/expand`: the policy for *future* calls. A committed
    /// line stays as it is.
    pub fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
        self.push_text(&format!("» tool output: {}", policy.word()));
    }

    /// Advance the spinner while a working state lasts. Returns whether a
    /// repaint is due (the spinner frame or an elapsed second changed).
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.activity.kind.working() {
            return false;
        }
        self.frame = self.frame.wrapping_add(1);
        if self.activity.kind == Kind::RateLimited {
            // The countdown is wall-clock based: read it, do not subtract.
            if let Some(deadline) = self.retry_deadline {
                let left = deadline.saturating_duration_since(now);
                self.activity.tick_retry(left);
            }
        }
        true
    }
}

// ------------------------------------------------------------------- input

/// The next quoted or bare path-shaped token in `text`, as `(start, token)`.
///
/// Quoted first (a Windows path with spaces arrives quoted), then bare: a token
/// of non-space characters whose extension is one kelir knows. The caller
/// decides whether it is a real file; this only finds the candidates.
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
    // Bare tokens: split on whitespace and keep the first with an image
    // extension. A quoted match wins because it can contain spaces.
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

/// A key press the TUI acts on (already translated from a terminal event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    Edit(Key),
    /// Backspace: the chip at the cursor (and its file) when there is one, a
    /// character otherwise.
    Backspace,
    CtrlC,
    ToggleTail,
    /// Esc: stop the turn, but only while working.
    Stop,
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
        KeyCode::Esc => Some(Press::Stop),
        // Enter sends; Alt-Enter (and Shift-Enter where the terminal reports
        // it) starts a new line instead.
        KeyCode::Enter if alt || shift => Some(Press::Edit(Key::Newline)),
        KeyCode::Enter => Some(Press::Edit(Key::Enter)),
        // Backspace belongs to the chip when the cursor is on one: STEER-10
        // req. 3 and the docs say one Backspace removes the whole chip. The
        // editor ignores `RemoveChip` when no chip is at the cursor and the TUI
        // then falls back to a plain Backspace, so a chip never costs a
        // keystroke and typed text never loses one.
        KeyCode::Backspace => Some(Press::Backspace),
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
    pub state: Rect,
    pub editor: Rect,
    pub facts: Rect,
}

/// Three live rows, the state row, three editor rows, the facts row — fewer
/// when the screen is shorter than the viewport. The state row goes last (a
/// user who cannot see whether the agent runs has nothing left), then the
/// live rows, then the facts row, then the editor to one row.
pub fn layout(area: Rect) -> Areas {
    let mut areas = layout_of(area.width as usize, area.height as usize);
    // The rects are built from the row count alone; move them to where the
    // caller's area really starts (an inline viewport does not start at row 0,
    // and drawing at row 0 would paint over the scrollback above it).
    for row in [
        &mut areas.live,
        &mut areas.state,
        &mut areas.editor,
        &mut areas.facts,
    ] {
        row.x = area.x;
        row.y += area.y;
    }
    areas
}

fn layout_of(width: usize, height: usize) -> Areas {
    let area = Rect::new(0, 0, width as u16, height as u16);
    if area.height == 0 {
        return Areas {
            live: area,
            state: area,
            editor: area,
            facts: area,
        };
    }
    // The order rows are given up (design note §7): the facts row first, then
    // live rows, then the editor down to one row; the state row is the last to
    // go, because a user who cannot see whether the agent is working has
    // nothing left.
    let facts_height = u16::from(area.height >= 6);
    let state_height = u16::from(area.height >= 2);
    let space = area.height.saturating_sub(facts_height + state_height);
    let editor_height = 3.min(space.saturating_sub(1)).max(u16::from(space > 0));
    let live_height = space.saturating_sub(editor_height);

    let live = Rect::new(area.x, area.y, area.width, live_height);
    let state = Rect::new(area.x, live.y + live_height, area.width, state_height);
    let editor = Rect::new(area.x, state.y + state_height, area.width, editor_height);
    let facts = Rect::new(area.x, editor.y + editor_height, area.width, facts_height);
    Areas {
        live,
        state,
        editor,
        facts,
    }
}

// -------------------------------------------------------------------- draw

/// The top row of the last painted inline viewport, for the panic hook: a
/// panic erases the same rows a clean exit does. `u16::MAX` means "never
/// painted".
static VIEWPORT_Y: AtomicU16 = AtomicU16::new(u16::MAX);

/// Paint the viewport. Everything else goes through `insert_before`.
pub fn render(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    app.set_size(area.width as usize, area.height as usize);
    VIEWPORT_Y.store(area.y, Ordering::Relaxed);
    let areas = layout(area);
    if app.show_tail {
        render_tail(app, frame, area, areas.facts);
    } else {
        render_live(app, frame, areas.live);
        render_editor(app, frame, area, areas.editor);
        render_state(app, frame, areas.state);
    }
    render_facts(app, frame, areas.facts);
}

/// The streamed text (markdown, styled) in the live area.
fn render_live(app: &App, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let lines = app.live_lines();
    let height = area.height as usize;
    let start = lines.len().saturating_sub(height);
    frame.render_widget(Paragraph::new(lines[start..].to_vec()), area);
}

/// The editor: the window of rows that keeps the cursor visible, the prompt
/// dimmed while the agent works, the image chips reversed, and the cursor
/// itself counted in display columns.
fn render_editor(app: &App, frame: &mut Frame, screen: Rect, area: Rect) {
    if area.height == 0 {
        return;
    }
    let theme = app.theme;
    let prompt_style = if app.activity.kind.working() {
        theme.dim()
    } else {
        theme.prose()
    };
    let (first, visible) = app.editor.window(area.height as usize);
    let lines = app.editor.lines();
    let mut text: Vec<Line<'static>> = Vec::new();
    for (index, line) in lines.iter().enumerate().skip(first).take(visible) {
        let prefix = if index == 0 {
            theme.glyphs.prompt()
        } else {
            theme.glyphs.prompt_pad()
        };
        let mut spans = vec![Span::styled(prefix.to_string(), prompt_style)];
        spans.extend(chip_spans(app, line, theme));
        text.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(text), area);
    let (row, column) = app.editor.cursor_display();
    let x = (area.x as usize + column).min(screen.width.saturating_sub(1) as usize) as u16;
    let local = row
        .saturating_sub(first)
        .min(area.height.saturating_sub(1) as usize);
    frame.set_cursor_position(Position::new(x, area.y + local as u16));
}

/// One editor row's spans, with every chip label painted reversed.
fn chip_spans(app: &App, line: &str, theme: Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let mut earliest: Option<(usize, &Chip)> = None;
        for chip in app.editor.chips() {
            if let Some(at) = rest.find(&chip.label)
                && earliest.is_none_or(|(best, _)| at < best)
            {
                earliest = Some((at, chip));
            }
        }
        match earliest {
            Some((at, chip)) => {
                if at > 0 {
                    spans.push(Span::styled(rest[..at].to_string(), theme.prose()));
                }
                spans.push(Span::styled(chip.label.clone(), theme.chip()));
                rest = &rest[at + chip.label.len()..];
            }
            None => {
                spans.push(Span::styled(rest.to_string(), theme.prose()));
                break;
            }
        }
    }
    spans
}

/// The output tail (Ctrl-O): the whole area above the state row, dim.
fn render_tail(app: &App, frame: &mut Frame, screen: Rect, facts: Rect) {
    // Everything above the facts row: the overlay replaces the live area, the
    // state row and the editor while it is open, and the facts row says how to
    // close it.
    let height = facts.y.saturating_sub(screen.y);
    let area = Rect {
        x: screen.x,
        y: screen.y,
        width: screen.width,
        height,
    };
    // The overlay's header is the call line (design note §4), so the reader
    // knows which call the output belongs to; the rest is the dim tail.
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(call) = app.last_call() {
        lines.push(call.line(app.theme, area.width as usize));
    }
    lines.extend(app.tail.iter().map(|row| Line::from(row.clone())));
    frame.render_widget(
        Paragraph::new(lines)
            .style(app.theme.dim())
            .scroll((tail_scroll(app, area.height as usize), 0)),
        area,
    );
}

fn tail_scroll(app: &App, height: usize) -> u16 {
    // The header takes one row, so the tail starts one lower.
    let rows = app.tail.len() + usize::from(app.last_call().is_some());
    rows.saturating_sub(height.max(1)).min(u16::MAX as usize) as u16
}

/// The state row: glyph, word, elapsed, detail — dim while idle, cyan while
/// working, yellow while paused, red on an error.
fn render_state(app: &App, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let theme = app.theme;
    let line = app
        .activity
        .line(theme, app.frame, Instant::now(), area.width as usize);
    let line = if app.note.is_empty() {
        line
    } else {
        // A transient note (`saving image…`, a refused paste) wins the row's
        // detail for a moment.
        let text = fit(
            &format!(" {} · {}", app.activity.kind.word(), app.note),
            area.width as usize,
        );
        Line::from(Span::styled(text, theme.dim()))
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn render_facts(app: &App, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let theme = app.theme;
    let mut text = facts_for_width(&app.status, area.width as usize);
    if app.show_tail {
        text = format!("tail (Ctrl-O closes) · {text}");
        text = fit(&text, area.width as usize);
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(text, theme.dim()))),
        area,
    );
}

// -------------------------------------------------------------------- loop

/// Everything the run loop needs to paint one inline view.
pub struct Tui {
    pub app: App,
    terminal: ratatui::DefaultTerminal,
    events: mpsc::UnboundedReceiver<UiEvent>,
    keys: mpsc::UnboundedReceiver<Event>,
    /// When the next working-state tick is due (only armed while working).
    tick: Option<Instant>,
    /// When the last frame went out, for the frame cap.
    last_paint: Option<Instant>,
}

enum Step {
    Ui(UiEvent),
    Key(Event),
    /// The spinner/elapsed/retry tick.
    Tick,
}

/// How long the input thread waits for a key before checking again. A short
/// poll, never a blocking `read`: on Unix the cursor-position query ratatui
/// makes when the window is resized needs the same event-reader lock, and a
/// thread parked in `read()` makes that query time out (and the UI die) the
/// first time the terminal changes size.
const KEY_POLL: Duration = Duration::from_millis(30);

/// Take the terminal (raw mode, panic hook, inline viewport), the keyboard, and
/// the session's event stream. The caller has already checked that stdout is a
/// terminal.
pub fn start(
    events: mpsc::UnboundedReceiver<UiEvent>,
    to_tui: mpsc::UnboundedSender<UiEvent>,
    verbs: mpsc::UnboundedSender<Incoming>,
    banner: &[String],
    cap: Option<u32>,
    theme: Theme,
) -> std::io::Result<Tui> {
    let (width, height) = ratatui::crossterm::terminal::size().unwrap_or((80, VIEWPORT_HEIGHT));
    let terminal = ratatui::try_init_with_options(ratatui::TerminalOptions {
        viewport: ratatui::Viewport::Inline(VIEWPORT_HEIGHT.min(height)),
    })?;
    install_panic_hook();
    // Bracketed paste, so a pasted block of text arrives as one `Paste` event
    // (with its newlines) instead of a burst of keystrokes whose Enter would
    // submit it.
    let _ = ratatui::crossterm::execute!(
        std::io::stdout(),
        ratatui::crossterm::event::EnableBracketedPaste
    );
    let mut app = App::new(
        width as usize,
        height.min(VIEWPORT_HEIGHT) as usize,
        to_tui,
        verbs,
    );
    app.theme = theme;
    for line in banner {
        app.push_text(line);
    }
    // The cap is known before the first turn (it comes from config or the
    // flag), so the idle facts row can already say `step 0/N` instead of a
    // bare `step 0` that would hide the limit until a turn started (review N1).
    app.status.limit = cap;
    let (key_tx, keys) = mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        loop {
            match ratatui::crossterm::event::poll(KEY_POLL) {
                Ok(true) => match ratatui::crossterm::event::read() {
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
                },
                Ok(false) => {}
                Err(_) => return,
            }
        }
    });
    Ok(Tui {
        app,
        terminal,
        events,
        keys,
        tick: None,
        last_paint: None,
    })
}

/// On a panic, do what [`Tui::finish`] does with the last painted viewport
/// (erase it, put the cursor on the next line, show it, turn paste and raw mode
/// off) before ratatui's hook and the original hook print the message.
/// `KELIR_TEST_PANIC` exercises this path in `tests/e2e_tui.rs`.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut stdout = std::io::stdout();
        let top = VIEWPORT_Y.load(Ordering::Relaxed);
        if top != u16::MAX {
            let _ = ratatui::crossterm::execute!(
                stdout,
                MoveTo(0, top),
                Clear(ClearType::FromCursorDown)
            );
        }
        let _ = ratatui::crossterm::execute!(
            stdout,
            ratatui::crossterm::event::DisableBracketedPaste,
            Show,
            ratatui::crossterm::style::Print("\r\n")
        );
        let _ = stdout.flush();
        let _ = ratatui::crossterm::terminal::disable_raw_mode();
        previous(info);
    }));
}

impl Tui {
    /// Draw, then wait for the next key, session event or tick. Returns when the
    /// session says goodbye.
    pub async fn run(mut self) -> std::io::Result<()> {
        loop {
            self.flush_status()?;
            self.flush_scroll()?;
            self.draw()?;
            // A documented, test-only panic with the terminal fully owned (raw
            // mode on, bracketed paste on, a painted viewport): it exercises
            // the restore path a real crash takes (`tests/e2e_tui.rs`).
            if std::env::var_os("KELIR_TEST_PANIC").is_some() {
                panic!("KELIR_TEST_PANIC: injected panic to prove the terminal is restored");
            }
            if self.app.quit {
                break;
            }
            // The tick exists only while the agent works: idle costs no wakeups.
            if !self.app.activity.kind.working() {
                self.tick = None;
            } else if self.tick.is_none() {
                self.tick = Some(Instant::now() + SPINNER_TICK);
            }
            let step: Option<Step> = tokio::select! {
                event = self.events.recv() => event.map(Step::Ui),
                event = self.keys.recv() => event.map(Step::Key),
                _ = wait_until(self.tick), if self.tick.is_some() => Some(Step::Tick),
            };
            match step {
                Some(Step::Ui(event)) => self.drain(event),
                Some(Step::Key(event)) => {
                    self.on_terminal(event);
                    while let Ok(event) = self.keys.try_recv() {
                        self.on_terminal(event);
                    }
                }
                Some(Step::Tick) => {
                    let now = Instant::now();
                    if self.app.tick(now) {
                        self.tick = Some(now + SPINNER_TICK);
                    }
                }
                None => self.app.quit = true,
            }
        }
        self.finish();
        Ok(())
    }

    /// Everything already queued is applied before the next paint, so a burst
    /// of streamed deltas costs one frame, not one frame each. The frame cap
    /// bounds the other direction: a fast provider cannot make the TUI repaint
    /// more often than [`FRAME_MIN`].
    fn drain(&mut self, first: UiEvent) {
        self.app.apply(first);
        let mut applied = 1usize;
        while let Ok(event) = self.events.try_recv() {
            self.app.apply(event);
            applied += 1;
        }
        while let Ok(event) = self.keys.try_recv() {
            self.on_terminal(event);
        }
        // Frame cap: a burst of provider deltas is coalesced into one frame, and
        // two bursts closer together than `FRAME_MIN` are held to it, so a fast
        // provider cannot make the TUI repaint more than ~30 times a second.
        // A single key press is never delayed here.
        if applied > 1
            && let Some(last) = self.last_paint
        {
            let elapsed = last.elapsed();
            if elapsed < FRAME_MIN {
                std::thread::sleep(FRAME_MIN - elapsed);
            }
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
                self.app.paste(&text);
            }
            Event::Resize(width, height) => {
                self.app.set_size(width as usize, height as usize);
                // The terminal's reflow of the old rows is unpredictable; the
                // viewport is cleared from its top row down and redrawn.
                self.redraw_after_resize();
            }
            _ => {}
        }
    }

    fn redraw_after_resize(&mut self) {
        let top = VIEWPORT_Y.load(Ordering::Relaxed);
        if top == u16::MAX {
            return;
        }
        let mut stdout = std::io::stdout();
        let _ =
            ratatui::crossterm::execute!(stdout, MoveTo(0, top), Clear(ClearType::FromCursorDown));
        let _ = stdout.flush();
        let _ = self.terminal.autoresize();
    }

    /// Write the committed rows above the viewport, inside synchronised output
    /// so a frame is never torn.
    fn flush_scroll(&mut self) -> std::io::Result<()> {
        let rows = self.app.take_scroll();
        if rows.is_empty() {
            return Ok(());
        }
        let rows = tail_rows(rows);
        let height = rows.len() as u16;
        let mut stdout = std::io::stdout();
        let _ = ratatui::crossterm::execute!(
            stdout,
            ratatui::crossterm::terminal::BeginSynchronizedUpdate
        );
        let result = self.terminal.insert_before(height, |buffer| {
            Paragraph::new(rows).render(buffer.area, buffer);
        });
        let _ = ratatui::crossterm::execute!(
            stdout,
            ratatui::crossterm::terminal::EndSynchronizedUpdate
        );
        result
    }

    /// Write Herdr's status records between two complete frames, from the task
    /// that paints. Nothing else writes to stdout while the viewport is up, so
    /// an OSC can never land inside one of crossterm's CSI sequences (M2
    /// review F2).
    fn flush_status(&mut self) -> std::io::Result<()> {
        let records = self.app.take_status_records();
        if records.is_empty() {
            return Ok(());
        }
        let mut stdout = std::io::stdout();
        for record in records {
            stdout.write_all(record.as_bytes())?;
        }
        stdout.flush()
    }

    /// One frame, wrapped in synchronised output: terminals that do not know
    /// the sequence ignore it, so there is no capability probing.
    fn draw(&mut self) -> std::io::Result<()> {
        let mut stdout = std::io::stdout();
        let _ = ratatui::crossterm::execute!(
            stdout,
            ratatui::crossterm::terminal::BeginSynchronizedUpdate
        );
        let app = &mut self.app;
        let result = self.terminal.draw(|frame| render(app, frame));
        let _ = ratatui::crossterm::execute!(
            stdout,
            ratatui::crossterm::terminal::EndSynchronizedUpdate
        );
        self.last_paint = Some(Instant::now());
        result.map(|_| ())
    }

    /// Leave the terminal usable: raw mode off, bracketed paste off, the
    /// viewport erased, and the cursor on the first free row below it so the
    /// shell's prompt cannot overwrite a committed line (M2 review F3).
    fn finish(&mut self) {
        let mut stdout = std::io::stdout();
        let _ =
            ratatui::crossterm::execute!(stdout, ratatui::crossterm::event::DisableBracketedPaste);
        // Erase the inline area only: everything above it is the turn history
        // (the banner included) and stays.
        let top = VIEWPORT_Y.load(Ordering::Relaxed);
        if top != u16::MAX {
            let _ = ratatui::crossterm::execute!(
                stdout,
                MoveTo(0, top),
                Clear(ClearType::FromCursorDown)
            );
        }
        // The cursor is at the (now blank) viewport's first row; one newline
        // puts the shell prompt below it. When the viewport reached the bottom
        // row, that newline scrolls the screen once, which is exactly the free
        // line the prompt needs.
        let _ =
            ratatui::crossterm::execute!(stdout, Show, ratatui::crossterm::style::Print("\r\n"));
        let _ = stdout.flush();
        let _ = ratatui::crossterm::terminal::disable_raw_mode();
    }
}

/// Sleep until `at`, or forever when there is no tick (the select arm is
/// guarded by `is_some`).
async fn wait_until(at: Option<Instant>) {
    match at {
        Some(at) => {
            let now = Instant::now();
            if at > now {
                tokio::time::sleep(at - now).await;
            }
        }
        None => std::future::pending().await,
    }
}

fn tail_rows(rows: Vec<Line<'static>>) -> Vec<Line<'static>> {
    if rows.len() <= u16::MAX as usize {
        return rows;
    }
    rows[rows.len() - u16::MAX as usize..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;
    use serde_json::json;

    fn response(ok: bool, verb: &str) -> Response {
        if ok {
            Response::ok(verb, 3, "queued", json!({}))
        } else {
            Response::error("nothing to stop")
        }
    }

    /// A 1280x720 PNG header: enough for the pointer and the sniff.
    fn png_720() -> Vec<u8> {
        let mut png = vec![0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1280u32.to_be_bytes());
        png.extend_from_slice(&720u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png
    }

    fn app() -> App {
        app_of_width(60)
    }

    /// One settled `bash` call, as the session reports it.
    fn tool_settled(command: &str) -> UiEvent {
        UiEvent::ToolSettled {
            name: "bash".into(),
            input: json!({ "command": command }),
            outcome: "ok".into(),
            exit_code: Some(0),
            elapsed_ms: 100,
            bytes: 4,
            lines: 1,
            spilled: false,
            out_path: String::new(),
            tail: String::new(),
        }
    }

    fn app_of_width(width: usize) -> App {
        let (to_tui, _rx) = mpsc::unbounded_channel();
        let (verbs, _vrx) = mpsc::unbounded_channel();
        App::new(width, VIEWPORT_HEIGHT as usize, to_tui, verbs)
    }

    fn row_texts(rows: &[Row]) -> Vec<String> {
        rows.iter().map(Row::text).collect()
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn the_facts_row_names_model_step_tokens_cost_and_ignores_calls() {
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
            turn_steps: 3,
            limit: None,
            phase: Phase::Working,
        };
        assert_eq!(
            status_text(&status),
            "kelir working · deepseek/deepseek-chat · step 3 · 1.5k tok · 75% cache · $0.0004"
        );
        // The state row carries the running count now, so the facts row does
        // not repeat it.
        assert!(!status_text(&status).contains("running"));
        assert_eq!(
            status_text(&Status::default()),
            "kelir idle · step 0 · 0 tok"
        );
    }

    /// A cap is the only time the step count is shown against a limit: with no
    /// cap a counter with nothing to count to is noise.
    #[test]
    fn the_facts_row_shows_steps_used_and_limit_only_when_a_cap_is_set() {
        let mut status = Status {
            model: "m".into(),
            turn_steps: 2,
            limit: Some(5),
            phase: Phase::Working,
            ..Status::default()
        };
        assert!(
            status_text(&status).contains(" · step 2/5 · "),
            "{}",
            status_text(&status)
        );
        status.limit = None;
        status.step = 7;
        assert!(
            status_text(&status).contains(" · step 7 · "),
            "{}",
            status_text(&status)
        );
        assert!(!status_text(&status).contains('/'));
    }

    #[test]
    fn the_facts_row_drops_the_least_useful_piece_first_when_narrow() {
        let status = Status {
            model: "deepseek/deepseek-v4.1-flash-1017".into(),
            step: 12,
            prompt_tokens: 12_000,
            completion_tokens: 100,
            cached_tokens: 10_000,
            cost: 0.0028,
            phase: Phase::Working,
            ..Status::default()
        };
        let wide = facts_for_width(&status, 120);
        assert!(wide.contains("cache"), "{wide}");
        assert!(wide.contains("$0.0028"), "{wide}");
        let narrow = facts_for_width(&status, 30);
        assert!(narrow.contains("tok"), "{narrow}");
        assert!(!narrow.contains("cache"), "{narrow}");
        for width in [60usize, 40, 30, 20, 12] {
            let text = facts_for_width(&status, width);
            assert!(
                scrollback::display_width(&text) <= width,
                "width {width}: {text:?}"
            );
            assert!(
                text.contains("kelir") || text.contains("working"),
                "{text:?}"
            );
        }
    }

    /// A paused turn says so in the state row and in one line of text, and it
    /// keeps the turn's own count for the next look.
    #[test]
    fn a_paused_turn_reads_paused_and_says_how_to_resume() {
        let mut app = app_of_width(80);
        app.apply(UiEvent::TurnStarted {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
            limit: Some(3),
        });
        app.apply(UiEvent::Step {
            step: 3,
            provider: "p".into(),
            model: "m".into(),
            turn_steps: 3,
            limit: Some(3),
        });
        app.apply(UiEvent::Paused { steps: 3 });
        assert_eq!(app.status.phase, Phase::Paused);
        assert_eq!(app.activity.kind, Kind::Paused);
        let row = app.activity.line(app.theme, 0, Instant::now(), 80);
        assert!(line_text(&row).contains("paused"), "{:?}", line_text(&row));
    }

    /// The prompt is dimmed while the agent works and normal when idle, and it
    /// still accepts `say`/`later` (a submitted line is sent either way).
    #[test]
    fn the_prompt_dims_while_working_and_accepts_input_in_both_states() {
        let mut app = app();
        assert_eq!(app.activity.kind, Kind::Idle);
        app.apply(UiEvent::TurnStarted {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
            limit: None,
        });
        assert!(app.activity.kind.working());
        // Submitting while working sends a `say`, exactly like an idle submit.
        app.press(Press::Edit(Key::Char('h')));
        app.press(Press::Edit(Key::Char('i')));
        app.press(Press::Edit(Key::Enter));
        let rows = app.take_scroll();
        assert!(
            rows.iter().any(|row| line_text(row).contains("› hi")),
            "{:?}",
            rows.iter().map(line_text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_streamed_answer_holds_only_the_tail_and_commits_every_row_once() {
        let mut app = app_of_width(40);
        app.apply(UiEvent::TurnStarted {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
            limit: None,
        });
        let words = (0..200).map(|i| format!("word{i} ")).collect::<String>();
        for chunk in words.as_bytes().chunks(20) {
            app.apply(UiEvent::Delta(String::from_utf8_lossy(chunk).into_owned()));
            // The live area never holds more than its height.
            assert!(app.live_view().len() <= app.live_height() + 1);
        }
        app.apply(UiEvent::Assistant {
            text: words.clone(),
        });
        let committed: Vec<String> = app.take_scroll().iter().map(line_text).collect();
        let joined: Vec<&str> = committed.iter().map(String::as_str).collect();
        // Every word survives, exactly once.
        for i in [0usize, 50, 199] {
            let needle = format!("word{i}");
            let count = joined.iter().filter(|row| row.contains(&needle)).count();
            assert_eq!(count, 1, "{needle} appears {count} times");
        }
        assert!(app.live_view().is_empty(), "nothing is left live");
    }

    #[test]
    fn markdown_is_rendered_in_the_live_area_and_committed_with_its_styles() {
        let mut app = app_of_width(60);
        app.apply(UiEvent::Delta("## Files\n\nsome **bold** text\n".into()));
        // A closed block never waits for the message to end (S1): the heading is
        // in the scrollback, and the block still streaming is the live area.
        let committed: Vec<String> = app.take_scroll().iter().map(line_text).collect();
        assert!(committed.iter().any(|row| row == "Files"), "{committed:?}");
        assert!(
            !committed.iter().any(|row| row.contains("##")),
            "{committed:?}"
        );
        let live = row_texts(&app.live_view());
        assert!(
            live.iter().any(|row| row.contains("some bold text")),
            "{live:?}"
        );
        assert!(!live.iter().any(|row| row.contains("**")), "{live:?}");
        app.apply(UiEvent::Assistant {
            text: "## Files\n\nsome **bold** text\n".into(),
        });
        let committed = app.take_scroll();
        let styled = committed
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.style.add_modifier.contains(Modifier::BOLD));
        assert!(styled, "the bold run survives into the scrollback");
    }

    /// The turn shapes the review named: calls then an answer, an answer then
    /// calls, and the two interleaved. The committed rows are in event order in
    /// every case (`B1`) — a model that writes its sentence before running the
    /// call it announces must not have that sentence land above the call.
    #[test]
    fn the_committed_rows_follow_the_order_of_events_in_every_turn_shape() {
        // calls, then prose.
        let mut app = app_of_width(80);
        app.apply(tool_settled("git --version"));
        app.apply(UiEvent::ToolStarted {
            name: "bash".into(),
            input: json!({"command": "python --version"}),
        });
        app.apply(tool_settled("python --version"));
        app.apply(UiEvent::Assistant {
            text: "Both are installed.".into(),
        });
        app.apply(UiEvent::TurnEnded {
            outcome: "done".into(),
            final_text: String::new(),
        });
        let rows: Vec<String> = app.take_scroll().iter().map(line_text).collect();
        let git = rows
            .iter()
            .position(|r| r.contains("git --version"))
            .unwrap();
        let python = rows
            .iter()
            .position(|r| r.contains("python --version"))
            .unwrap();
        let prose = rows.iter().position(|r| r.contains("Both are")).unwrap();
        assert!(git < python && python < prose, "{rows:?}");

        // Prose, then calls, then prose: the piece before the calls stays above
        // them, and the piece after them below, even with no blank line between
        // the pieces (the model streams one paragraph across the call).
        let mut app = app_of_width(80);
        app.apply(UiEvent::Delta("Let me check both versions first.\n".into()));
        app.apply(tool_settled("git --version"));
        app.apply(UiEvent::Delta(
            " Both are installed, so the answer comes after.\n".into(),
        ));
        app.apply(UiEvent::Assistant {
            text: "Let me check both versions first.\n Both are installed, so the answer comes after.\n"
                .into(),
        });
        app.apply(UiEvent::TurnEnded {
            outcome: "done".into(),
            final_text: String::new(),
        });
        let rows: Vec<String> = app.take_scroll().iter().map(line_text).collect();
        let before = rows
            .iter()
            .position(|r| r.contains("Let me check both"))
            .unwrap();
        let call = rows
            .iter()
            .position(|r| r.contains("git --version"))
            .unwrap();
        let after = rows
            .iter()
            .position(|r| r.contains("Both are installed"))
            .unwrap();
        assert!(
            before < call && call < after,
            "event order, not paragraph order: {rows:?}"
        );

        // Calls between two prose blocks, then a fold: still ordered.
        let mut app = app_of_width(80);
        for c in ["a", "b", "c"] {
            app.apply(tool_settled(c));
        }
        app.apply(UiEvent::Delta("Done.".into()));
        app.apply(UiEvent::Assistant {
            text: "Done.".into(),
        });
        let rows: Vec<String> = app.take_scroll().iter().map(line_text).collect();
        let fold = rows
            .iter()
            .position(|r| r.contains("ran 3 commands"))
            .unwrap();
        let prose = rows.iter().position(|r| r.contains("Done.")).unwrap();
        assert!(fold < prose, "{rows:?}");
    }

    /// The design note's spacing rule (review N1): one blank row between a
    /// prompt echo and what follows it, one between tool lines and the prose
    /// after them, and never two in a row.
    #[test]
    fn blocks_of_history_are_separated_by_exactly_one_blank_row() {
        let mut app = app_of_width(60);
        app.press(Press::Edit(Key::Char('h')));
        app.press(Press::Edit(Key::Char('i')));
        app.press(Press::Edit(Key::Enter));
        app.apply(UiEvent::ToolSettled {
            name: "bash".into(),
            input: json!({"command": "ls"}),
            outcome: "ok".into(),
            exit_code: Some(0),
            elapsed_ms: 10,
            bytes: 2,
            lines: 1,
            spilled: false,
            out_path: String::new(),
            tail: String::new(),
        });
        app.apply(UiEvent::Delta("all good\n\nThe end.\n".into()));
        app.apply(UiEvent::Assistant {
            text: "all good\n\nThe end.\n".into(),
        });
        // A second prompt is separated from the block before it.
        app.press(Press::Edit(Key::Char('y')));
        app.press(Press::Edit(Key::Enter));
        let rows: Vec<String> = app.take_scroll().iter().map(line_text).collect();
        let echo = rows
            .iter()
            .position(|row| row.contains("› hi"))
            .expect("echo");
        let tool = rows
            .iter()
            .position(|row| row.contains("ls"))
            .expect("tool");
        let prose = rows
            .iter()
            .position(|row| row.contains("all good"))
            .expect("prose");
        // The design note's rules: a blank row before a user echo (not this
        // one, it opens the scrollback) and before prose after tool lines; none
        // between the echo and the tool lines of the same block.
        assert!(
            rows[echo + 1].contains("$ ls"),
            "a call follows its echo directly: {rows:?}"
        );
        assert_eq!(
            rows[tool + 1],
            "",
            "prose after tools is one row down: {rows:?}"
        );
        let second = rows
            .iter()
            .position(|row| row.contains("› y"))
            .expect("the second echo");
        assert!(
            rows[second - 1].is_empty(),
            "a new prompt starts a block: {rows:?}"
        );
        assert!(echo < tool && tool < prose && prose < second, "{rows:?}");
        // Never two blank rows in a row anywhere in the block.
        for pair in rows.windows(2) {
            assert!(
                !(pair[0].is_empty() && pair[1].is_empty()),
                "two blank rows: {rows:?}"
            );
        }
    }

    #[test]
    fn tool_calls_are_one_dim_line_each_and_three_fold_when_the_group_closes() {
        let mut app = app_of_width(80);
        for i in 0..2 {
            app.apply(UiEvent::ToolSettled {
                name: "bash".into(),
                input: json!({"command": format!("cmd{i}")}),
                outcome: "ok".into(),
                exit_code: Some(0),
                elapsed_ms: 100,
                bytes: 10,
                lines: 1,
                spilled: false,
                out_path: String::new(),
                tail: String::new(),
            });
        }
        // The open group lives in the live area until it closes; nothing is
        // committed yet.
        assert!(app.take_scroll().is_empty());
        let live: Vec<String> = app.live_lines().iter().map(line_text).collect();
        assert!(
            live.iter().any(|row| row.contains("$ cmd0")),
            "the open group is visible: {live:?}"
        );
        // The third call and a prose start close the group as a fold.
        app.apply(UiEvent::ToolSettled {
            name: "bash".into(),
            input: json!({"command": "cmd2"}),
            outcome: "ok".into(),
            exit_code: Some(0),
            elapsed_ms: 100,
            bytes: 10,
            lines: 1,
            spilled: false,
            out_path: String::new(),
            tail: String::new(),
        });
        app.apply(UiEvent::Assistant {
            text: "done".into(),
        });
        let rows = app.take_scroll();
        let texts: Vec<String> = rows.iter().map(line_text).collect();
        assert!(
            texts.iter().any(|row| row.contains("ran 3 commands")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_failed_call_is_red_and_carries_one_tail_line() {
        let mut app = app_of_width(80);
        app.apply(UiEvent::ToolSettled {
            name: "bash".into(),
            input: json!({"command": "cargo test"}),
            outcome: "error".into(),
            exit_code: Some(101),
            elapsed_ms: 4100,
            bytes: 400,
            lines: 2,
            spilled: false,
            out_path: String::new(),
            tail: "error[E0432]: unresolved import `foo`".into(),
        });
        app.apply(UiEvent::Assistant { text: "x".into() });
        let rows = app.take_scroll();
        let texts: Vec<String> = rows.iter().map(line_text).collect();
        let call = texts
            .iter()
            .find(|row| row.contains("cargo test"))
            .expect("the call line");
        assert!(call.contains("✗ exit 101"), "{call:?}");
        let tail = texts
            .iter()
            .find(|row| row.contains("error[E0432]"))
            .expect("the tail line");
        assert!(tail.starts_with("    "), "{tail:?}");
        let red = rows.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.style.fg == Some(ratatui::style::Color::Red))
        });
        assert!(red, "the failure is emphasised by colour");
    }

    #[test]
    fn expand_appends_output_and_the_two_commands_set_the_policy_each_in_one_direction() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::write(&out, "line one\nline two\n").unwrap();
        let mut app = app_of_width(80);
        app.apply(UiEvent::ToolSettled {
            name: "bash".into(),
            input: json!({"command": "ls"}),
            outcome: "ok".into(),
            exit_code: Some(0),
            elapsed_ms: 10,
            bytes: 18,
            lines: 2,
            spilled: false,
            out_path: out.to_string_lossy().into_owned(),
            tail: String::new(),
        });
        app.take_scroll();
        app.expand(None);
        let texts: Vec<String> = app.take_scroll().iter().map(line_text).collect();
        assert!(texts.iter().any(|row| row.contains("#1 $ ls")), "{texts:?}");
        assert!(
            texts.iter().any(|row| row.contains("line one")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|row| row.contains("cannot unfold")),
            "{texts:?}"
        );
        // No such call: a note, not a panic.
        app.expand(Some(9));
        let texts: Vec<String> = app.take_scroll().iter().map(line_text).collect();
        assert!(
            texts.iter().any(|row| row.contains("no call #9")),
            "{texts:?}"
        );
        // The policy changes what future calls do, and each command has one
        // direction: `/expand` never collapses and `/collapse` never expands,
        // however often either is said (S6).
        app.set_policy(Policy::Full);
        assert_eq!(app.policy, Policy::Full);
        app.submit("/expand");
        assert_eq!(app.policy, Policy::Full);
        app.submit("/collapse");
        assert_eq!(app.policy, Policy::Collapsed);
        app.submit("/collapse");
        assert_eq!(app.policy, Policy::Collapsed, "idempotent");
        app.submit("/expand");
        assert_eq!(app.policy, Policy::Full, "and it does expand");
        app.submit("/expand");
        assert_eq!(app.policy, Policy::Full, "idempotent the other way");
        app.submit("/expand last");
        assert_eq!(app.policy, Policy::Full, "naming a call only appends");
    }

    #[test]
    fn the_turn_summary_line_lands_before_the_row_goes_idle() {
        let mut app = app_of_width(80);
        app.apply(UiEvent::TurnStarted {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
            limit: None,
        });
        app.apply(UiEvent::Step {
            step: 1,
            provider: "p".into(),
            model: "m".into(),
            turn_steps: 1,
            limit: None,
        });
        app.apply(UiEvent::Usage {
            prompt: 1000,
            completion: 200,
            cached: 900,
            cost: 0.001,
        });
        app.apply(UiEvent::Assistant { text: "hi".into() });
        app.apply(UiEvent::TurnEnded {
            outcome: "done".into(),
            final_text: "hi".into(),
        });
        assert_eq!(app.activity.kind, Kind::Idle);
        assert_eq!(app.status.phase, Phase::Done);
        let texts: Vec<String> = app.take_scroll().iter().map(line_text).collect();
        let summary = texts
            .iter()
            .find(|row| row.contains("tok") && row.contains("$0.0010"))
            .expect("the summary line");
        assert!(summary.contains("✓"), "{summary:?}");
        assert!(summary.contains("1 step"), "{summary:?}");
        assert!(!summary.contains("1 steps"), "{summary:?}");
    }

    #[test]
    fn a_pasted_image_path_becomes_a_chip_and_the_prompt_gets_the_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_of_width(80);
        app.set_session_dir(dir.path().to_path_buf());
        let image = dir.path().join("shot.png");
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1280u32.to_be_bytes());
        png.extend_from_slice(&720u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        std::fs::write(&image, &png).unwrap();
        app.press(Press::Edit(Key::Char('w')));
        app.paste(&format!("\"{}\"", image.display()));
        // The editor holds the chip, not the path.
        assert_eq!(app.editor.text(), "w [image 1] ");
        assert!(
            app.editor.chips()[0]
                .pointer
                .contains("attachments/1-shot.png")
        );
        assert!(app.editor.chips()[0].pointer.contains("1280x720"));
        assert!(dir.path().join("attachments/1-shot.png").is_file());
        // Submitting expands the chip to the pointer: the model never sees the
        // label or the absolute path.
        app.press(Press::Edit(Key::Enter));
        let texts: Vec<String> = app.take_scroll().iter().map(line_text).collect();
        let echoed = texts
            .iter()
            .find(|row| row.contains("what the model sees") || row.contains("attachments/1-"))
            .expect("the submitted line");
        assert!(
            echoed.contains("[image: attachments/1-shot.png, 1280x720"),
            "{echoed:?}"
        );
        assert!(!echoed.contains("what the"), "{echoed:?}");
    }

    /// The maintainer's platform: on Windows a terminal's paste arrives as
    /// keystrokes (crossterm decodes `ESC[200~` only on Unix). The chip must
    /// appear when the path has been typed, as it does after a bracketed paste —
    /// not only at submit, and not as raw path text (`S8`).
    #[test]
    fn a_typed_image_path_burst_becomes_a_chip_at_paste_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_of_width(100);
        app.set_session_dir(dir.path().to_path_buf());
        let image = dir.path().join("shot.png");
        std::fs::write(&image, png_720()).unwrap();
        for c in "look ".chars() {
            app.press(Press::Edit(Key::Char(c)));
        }
        // Every keystroke of the path, exactly as a terminal sends it.
        for c in image.display().to_string().chars() {
            app.press(Press::Edit(Key::Char(c)));
        }
        assert_eq!(app.editor.chips().len(), 1, "{:?}", app.editor.text());
        assert_eq!(app.editor.text(), "look [image 1] ");
        assert!(dir.path().join("attachments/1-shot.png").is_file());
        // Backspace removes the chip whole, and its file with it.
        app.press(translate(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)).unwrap());
        assert!(app.editor.chips().is_empty());
        assert_eq!(app.editor.text(), "look ");
        assert_eq!(
            std::fs::read_dir(dir.path().join("attachments"))
                .map(|entries| entries.count())
                .unwrap_or(0),
            0,
            "the chip's file went too"
        );
    }

    /// A typed path that is only half a file stays text until it is a file: a
    /// prefix of a real path is not an image, and nothing is saved for it.
    #[test]
    fn a_partial_typed_path_is_still_text_and_a_quote_comes_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_of_width(100);
        app.set_session_dir(dir.path().to_path_buf());
        let image = dir.path().join("my shot.png");
        std::fs::write(&image, png_720()).unwrap();
        let full = image.display().to_string();
        for c in full[..full.len() - 2].chars() {
            app.press(Press::Edit(Key::Char(c)));
        }
        assert!(app.editor.chips().is_empty(), "{:?}", app.editor.text());
        assert!(
            app.editor.text().trim_end().ends_with(".p"),
            "{:?}",
            app.editor.text()
        );
        app.press(Press::Edit(Key::Clear));
        // A quoted path with a space: the quote pair goes with the chip.
        for c in format!("\"{}\"", full).chars() {
            app.press(Press::Edit(Key::Char(c)));
        }
        assert_eq!(app.editor.chips().len(), 1, "{:?}", app.editor.text());
        assert!(
            app.editor.text().trim_end().ends_with("[image 1]"),
            "{:?}",
            app.editor.text()
        );
        assert!(
            !app.editor.text().contains("my shot"),
            "{:?}",
            app.editor.text()
        );
    }

    #[test]
    fn a_paste_that_is_not_an_image_path_is_text() {
        let mut app = app_of_width(80);
        app.paste("just some text\nwith two lines");
        assert_eq!(app.editor.text(), "just some text\nwith two lines");
        assert!(app.editor.chips().is_empty());
    }

    #[test]
    fn removing_an_unsent_chip_deletes_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_of_width(80);
        app.set_session_dir(dir.path().to_path_buf());
        let image = dir.path().join("shot.png");
        std::fs::write(&image, b"\x89PNG\r\n\x1a\nrest").unwrap();
        app.paste(&image.display().to_string());
        let saved = dir.path().join("attachments");
        assert!(saved.read_dir().unwrap().next().is_some());
        app.press(Press::Edit(Key::RemoveChip));
        assert!(app.editor.chips().is_empty());
        assert_eq!(
            saved.read_dir().unwrap().count(),
            0,
            "the file went with the chip"
        );
    }

    #[test]
    fn a_missing_session_dir_leaves_a_pasted_path_as_text() {
        let mut app = app_of_width(80);
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("shot.png");
        std::fs::write(&image, b"x").unwrap();
        app.paste(&image.display().to_string());
        assert!(
            app.editor.text().contains("shot.png"),
            "no session dir: the path is text: {:?}",
            app.editor.text()
        );
        assert!(app.editor.chips().is_empty());
    }

    #[test]
    fn esc_stops_only_while_working() {
        let mut app = app_of_width(80);
        // Idle: nothing to stop, no line.
        app.press(Press::Stop);
        assert!(app.take_scroll().is_empty());
        app.apply(UiEvent::TurnStarted {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
            limit: None,
        });
        app.press(Press::Stop);
        let texts: Vec<String> = app.take_scroll().iter().map(line_text).collect();
        assert!(texts.iter().any(|row| row.contains("stop")), "{texts:?}");
    }

    #[test]
    fn a_retry_event_puts_the_countdown_in_the_state_row() {
        let mut app = app_of_width(100);
        app.apply(UiEvent::TurnStarted {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
            limit: None,
        });
        app.apply(UiEvent::Retry {
            retry_in_ms: 34_000,
            attempt: 5,
            detail: "429".into(),
        });
        assert_eq!(app.activity.kind, Kind::RateLimited);
        let row = line_text(&app.activity.line(app.theme, 0, Instant::now(), 100));
        assert!(row.contains("rate limited"), "{row:?}");
        assert!(row.contains("retrying in 34 s"), "{row:?}");
        assert!(row.contains("attempt 5"), "{row:?}");
        // A following step leaves the countdown behind.
        app.apply(UiEvent::Step {
            step: 2,
            provider: "p".into(),
            model: "m".into(),
            turn_steps: 2,
            limit: None,
        });
        assert_eq!(app.activity.kind, Kind::Thinking);
    }

    #[test]
    fn an_attachment_event_is_one_dim_view_line() {
        let mut app = app_of_width(80);
        app.apply(UiEvent::Attachment {
            rel: "attachments/1-shot.png".into(),
            bytes: 319_488,
        });
        let texts: Vec<String> = app.take_scroll().iter().map(line_text).collect();
        assert!(
            texts
                .iter()
                .any(|row| row.contains("view attachments/1-shot.png") && row.contains("312 KB")),
            "{texts:?}"
        );
    }

    #[test]
    fn the_tick_only_advances_while_working() {
        let mut app = app_of_width(80);
        assert!(!app.tick(Instant::now()), "idle costs no repaint");
        app.apply(UiEvent::TurnStarted {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
            limit: None,
        });
        assert!(app.tick(Instant::now()));
        app.apply(UiEvent::TurnEnded {
            outcome: "done".into(),
            final_text: String::new(),
        });
        assert!(!app.tick(Instant::now()));
    }

    /// An inline viewport does not start at row 0: the rows must be positioned
    /// from the frame's own origin, or every row is clipped (a real terminal
    /// showed a stale screen because of exactly this).
    #[test]
    fn every_row_is_positioned_from_the_frames_own_origin() {
        for top in [0u16, 6, 40] {
            let areas = layout(Rect::new(3, top, 80, 8));
            assert_eq!(areas.live.y, top, "live");
            assert_eq!(areas.state.y, top + 3, "state");
            assert_eq!(areas.editor.y, top + 4, "editor");
            assert_eq!(areas.facts.y, top + 7, "facts");
            for row in [areas.live, areas.state, areas.editor, areas.facts] {
                assert_eq!(row.x, 3, "x follows the frame");
                assert!(row.bottom() <= top + 8, "row inside the frame: {row:?}");
            }
        }
    }

    #[test]
    fn the_eight_rows_are_three_live_a_state_row_three_editor_rows_and_the_facts() {
        let areas = layout(Rect::new(0, 0, 80, 8));
        assert_eq!(areas.live.y, 0);
        assert_eq!(areas.live.height, 3);
        assert_eq!(areas.state.height, 1);
        assert_eq!(areas.editor.height, 3);
        assert_eq!(areas.facts.height, 1);
        assert_eq!(areas.state.y, areas.live.y + 3);
        assert_eq!(areas.editor.y, 4);
        assert_eq!(areas.facts.y, 7);
        // A shorter screen drops the live rows first, then the facts row; the
        // state row is the last to go.
        for height in [7u16, 5, 3, 2] {
            let areas = layout(Rect::new(0, 0, 40, height));
            let areas_sum =
                areas.live.height + areas.state.height + areas.editor.height + areas.facts.height;
            assert_eq!(areas_sum, height, "height {height}");
            assert!(areas.state.height > 0 || height == 0, "height {height}");
        }
    }

    #[test]
    fn an_escape_stops_and_ctrl_o_and_ctrl_l_keep_their_meaning() {
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        assert_eq!(
            translate(key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Press::Stop)
        );
        assert_eq!(
            translate(key(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            Some(Press::ToggleTail)
        );
        assert_eq!(
            translate(key(KeyCode::Char('l'), KeyModifiers::CONTROL)),
            Some(Press::Redraw)
        );
        assert_eq!(
            translate(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Press::Edit(Key::Enter))
        );
        let mut release = key(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(translate(release), None);
    }

    #[test]
    fn a_reply_that_is_not_quiet_is_echoed_and_a_refusal_is_a_note() {
        let mut app = app();
        app.apply(UiEvent::Reply(response(true, "compact")));
        let texts: Vec<String> = app.take_scroll().iter().map(line_text).collect();
        assert!(
            texts.iter().any(|row| row.contains("compact: queued")),
            "{texts:?}"
        );
        app.apply(UiEvent::Reply(response(false, "stop")));
        let texts: Vec<String> = app.take_scroll().iter().map(line_text).collect();
        assert!(
            texts
                .iter()
                .any(|row| row.contains("kelir: nothing to stop")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_running_call_label_names_the_tool_and_its_first_line_never_json() {
        assert_eq!(
            call_label("bash", &json!({"command": "git --version"})),
            "bash: git --version"
        );
        assert_eq!(
            call_label(
                "bash",
                &json!({"command": "cargo test
&& cargo fmt"})
            ),
            "bash: cargo test"
        );
        assert_eq!(
            call_label("edit", &json!({"path": "src/lib.rs"})),
            "edit: src/lib.rs"
        );
        // An unknown tool with a big blob is just its name: the state row is
        // not the place for JSON.
        let blob = json!({"weird": "x".repeat(500)});
        assert_eq!(call_label("mytool", &blob), "mytool");
        assert!(!call_label("mytool", &blob).contains('{'));
    }

    #[test]
    fn the_last_line_helper_skips_blank_trailing_lines() {
        assert_eq!(last_line("a\nb\n\n"), "b");
        assert_eq!(last_line(""), "");
        assert_eq!(last_line("\n \n"), "");
    }
}
