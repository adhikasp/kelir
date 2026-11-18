//! Turning text into scrollback rows, and deciding what stays in the inline
//! area.
//!
//! Two pure pieces: [`wrap`] breaks a string into terminal rows of a given
//! width (wide characters count two columns, a break lands on a space when
//! there is one), and [`Live`] tracks a streamed message so every row is
//! committed to the scrollback exactly once — the rows that no longer fit in
//! the live area go first, the rest when the message is finished.

use unicode_width::UnicodeWidthChar;

fn width_of(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// One character's display width (two columns for CJK, zero for combining
/// marks and control characters).
pub fn char_width(c: char) -> usize {
    width_of(c)
}

/// `Line`'s display width, without allocating.
pub fn display_width(line: &str) -> usize {
    line.chars().map(width_of).sum()
}

/// Break `text` into rows no wider than `width` columns. Newlines are row
/// breaks; a break prefers the last space on the row; trailing spaces are
/// dropped; a row that is too narrow for one character still gets it.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.split('\n') {
        wrap_line(line, width, &mut out);
    }
    out
}

fn wrap_line(line: &str, width: usize, out: &mut Vec<String>) {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        out.push(String::new());
        return;
    }
    let mut start = 0usize;
    let mut last_space: Option<usize> = None;
    let mut column = 0usize;
    let mut index = 0usize;
    while index < chars.len() {
        let char_width = width_of(chars[index]);
        // A space is only a break opportunity, never the reason to break: a row
        // may hold exactly `width` columns and the space after it belongs to the
        // break (it is dropped, not printed).
        if column + char_width > width && index > start && chars[index] != ' ' {
            let end = match last_space {
                Some(space) if space > start => space,
                _ => index,
            };
            let row: String = chars[start..end].iter().collect();
            out.push(row.trim_end().to_string());
            start = end;
            if last_space == Some(end) {
                start += 1;
            }
            while start < chars.len() && chars[start] == ' ' {
                start += 1;
            }
            last_space = None;
            column = 0;
            index = start;
            continue;
        }
        if chars[index] == ' ' {
            last_space = Some(index);
        }
        column += char_width;
        index += 1;
    }
    let row: String = chars[start..].iter().collect();
    out.push(row.trim_end().to_string());
}

/// A streamed message and the row the scrollback has reached.
///
/// Wrapping is incremental: each character is fed through the same break rules
/// as [`wrap`] exactly once, and rows that can no longer change are kept. Only
/// a resize re-wraps the whole message. Re-wrapping the whole text on every
/// frame was O(n²) in the length of one answer (M2 review N4).
#[derive(Debug, Clone)]
pub struct Live {
    /// The whole message, kept only so a resize can re-wrap it.
    text: String,
    width: usize,
    rows: usize,
    /// Rows already handed to the scrollback.
    emitted: usize,
    /// Rows of complete text: no future delta can change them.
    settled: Vec<String>,
    /// The line being written, its column count, and the last space in it.
    open: Vec<char>,
    column: usize,
    last_space: Option<usize>,
    /// Characters fed through the wrapper (a test asserts it is linear).
    #[cfg(test)]
    wrapped: usize,
}

impl Live {
    pub fn new(width: usize, rows: usize) -> Self {
        Self {
            text: String::new(),
            width: width.max(1),
            rows: rows.max(1),
            emitted: 0,
            settled: Vec::new(),
            open: Vec::new(),
            column: 0,
            last_space: None,
            #[cfg(test)]
            wrapped: 0,
        }
    }

    /// A resize re-wraps once, from scratch; rows already committed stay as
    /// they were.
    pub fn resize(&mut self, width: usize, rows: usize) {
        let width = width.max(1);
        let rows = rows.max(1);
        if self.width == width && self.rows == rows {
            return;
        }
        self.width = width;
        self.rows = rows;
        let text = std::mem::take(&mut self.text);
        self.settled.clear();
        self.open.clear();
        self.column = 0;
        self.last_space = None;
        self.text.push_str(&text);
        for c in text.chars() {
            self.feed(c);
        }
        self.emitted = self.emitted.min(self.total());
    }

    pub fn push(&mut self, delta: &str) {
        self.text.push_str(delta);
        for c in delta.chars() {
            self.feed(c);
        }
    }

    /// One character through the wrap rules of [`wrap`].
    fn feed(&mut self, c: char) {
        #[cfg(test)]
        {
            self.wrapped += 1;
        }
        if c == '\n' {
            self.end_row();
            return;
        }
        let width = width_of(c);
        if self.column + width > self.width && !self.open.is_empty() && c != ' ' {
            match self.last_space.filter(|space| *space > 0) {
                Some(space) => {
                    // Break at the last space: the head is a finished row, the
                    // tail (after the space, and any spaces after it) carries
                    // over and this character joins it.
                    let head: String = self.open[..space].iter().collect();
                    self.settled.push(head.trim_end().to_string());
                    let mut tail: Vec<char> = self.open[space + 1..].to_vec();
                    while tail.first() == Some(&' ') {
                        tail.remove(0);
                    }
                    tail.push(c);
                    self.column = tail.iter().map(|c| width_of(*c)).sum();
                    self.last_space = tail.iter().rposition(|c| *c == ' ');
                    self.open = tail;
                    return;
                }
                None => {
                    // No space to break at: the row is full.
                    self.end_row();
                    self.open.push(c);
                    self.column = width;
                    self.last_space = None;
                    return;
                }
            }
        }
        self.open.push(c);
        self.column += width;
        if c == ' ' {
            self.last_space = Some(self.open.len() - 1);
        }
    }

    /// Finish the open line: it becomes a row even when it is empty (a
    /// newline, and the end of the message, both do that in [`wrap`]).
    fn end_row(&mut self) {
        let row: String = self.open.iter().collect();
        self.settled.push(row.trim_end().to_string());
        self.open.clear();
        self.column = 0;
        self.last_space = None;
    }

    /// The open line as it would be displayed (trailing spaces trimmed).
    fn open_row(&self) -> String {
        self.open.iter().collect::<String>().trim_end().to_string()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    pub fn total(&self) -> usize {
        if self.text.is_empty() {
            return 0;
        }
        self.settled.len() + 1
    }

    /// Rows that no longer fit in the live area, in order, each exactly once.
    pub fn due(&mut self) -> Vec<String> {
        let total = self.total();
        let keep = self.rows;
        if total <= keep {
            return Vec::new();
        }
        let up_to = total - keep;
        if up_to <= self.emitted {
            return Vec::new();
        }
        // `keep` is at least one row and the open line is exactly one row, so
        // `up_to` never reaches it: only settled rows are committed.
        let out = self.settled[self.emitted..up_to].to_vec();
        self.emitted = up_to;
        out
    }

    /// The rows still in the live area.
    pub fn view(&self) -> Vec<String> {
        if self.text.is_empty() {
            return Vec::new();
        }
        let mut rows = self.settled[self.emitted.min(self.settled.len())..].to_vec();
        rows.push(self.open_row());
        rows
    }

    /// The message is finished: everything left goes to the scrollback, and
    /// the buffer is ready for the next message.
    pub fn finish(&mut self) -> Vec<String> {
        if !self.text.is_empty() {
            self.end_row();
        }
        let out = self.settled[self.emitted.min(self.settled.len())..].to_vec();
        self.text.clear();
        self.settled.clear();
        self.open.clear();
        self.column = 0;
        self.last_space = None;
        self.emitted = 0;
        #[cfg(test)]
        {
            self.wrapped = 0;
        }
        out
    }

    /// Test hook: how many characters have been fed through the wrapper. It
    /// equals the length of the message when nothing was re-wrapped.
    #[cfg(test)]
    pub fn wrapped_chars(&self) -> usize {
        self.wrapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_lines_are_not_touched() {
        assert_eq!(wrap("hello", 80), vec!["hello"]);
        assert_eq!(wrap("", 80), vec![""]);
    }

    #[test]
    fn newlines_are_row_breaks() {
        assert_eq!(wrap("a\nb\n", 80), vec!["a", "b", ""]);
        assert_eq!(wrap("\n\n", 80), vec!["", "", ""]);
    }

    #[test]
    fn a_long_line_breaks_at_spaces() {
        assert_eq!(wrap("one two three", 8), vec!["one two", "three"]);
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(
            wrap("aaa bbb ccc ddd", 7),
            vec!["aaa bbb", "ccc ddd"],
            "the space at the break is dropped, not repeated"
        );
    }
    #[test]
    fn a_word_longer_than_the_row_is_broken() {
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
        assert_eq!(wrap("ab cdefg", 3), vec!["ab", "cde", "fg"]);
    }

    #[test]
    fn wide_characters_count_two_columns() {
        assert_eq!(display_width("日本"), 4);
        assert_eq!(display_width("aé"), 2);
        assert_eq!(wrap("日本語", 4), vec!["日本", "語"]);
        // A row of width 1 still gets the wide character.
        assert_eq!(wrap("日", 1), vec!["日"]);
    }

    #[test]
    fn trailing_spaces_are_dropped_and_leading_ones_kept() {
        assert_eq!(wrap("ab   ", 10), vec!["ab"]);
        assert_eq!(wrap("  ab", 10), vec!["  ab"]);
    }

    #[test]
    fn the_live_area_keeps_the_tail_and_commits_the_rest_once() {
        let mut live = Live::new(4, 2);
        let mut committed: Vec<String> = Vec::new();
        for delta in ["aa ", "bb ", "cc ", "dd ", "ee"] {
            live.push(delta);
            committed.extend(live.due());
        }
        assert_eq!(committed, vec!["aa", "bb", "cc"], "three rows scrolled off");
        assert_eq!(live.view(), vec!["dd", "ee"]);
        committed.extend(live.finish());
        assert_eq!(
            committed,
            vec!["aa", "bb", "cc", "dd", "ee"],
            "every row exactly once"
        );
        assert!(live.is_empty());
        assert!(live.view().is_empty());
    }

    #[test]
    fn a_message_shorter_than_the_live_area_is_committed_whole() {
        let mut live = Live::new(80, 4);
        live.push("done");
        assert!(live.due().is_empty());
        assert_eq!(live.view(), vec!["done"]);
        assert_eq!(live.finish(), vec!["done"]);
    }

    #[test]
    fn a_resize_does_not_repeat_committed_rows() {
        let mut live = Live::new(4, 1);
        live.push("aaaaaaaa");
        assert_eq!(live.due(), vec!["aaaa"], "the first row scrolls off");
        live.resize(2, 1);
        // 8 columns of `aaaa` at width 2 is 4 rows; one was already emitted.
        assert_eq!(live.due(), vec!["aa", "aa"]);
        assert_eq!(live.view(), vec!["aa"]);
        assert_eq!(live.finish(), vec!["aa"]);
    }

    #[test]
    fn streamed_text_wraps_exactly_like_wrap() {
        let text = "a bb ccc  dd\n\nee f\n\n  leading\ntrailing   \nwide 日本語 x\n\nlast";
        for width in [1, 2, 3, 5, 13, 40, 80] {
            let mut live = Live::new(width, 2);
            let mut rows = Vec::new();
            for c in text.chars() {
                live.push(&c.to_string());
                rows.extend(live.due());
            }
            rows.extend(live.finish());
            assert_eq!(rows, wrap(text, width), "width {width}");
        }
    }

    #[test]
    fn a_one_megabyte_message_is_wrapped_once() {
        // A single long answer in 1 KB deltas: the old `due()` re-wrapped the
        // whole message every frame (O(n²)); this proves every character goes
        // through the wrapper once.
        let mut text = String::with_capacity(1 << 20);
        let mut index = 0;
        while text.len() < (1 << 20) {
            text.push_str(&format!("word{index} "));
            index += 1;
        }
        let chars: Vec<char> = text.chars().collect();
        let expected = wrap(&text, 80);
        let mut live = Live::new(80, 4);
        let mut rows = Vec::new();
        for chunk in chars.chunks(1024) {
            let delta: String = chunk.iter().collect();
            live.push(&delta);
            rows.extend(live.due());
        }
        assert_eq!(
            live.wrapped_chars(),
            chars.len(),
            "characters went through the wrapper more than once"
        );
        rows.extend(live.finish());
        assert!(rows.len() > 1000, "{} rows", rows.len());
        for row in &rows {
            assert!(display_width(row) <= 80, "{row:?}");
        }
        assert_eq!(rows, expected);
    }
}
