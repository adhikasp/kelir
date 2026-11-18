//! The input editor: a small multi-line buffer with a cursor and history.
//!
//! The state machine takes [`Key`] values, not terminal events, so every rule
//! here is unit-tested without a terminal. The TUI translates
//! `crossterm` events into [`Key`] and renders the buffer.

/// One editor input. Everything the editor reacts to lives here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// A newline inside the message (Alt-Enter, Shift-Enter, Ctrl-J).
    Newline,
    /// Enter: submit the buffer.
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    /// Ctrl-K: cut from the cursor to the end of the line.
    KillToEnd,
    /// Ctrl-U: clear the whole buffer.
    Clear,
    /// A bracketed paste, newlines and all.
    Paste(String),
    HistoryPrev,
    HistoryNext,
}

/// What a key did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The buffer changed (or the cursor moved).
    Edited,
    /// Enter on a non-empty buffer: the text, and the buffer is empty again.
    Submitted(String),
    /// The key means nothing here (or the buffer was empty on Enter).
    Ignored,
}

#[derive(Debug, Clone)]
pub struct Editor {
    lines: Vec<String>,
    /// Cursor line, always a valid index.
    row: usize,
    /// Cursor column, in `char`s, never past the end of the line.
    col: usize,
    history: Vec<String>,
    /// Where in `history` the buffer is, while browsing.
    browsing: Option<usize>,
    /// The buffer as it was before browsing started.
    draft: Option<Vec<String>>,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            history: Vec::new(),
            browsing: None,
            draft: None,
        }
    }
}

impl Editor {
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// The window of buffer rows to draw when only `rows` fit: the buffer is
    /// scrolled just enough to keep the cursor visible.
    pub fn window(&self, rows: usize) -> (usize, usize) {
        let rows = rows.max(1);
        if self.lines.len() <= rows {
            return (0, self.lines.len().max(1));
        }
        let first = if self.row < rows {
            0
        } else {
            (self.row + 1).saturating_sub(rows)
        };
        (first, rows)
    }

    /// Apply one key.
    pub fn handle(&mut self, key: Key) -> Outcome {
        // A line feed that arrives as a character (a paste, an unusual
        // terminal) is a newline, never a literal 0x0A in the buffer.
        if key == Key::Char('\n') {
            return self.handle(Key::Newline);
        }
        match key {
            Key::Char(c) => {
                self.stop_browsing();
                let row = self.row;
                let col = self.char_to_byte(row, self.col);
                self.lines[row].insert(col, c);
                self.col += 1;
                Outcome::Edited
            }
            Key::Newline => {
                self.stop_browsing();
                let row = self.row;
                let col = self.char_to_byte(row, self.col);
                let rest = self.lines[row].split_off(col);
                self.lines.insert(row + 1, rest);
                self.row += 1;
                self.col = 0;
                Outcome::Edited
            }
            Key::Paste(text) => {
                self.stop_browsing();
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                for (index, segment) in text.split('\n').enumerate() {
                    if index > 0 {
                        self.split_line();
                    }
                    for c in segment.chars() {
                        let row = self.row;
                        let col = self.char_to_byte(row, self.col);
                        self.lines[row].insert(col, c);
                        self.col += 1;
                    }
                }
                Outcome::Edited
            }
            Key::Enter => {
                let text = self.text();
                if text.trim().is_empty() {
                    return Outcome::Ignored;
                }
                if self.history.last() != Some(&text) {
                    self.history.push(text.clone());
                }
                self.reset();
                Outcome::Submitted(text)
            }
            Key::Backspace => {
                self.stop_browsing();
                if self.col > 0 {
                    let row = self.row;
                    let end = self.char_to_byte(row, self.col);
                    let start = self.char_to_byte(row, self.col - 1);
                    self.lines[row].replace_range(start..end, "");
                    self.col -= 1;
                } else if self.row > 0 {
                    let line = self.lines.remove(self.row);
                    self.row -= 1;
                    self.col = self.lines[self.row].chars().count();
                    self.lines[self.row].push_str(&line);
                } else {
                    return Outcome::Ignored;
                }
                Outcome::Edited
            }
            Key::Delete => {
                self.stop_browsing();
                let row = self.row;
                if self.col < self.lines[row].chars().count() {
                    let start = self.char_to_byte(row, self.col);
                    let end = self.char_to_byte(row, self.col + 1);
                    self.lines[row].replace_range(start..end, "");
                } else if self.row + 1 < self.lines.len() {
                    let next = self.lines.remove(self.row + 1);
                    self.lines[row].push_str(&next);
                } else {
                    return Outcome::Ignored;
                }
                Outcome::Edited
            }
            Key::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.lines[self.row].chars().count();
                } else {
                    return Outcome::Ignored;
                }
                Outcome::Edited
            }
            Key::Right => {
                if self.col < self.lines[self.row].chars().count() {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                } else {
                    return Outcome::Ignored;
                }
                Outcome::Edited
            }
            Key::Up => {
                if self.row > 0 {
                    self.row -= 1;
                    self.col = self.col.min(self.lines[self.row].chars().count());
                    Outcome::Edited
                } else {
                    self.history_prev()
                }
            }
            Key::Down => {
                if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = self.col.min(self.lines[self.row].chars().count());
                    Outcome::Edited
                } else {
                    self.history_next()
                }
            }
            Key::Home => {
                self.col = 0;
                Outcome::Edited
            }
            Key::End => {
                self.col = self.lines[self.row].chars().count();
                Outcome::Edited
            }
            Key::KillToEnd => {
                self.stop_browsing();
                let row = self.row;
                let at = self.char_to_byte(row, self.col);
                self.lines[row].truncate(at);
                Outcome::Edited
            }
            Key::Clear => {
                self.reset();
                Outcome::Edited
            }
            Key::HistoryPrev => self.history_prev(),
            Key::HistoryNext => self.history_next(),
        }
    }

    // ----------------------------------------------------------- internals

    fn split_line(&mut self) {
        let row = self.row;
        let col = self.char_to_byte(row, self.col);
        let rest = self.lines[row].split_off(col);
        self.lines.insert(row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    fn reset(&mut self) {
        self.lines = vec![String::new()];
        self.row = 0;
        self.col = 0;
        self.browsing = None;
        self.draft = None;
    }

    fn char_to_byte(&self, row: usize, col: usize) -> usize {
        let line = &self.lines[row];
        line.char_indices()
            .nth(col)
            .map(|(index, _)| index)
            .unwrap_or(line.len())
    }

    fn stop_browsing(&mut self) {
        if self.browsing.take().is_some() {
            self.draft = None;
        }
    }

    fn history_prev(&mut self) -> Outcome {
        if self.history.is_empty() {
            return Outcome::Ignored;
        }
        let next = match self.browsing {
            None => {
                self.draft = Some(self.lines.clone());
                self.history.len() - 1
            }
            Some(0) => return Outcome::Ignored,
            Some(index) => index - 1,
        };
        self.browsing = Some(next);
        self.load(&self.history[next].clone());
        Outcome::Edited
    }

    fn history_next(&mut self) -> Outcome {
        let Some(index) = self.browsing else {
            return Outcome::Ignored;
        };
        if index + 1 < self.history.len() {
            self.browsing = Some(index + 1);
            let text = self.history[index + 1].clone();
            self.load(&text);
        } else {
            self.browsing = None;
            let draft = self.draft.take().unwrap_or_else(|| vec![String::new()]);
            self.lines = draft;
            self.row = self.lines.len() - 1;
            self.col = self.lines[self.row].chars().count();
        }
        Outcome::Edited
    }

    fn load(&mut self, text: &str) {
        self.lines = text.split('\n').map(str::to_string).collect();
        self.row = self.lines.len() - 1;
        self.col = self.lines[self.row].chars().count();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_text(editor: &mut Editor, text: &str) {
        for c in text.chars() {
            editor.handle(Key::Char(c));
        }
    }

    #[test]
    fn typing_appends_at_the_cursor() {
        let mut editor = Editor::default();
        type_text(&mut editor, "hello");
        assert_eq!(editor.text(), "hello");
        assert_eq!(editor.cursor(), (0, 5));
        editor.handle(Key::Left);
        editor.handle(Key::Left);
        type_text(&mut editor, "X");
        assert_eq!(editor.text(), "helXlo");
        assert_eq!(editor.cursor(), (0, 4));
    }

    #[test]
    fn enter_submits_and_empties_the_buffer() {
        let mut editor = Editor::default();
        type_text(&mut editor, "do it");
        assert_eq!(
            editor.handle(Key::Enter),
            Outcome::Submitted("do it".into())
        );
        assert!(editor.is_empty());
        assert_eq!(editor.handle(Key::Enter), Outcome::Ignored);
        assert_eq!(editor.history(), &["do it".to_string()]);
    }

    #[test]
    fn alt_enter_adds_a_line_instead_of_submitting() {
        let mut editor = Editor::default();
        type_text(&mut editor, "one");
        assert_eq!(editor.handle(Key::Newline), Outcome::Edited);
        type_text(&mut editor, "two");
        assert_eq!(editor.text(), "one\ntwo");
        assert_eq!(editor.cursor(), (1, 3));
        assert_eq!(
            editor.handle(Key::Enter),
            Outcome::Submitted("one\ntwo".into())
        );
    }

    #[test]
    fn an_empty_or_blank_buffer_is_not_sent() {
        let mut editor = Editor::default();
        assert_eq!(editor.handle(Key::Enter), Outcome::Ignored);
        type_text(&mut editor, "   ");
        assert_eq!(editor.handle(Key::Enter), Outcome::Ignored);
        assert_eq!(editor.text(), "   ");
    }

    #[test]
    fn newline_splits_and_backspace_joins() {
        let mut editor = Editor::default();
        type_text(&mut editor, "ab");
        editor.handle(Key::Left);
        editor.handle(Key::Newline);
        assert_eq!(editor.text(), "a\nb");
        editor.handle(Key::Backspace);
        assert_eq!(editor.text(), "ab", "backspace at the start joins the line");
        assert_eq!(editor.cursor(), (0, 1));
    }

    #[test]
    fn delete_forward_and_backspace_never_panic_at_the_edges() {
        let mut editor = Editor::default();
        assert_eq!(editor.handle(Key::Backspace), Outcome::Ignored);
        assert_eq!(editor.handle(Key::Delete), Outcome::Ignored);
        type_text(&mut editor, "ab\ncd");
        editor.handle(Key::Clear);
        type_text(&mut editor, "ab");
        editor.handle(Key::Home);
        editor.handle(Key::Delete);
        assert_eq!(editor.text(), "b");
        editor.handle(Key::End);
        editor.handle(Key::Delete);
        assert_eq!(editor.text(), "b");
    }

    #[test]
    fn up_and_down_stay_inside_the_buffer_then_walk_history() {
        let mut editor = Editor::default();
        type_text(&mut editor, "first");
        editor.handle(Key::Enter);
        type_text(&mut editor, "second");
        editor.handle(Key::Enter);
        assert!(editor.is_empty());

        assert_eq!(editor.handle(Key::HistoryPrev), Outcome::Edited);
        assert_eq!(editor.text(), "second");
        editor.handle(Key::HistoryPrev);
        assert_eq!(editor.text(), "first");
        assert_eq!(editor.handle(Key::HistoryPrev), Outcome::Ignored);
        editor.handle(Key::HistoryNext);
        assert_eq!(editor.text(), "second");
        editor.handle(Key::HistoryNext);
        assert_eq!(editor.text(), "", "back to the draft");

        // A multi-line draft moves the cursor first.
        type_text(&mut editor, "a\nb");
        assert_eq!(editor.cursor(), (1, 1));
        editor.handle(Key::Up);
        assert_eq!(editor.cursor(), (0, 1));
        assert_eq!(editor.text(), "a\nb");
    }

    #[test]
    fn typing_while_browsing_starts_a_new_draft() {
        let mut editor = Editor::default();
        type_text(&mut editor, "old");
        editor.handle(Key::Enter);
        editor.handle(Key::HistoryPrev);
        type_text(&mut editor, "!");
        assert_eq!(editor.text(), "old!");
        editor.handle(Key::HistoryNext);
        assert_eq!(editor.text(), "old!", "editing stopped the browsing");
    }

    #[test]
    fn home_end_and_kill_to_end_work_on_the_cursor_line() {
        let mut editor = Editor::default();
        type_text(&mut editor, "one\ntwo");
        editor.handle(Key::Home);
        editor.handle(Key::KillToEnd);
        assert_eq!(editor.text(), "one\n");
        editor.handle(Key::Home);
        type_text(&mut editor, "T");
        editor.handle(Key::End);
        type_text(&mut editor, "!");
        assert_eq!(editor.text(), "one\nT!");
    }

    #[test]
    fn paste_keeps_its_newlines_and_normalises_carriage_returns() {
        let mut editor = Editor::default();
        editor.handle(Key::Paste("a\r\nb\rc".into()));
        assert_eq!(editor.text(), "a\nb\nc");
        assert_eq!(editor.cursor(), (2, 1));
    }

    #[test]
    fn the_cursor_is_counted_in_chars_not_bytes() {
        let mut editor = Editor::default();
        type_text(&mut editor, "héllo");
        assert_eq!(editor.cursor(), (0, 5));
        editor.handle(Key::Home);
        editor.handle(Key::Right);
        editor.handle(Key::Backspace);
        assert_eq!(editor.text(), "éllo");
    }

    #[test]
    fn the_window_scrolls_to_keep_the_cursor_visible() {
        let mut editor = Editor::default();
        for index in 0..10 {
            if index > 0 {
                editor.handle(Key::Newline);
            }
            type_text(&mut editor, &index.to_string());
        }
        assert_eq!(editor.lines().len(), 10);
        assert_eq!(editor.window(3), (7, 3));
        editor.handle(Key::HistoryPrev);
        assert_eq!(editor.window(3), (7, 3));
        let mut short = Editor::default();
        type_text(&mut short, "x");
        assert_eq!(short.window(3), (0, 1));
    }
}
