//! The agent's markdown, rendered into styled rows.
//! Three pieces, each testable alone:

use super::render::Renderer;
use super::*;

// ---------------------------------------------------------------- streaming

/// A block the splitter has finished, and how many of its rows were already committed while it streamed
/// (`S1b`): the TUI continues the block from there instead of writing those rows again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closed {
    pub text: String,
    pub delivered: usize,
}

impl From<Closed> for String {
    fn from(closed: Closed) -> Self {
        closed.text
    }
}

/// Splits streamed markdown into complete blocks.
#[derive(Debug, Default)]
pub struct Stream {
    /// The block being collected, its last line included but without that
    /// line's newline.
    open: String,
    /// The open fence's marker (` ``` ` or `~~~ ` plus its info string), while
    /// inside one.
    in_fence: Option<String>,
    /// The open block's last line looks like a table row, so a following row
    /// belongs to it.
    in_table: bool,
    /// The line being collected (the last line of `open`, without its newline): the block's
    /// structure is decided from this, never from the whole block.
    line: String,
    /// What the open block is (fence, list, prose): decided once its first line
    /// is complete, and what decides which of its rows are final early (`S1b`).
    shape: Option<Shape>,
    /// How many bytes of `open` are already out, and in how many rows: what the
    /// block is continued from when it closes, so no row is written twice.
    delivered_bytes: usize,
    delivered_rows: usize,
}

impl Stream {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, delta: &str) -> Vec<String> {
        let mut done: Vec<String> = Vec::new();
        for line in delta.split_inclusive('\n') {
            let newline = line.ends_with('\n');
            let text = line.trim_end_matches('\n');
            self.open.push_str(text);
            self.line.push_str(text);
            if newline {
                self.line_ended(&mut done);
            }
        }
        done
    }

    /// Feed a delta; returns the blocks it completed, each with the rows of it
    /// that were already delivered while it streamed.
    pub fn push_blocks(&mut self, delta: &str) -> Vec<Closed> {
        let mut done = Vec::new();
        for line in delta.split_inclusive('\n') {
            let newline = line.ends_with('\n');
            let text = line.trim_end_matches('\n');
            self.open.push_str(text);
            self.line.push_str(text);
            if newline {
                self.line_ended(&mut done);
            }
        }
        done
    }

    fn line_ended<T: From<Closed>>(&mut self, done: &mut Vec<T>) {
        let line = std::mem::take(&mut self.line);
        if let Some(fence) = self.in_fence.clone() {
            if opens_or_closes(&line, &fence) {
                self.in_fence = None;
                // The closing fence belongs to this block.
                self.hand_over(done);
                self.in_table = false;
            } else {
                self.open.push('\n');
            }
            return;
        }
        if let Some(fence) = fence_at(&line) {
            self.in_fence = Some(fence);
            self.open.push('\n');
            return;
        }
        if line.trim().is_empty() {
            // A blank line closes the block. A table's blank line closes it too: the table is complete,
            // and its widths need every row it has, which it now has.
            if !self.open.trim().is_empty() {
                self.hand_over(done);
            }
            self.open.clear();
            self.reset_block();
            self.in_table = false;
            return;
        }
        self.in_table = is_table_row(&line);
        self.open.push('\n');
    }

    /// The open block's text, exactly as it arrived (no synthetic closer).
    pub fn open(&self) -> &str {
        &self.open
    }

    /// The block that just closed: its text, and the rows of it that are already
    /// out (committed while it streamed).
    fn hand_over<T: From<Closed>>(&mut self, done: &mut Vec<T>) {
        let rows = self.delivered_rows;
        self.delivered_bytes = 0;
        self.delivered_rows = 0;
        let text = std::mem::take(&mut self.open);
        self.reset_block();
        done.push(T::from(Closed {
            text,
            delivered: rows,
        }));
    }

    /// A new block starts: nothing of the previous one carries over.
    fn reset_block(&mut self) {
        self.shape = None;
        self.delivered_bytes = 0;
        self.delivered_rows = 0;
    }

    /// What the open block is, once its first line is complete.
    pub fn shape(&self) -> Option<&Shape> {
        self.shape.as_ref()
    }

    /// How many bytes of the open block are already out.
    pub fn delivered_bytes(&self) -> usize {
        self.delivered_bytes
    }

    /// The rows of the open block that can be committed now (`S1b`): a fence's finished lines and a top-
    /// level list's finished items, keeping `keep` complete lines behind for the live area.
    pub fn settle(&mut self, layout: Layout, keep: usize) -> Vec<Row> {
        if self.shape.is_none() {
            self.shape = Shape::of(&self.open);
        }
        let Some(shape) = self.shape.clone() else {
            return Vec::new();
        };
        let in_fence = self.in_fence.is_some();
        let Some(cut) = stable_cut(&self.open, self.delivered_bytes, &shape, in_fence, keep) else {
            return Vec::new();
        };
        if cut <= self.delivered_bytes {
            return Vec::new();
        }
        let rows = piece_rows(&self.open, self.delivered_bytes, cut, &shape, layout);
        self.delivered_bytes = cut;
        self.delivered_rows += rows.len();
        rows
    }

    pub fn delivered_rows(&self) -> usize {
        self.delivered_rows
    }

    /// The block being streamed: rendered tolerantly (an unclosed fence shows
    /// as code, an unclosed `**` stays literal).
    pub fn tail(&self) -> String {
        let text = self.open.trim_end_matches('\n');
        if self.in_fence.is_some() {
            // An unfinished fence still renders as code: give the renderer the closing fence the stream
            // has not sent yet. One small string per frame is cheaper than leaking one per frame.
            format!(
                "{text}
```"
            )
        } else {
            text.to_string()
        }
    }

    /// The part of the open block not yet committed, rendered tolerantly.
    /// Keeping only this tail in the live draft makes a long stream linear in
    /// the number of deltas rather than quadratic in the message length.
    pub fn tail_remaining(&self) -> String {
        let mut text = self
            .open
            .get(self.delivered_bytes..)
            .unwrap_or("")
            .trim_end_matches('\n');
        // A paragraph's wrap depends on all its text, so `settle` cannot commit
        // rows early. Keep only its tail in the live draft: the whole block
        // lands in the transcript when it closes, and a huge prose delta does
        // not re-render a growing megabyte on every frame.
        if !text.is_empty()
            && self
                .shape
                .as_ref()
                .is_none_or(|s| matches!(s, Shape::Whole))
        {
            let mut cut = 0usize;
            for (i, _) in text.char_indices().rev() {
                if text.len() - i > 4096 {
                    cut = i;
                    break;
                }
            }
            text = &text[cut..];
        }
        if self.in_fence.is_some() {
            format!("{text}\n```")
        } else {
            text.to_string()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.open.trim().is_empty()
    }

    /// The stream is complete: whatever is left is the last block.
    pub fn finish(&mut self) -> Option<String> {
        self.finish_block().map(|closed| closed.text)
    }

    /// The stream is complete: the last block, with the rows of it that already
    /// went out while it streamed.
    pub fn finish_block(&mut self) -> Option<Closed> {
        self.in_fence = None;
        self.in_table = false;
        self.shape = None;
        if self.open.trim().is_empty() {
            self.open.clear();
            self.reset_block();
            return None;
        }
        let rows = self.delivered_rows;
        self.delivered_bytes = 0;
        self.delivered_rows = 0;
        Some(Closed {
            text: std::mem::take(&mut self.open),
            delivered: rows,
        })
    }
}

/// The fence an opening line starts (` ```rust ` → ` ```rust `), if it is one.
fn fence_at(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    for marker in ["```", "~~~"] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(format!("{marker}{}", rest.trim()));
        }
    }
    None
}

/// Is this line the closing fence of `open`? A closing fence repeats the
/// marker and carries no info string.
fn opens_or_closes(line: &str, open: &str) -> bool {
    let trimmed = line.trim_start();
    let marker = if open.starts_with("~~~") {
        "~~~"
    } else {
        "```"
    };
    match trimmed.strip_prefix(marker) {
        Some(rest) => rest.trim().is_empty(),
        None => false,
    }
}

/// Does this block end inside a list item that a following, indented block continues? (The renderer
/// knows: the item's container is still open at the end of the block — a loose item's blank line does
pub fn ends_in_item(text: &str) -> bool {
    // The splitter's cut is a blank line: the one place where CommonMark's *loose* list rule matters.
    let last = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    opens_item(text) || last.starts_with("  ") || last.starts_with("	")
}

/// Does this block open with a list item (`- x`, `* x`, `1. x`, an ordered
/// item)? Two of those in a row are two lists, never one item's continuation.
pub fn opens_item(block: &str) -> bool {
    let line = block
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let trimmed = line.trim_start();
    let mut chars = trimmed.chars().peekable();
    match chars.peek() {
        Some('-') | Some('*') | Some('+') => {
            chars.next();
            chars.next().is_some_and(char::is_whitespace)
        }
        Some(c) if c.is_ascii_digit() => {
            let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
            let rest = &trimmed[digits.len()..];
            rest.starts_with(". ") || rest.starts_with(") ")
        }
        _ => false,
    }
}

/// A line that looks like a table row: pipes on both ends.
fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() > 1 && trimmed.starts_with('|') && trimmed.ends_with('|')
}

/// Is this text a table, or the start of one? A boxed table's column widths are the widest
/// cell of every column, so no row of it is final before its last row has arrived.
pub fn looks_like_table(text: &str) -> bool {
    text.lines().any(is_table_row)
}

/// Does this line open a list item at column 0? An item's own continuation (an
/// indented line) is not one: its rows depend on the item it belongs to.
fn is_top_item(line: &str) -> bool {
    !line.starts_with([' ', '\t']) && opens_item(line)
}

/// The shape of a block being streamed: which of its rows are final before the
/// block ends (`S1b`), and how a piece of it renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// A fence. Every completed line is one row (code is clipped, never
    /// wrapped), so a line that ended can go out before the closing fence.
    Fence { marker: String, label: String },
    /// A top-level list. An item that ended cannot change: its wrapping and its
    /// hanging indent depend on that item alone.
    List,
    /// Prose or a table: no row is final until the block ends (a paragraph's
    /// wrap depends on all of it, a table's widths on every row it has).
    Whole,
}

impl Shape {
    /// The shape of a block from its lines so far.
    pub fn of(text: &str) -> Option<Shape> {
        let head = text.split_inclusive('\n').next().unwrap_or_default();
        let line = head.strip_suffix('\n')?;
        if let Some(marker) = fence_at(line) {
            let marker: String = marker.chars().take(3).collect();
            let trimmed = line.trim_start();
            let label = trimmed[marker.len()..].trim().to_string();
            return Some(Shape::Fence { marker, label });
        }
        if is_top_item(line) {
            return Some(Shape::List);
        }
        Some(Shape::Whole)
    }
}

/// The bytes of a fence's opening line, marker and label: the block's first
/// line is a label row, not code.
fn opening_len(text: &str) -> usize {
    text.find('\n').map_or(0, |at| at + 1)
}

/// How many of the `text`'s completed lines are whole, and where each of them
/// ends.
fn complete_lines(text: &str) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        if line.ends_with('\n') {
            ends.push(offset);
        }
    }
    ends
}

/// How much of `text` (an open block, from `from` on) is final, keeping at least `keep`
/// complete lines after the cut for the live area.
pub fn stable_cut(
    text: &str,
    from: usize,
    shape: &Shape,
    in_fence: bool,
    keep: usize,
) -> Option<usize> {
    let rest = text.get(from..)?;
    if !in_fence && looks_like_table(rest) {
        return None;
    }
    let ends = complete_lines(rest);
    let keep = keep.max(1);
    match shape {
        Shape::Fence { .. } => {
            // Every complete line but the last `keep` (the one being typed is
            // not final, and a couple of rows stay for the live area).
            let index = ends.len().checked_sub(keep + 1)?;
            Some(from + ends[index])
        }
        Shape::List if !in_fence => {
            // The cut is just before a top-level item, with `keep` complete
            // lines after it: every item before that one can never change.
            let lines: Vec<&str> = rest.split_inclusive('\n').collect();
            let mut cut = None;
            for (index, end) in ends.iter().enumerate() {
                if ends.len() - 1 - index < keep {
                    break;
                }
                if lines
                    .get(index + 1)
                    .is_some_and(|next| next.strip_suffix('\n').is_some_and(is_top_item))
                {
                    cut = Some(from + end);
                }
            }
            cut
        }
        _ => None,
    }
}

/// How many top-level ordered items `text` holds: the numbering a piece cut out
/// of the middle of a long list counts on from.
pub fn ordered_items(text: &str) -> u64 {
    text.lines()
        .filter(|line| !line.starts_with([' ', '\t']))
        .filter(|line| {
            let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
            !digits.is_empty()
                && line[digits.len()..].starts_with(['.', ')'])
                && line[digits.len() + 1..].starts_with(' ')
        })
        .count() as u64
}

/// The number a list's first ordered item carries (`3. x` → 3, `- x` → 1).
fn list_start(text: &str) -> u64 {
    let line = text.lines().next().unwrap_or_default();
    let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(1)
}

/// The rows of the block `text[from..to]` — a piece of a block that is being streamed (`to` a
/// cut), or all of what is left when the block closes (`to == text.len()`).
pub fn piece_rows(text: &str, from: usize, to: usize, shape: &Shape, layout: Layout) -> Vec<Row> {
    let Some(piece) = text.get(from.min(text.len())..to.min(text.len())) else {
        return Vec::new();
    };
    match shape {
        Shape::Fence { marker, label } => {
            let (label, body) = if from == 0 {
                (
                    label.as_str(),
                    &piece[opening_len(piece).min(piece.len())..],
                )
            } else {
                ("", piece)
            };
            let mut text = String::with_capacity(marker.len() + label.len() + body.len() + 4);
            text.push_str(marker);
            text.push_str(label);
            text.push('\n');
            text.push_str(body);
            if !body.is_empty() && !body.ends_with('\n') {
                text.push('\n');
            }
            // A fence that has not closed yet still renders as code: give the renderer the closing line
            // the model has not sent, so the rows are the ones the block will have.
            text.push_str(marker);
            render_block(&text, layout)
        }
        Shape::List if from > 0 => {
            let first = list_start(text) + ordered_items(&text[..from]);
            Renderer::render_with(piece, layout, Some(first))
        }
        _ => render_block(piece, layout),
    }
}

/// The rows of the open block that are not out yet: what the live area shows while a block streams.
pub fn open_rows(text: &str, shape: Option<&Shape>, delivered: usize, layout: Layout) -> Vec<Row> {
    let from = delivered.min(text.len());
    match shape {
        Some(shape) => piece_rows(text, from, text.len(), shape, layout),
        // The block's first line is still arriving: render it tolerantly.
        None => render_block(text.trim_end_matches('\n'), layout),
    }
}
