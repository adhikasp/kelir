//! The TUI's state. Every row on screen comes from the events applied here;
//! the transcript is owned by the app and rewrapped on resize, so the terminal
//! never drops or reflows a prompt or an answer.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use ratatui::text::Line;
use tokio::sync::{mpsc, oneshot};

use crate::features::attachments;
use crate::features::herdr;
use crate::ipc::{Incoming, Request};
use crate::tui::activity::{self, Activity, Kind};
use crate::tui::commands::{self, Action, Parsed};
use crate::tui::editor::{Chip, Editor, Key, Outcome};
use crate::tui::markdown::Stream;
use crate::tui::theme::Theme;
use crate::tui::tools_view::{self, CallLine};

use super::keys::Press;
use super::view;

use super::{Status, View};

mod entry;
mod events;
mod input;
mod layout;

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
    pub(super) fn text(&self) -> String {
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
    /// `/new` ended this session: the process leaves and starts a fresh one in
    /// the same place instead of returning to the shell.
    pub restart: bool,
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
            restart: false,
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

fn payload_text(value: &crate::log::event::Payload) -> String {
    match value {
        crate::log::event::Payload::Inline(value) => match value {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        },
        crate::log::event::Payload::Blob(reference) => reference.preview.clone(),
    }
}

fn payload_value(value: &crate::log::event::Payload) -> serde_json::Value {
    match value {
        crate::log::event::Payload::Inline(inner) => inner.clone(),
        crate::log::event::Payload::Blob(reference) => {
            serde_json::json!({"path": reference.path, "bytes": reference.bytes})
        }
    }
}
