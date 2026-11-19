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
    /// Insert a chip (a saved image) at the cursor, as one atomic element.
    Chip(Chip),
    /// Remove the chip whose label is at the cursor (or just before it), as one
    /// element: Backspace on a chip removes the whole thing, never one
    /// character of its label.
    RemoveChip,
    /// Drop every chip from the buffer (a submitted or cleared prompt): the
    /// chip decision belongs to the text, so this is what a caller after
    /// `Submitted` uses to forget them.
    DropChips,
    HistoryPrev,
    HistoryNext,
}

/// What a key did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The buffer changed (or the cursor moved).
    Edited,
    /// Enter on a non-empty buffer: the text, and the buffer is empty again.
    /// The caller reads [`Editor::take_chips`] for the images the text named.
    Submitted(String),
    /// The key means nothing here (or the buffer was empty on Enter).
    Ignored,
    /// A chip was removed: the caller deletes its file (nothing else referred to
    /// it yet).
    ChipRemoved(Chip),
}

/// A saved image in the input, as one atomic element.
///
/// The buffer holds the chip's *label* (`[image 1]`) as ordinary characters, so
/// every cursor rule stays simple; the label is deliberately unlikely to occur
/// in typed text. When the chip is on the row, the renderer paints the label
/// reversed; on submit the caller replaces it with the pointer text the model
/// sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    /// The per-prompt number: also the number in the label and the saved file.
    pub seq: u32,
    /// `attachments/1-clipboard.png`, relative to the session dir.
    pub rel: String,
    /// The pointer the prompt carries: `[image: attachments/1-…, 1280x720, …]`.
    pub pointer: String,
    /// The label in the buffer: `[image 1]`.
    pub label: String,
}

impl Chip {
    /// The chip for a saved attachment.
    pub fn new(seq: u32, rel: &str, pointer: &str) -> Self {
        Self {
            seq,
            rel: rel.to_string(),
            pointer: pointer.to_string(),
            label: crate::attachments::chip_label(seq),
        }
    }

    /// The label inside a line, if it is there.
    pub fn find(&self, line: &str) -> Option<usize> {
        line.find(&self.label)
    }
}

#[derive(Debug, Clone)]
pub struct Editor {
    lines: Vec<String>,
    /// The images this buffer names, in insertion order.
    chips: Vec<Chip>,
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
            chips: Vec::new(),
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

    /// The cursor as the renderer needs it: `(row, display column)`, the prompt
    /// included and every character measured by its terminal width (a chip's
    /// label is ordinary text, so it counts its own width).
    pub fn cursor_display(&self) -> (usize, usize) {
        use unicode_width::UnicodeWidthChar as _;
        let before: usize = self
            .lines
            .get(self.row)
            .map(|line| {
                line.chars()
                    .take(self.col)
                    .map(|c| c.width().unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0);
        (self.row, crate::tui::PROMPT_WIDTH + before)
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// The images the buffer names.
    pub fn chips(&self) -> &[Chip] {
        &self.chips
    }

    /// Take the chips (after a submit: the caller decides what happens to
    /// them).
    pub fn take_chips(&mut self) -> Vec<Chip> {
        std::mem::take(&mut self.chips)
    }

    /// Insert a chip at the cursor as one atomic element: a space before it when
    /// the cursor is mid-word, the label, and a space after it.
    pub fn insert_chip(&mut self, chip: Chip) {
        self.stop_browsing();
        let label = chip.label.clone();
        self.chips.push(chip);
        let row = self.row;
        let col = self.char_to_byte(row, self.col);
        let before_space = self.lines[row][..col]
            .chars()
            .last()
            .is_some_and(|c| c != ' ');
        let mut text = String::new();
        if before_space {
            text.push(' ');
        }
        text.push_str(&label);
        text.push(' ');
        let added = text.chars().count();
        self.lines[row].insert_str(col, &text);
        self.col += added;
    }

    /// Take the word that ends at the cursor (a keystroke burst's path, before
    /// it is typed), leaving the buffer and the cursor as they were minus that
    /// word. `None` when no word ends at the cursor.
    ///
    /// The word is the run of non-space characters ending at the cursor, with a
    /// closing quote and its matching opening quote taken along: the shapes a
    /// dropped path arrives in (`C:\x\a.png`, `"C:\x\a b.png"`,
    /// `& 'C:\x\a.png'`). A path with spaces is only a word when it is quoted,
    /// which is exactly what Explorer and the shell hand over.
    pub fn take_word_before_cursor(&mut self) -> Option<String> {
        let row = self.row;
        let col = self.char_to_byte(row, self.col);
        let line = self.lines[row].clone();
        let chars: Vec<char> = line[..col].chars().collect();
        let mut end = chars.len();
        while end > 0 && chars[end - 1].is_whitespace() {
            end -= 1;
        }
        if end == 0 {
            return None;
        }
        // A closing quote belongs to the word: find its opener.
        let closer = chars[end - 1];
        let (start, drop_to) = if closer == '"' || closer == '\'' {
            match chars[..end - 1].iter().rposition(|c| *c == closer) {
                Some(open) => (open, end),
                None => (end - 1, end),
            }
        } else {
            let open = chars[..end]
                .iter()
                .rposition(|c| c.is_whitespace())
                .map(|index| index + 1)
                .unwrap_or(0);
            (open, end)
        };
        let word: String = chars[start..drop_to].iter().collect();
        if word.trim_matches(['"', '\'', '&', ' ']).is_empty() {
            return None;
        }
        // Take the word and the single space before it: the chip inserts its own
        // spacing, and the burst's space is part of what the path brought.
        let from = if start > 0 && chars[start - 1] == ' ' {
            start - 1
        } else {
            start
        };
        let to_byte = |index: usize| -> usize {
            line[..col]
                .char_indices()
                .nth(index)
                .map(|(offset, _)| offset)
                .unwrap_or(col)
        };
        self.lines[row].replace_range(to_byte(from)..to_byte(drop_to), "");
        self.col = chars[..from].len();
        Some(word)
    }

    /// Remove the chip the cursor belongs to (just before it, or starting at
    /// it). Returns it so the caller can delete the file.
    pub fn remove_chip_at_cursor(&mut self) -> Option<Chip> {
        let row = self.row;
        let col = self.char_to_byte(row, self.col);
        let line = self.lines[row].clone();
        // The chip the cursor belongs to: the label that starts at the cursor,
        // or the one the cursor is inside or immediately after (a chip is one
        // element, so anywhere on its label means the whole chip).
        let mut hit: Option<(usize, usize, usize)> = None;
        for (index, chip) in self.chips.iter().enumerate() {
            let mut from = 0usize;
            while let Some(found) = line[from..].find(&chip.label) {
                let start = from + found;
                let end = start + chip.label.len();
                if start <= col && col <= end + 1 {
                    hit = Some((index, start, end));
                    break;
                }
                from = end;
            }
            if hit.is_some() {
                break;
            }
        }
        let (index, start, end) = hit?;
        let chip = self.chips.remove(index);
        // Take one trailing space with the label, so the sentence stays even.
        let end = if line[end..].starts_with(' ') {
            end + 1
        } else {
            end
        };
        self.lines[row].replace_range(start..end, "");
        self.col = line[..start].chars().count();
        Some(chip)
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
                // The chips stay for the caller: they are what the submitted
                // text now points at.
                Outcome::Submitted(text)
            }
            Key::Chip(chip) => {
                self.insert_chip(chip);
                Outcome::Edited
            }
            Key::RemoveChip => match self.remove_chip_at_cursor() {
                Some(chip) => Outcome::ChipRemoved(chip),
                None => Outcome::Ignored,
            },
            Key::DropChips => {
                self.chips.clear();
                Outcome::Edited
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

    #[test]
    fn a_chip_is_one_atomic_element_in_the_buffer() {
        let mut editor = Editor::default();
        for key in [
            Key::Char('l'),
            Key::Char('o'),
            Key::Char('o'),
            Key::Char('k'),
        ] {
            editor.handle(key);
        }
        editor.insert_chip(Chip::new(1, "attachments/1-a.png", "[image: x]"));
        assert_eq!(editor.text(), "look [image 1] ");
        assert_eq!(editor.chips().len(), 1);
        assert_eq!(
            editor.cursor(),
            (0, 15),
            "the cursor sits after the chip and its space"
        );
        // Typing continues after it.
        editor.handle(Key::Char('!'));
        assert_eq!(editor.text(), "look [image 1] !");
        // Past the '!' there is no chip to take; back over it and one
        // RemoveChip takes the whole label, not one character.
        assert_eq!(editor.handle(Key::RemoveChip), Outcome::Ignored);
        editor.handle(Key::Left);
        let outcome = editor.handle(Key::RemoveChip);
        match outcome {
            Outcome::ChipRemoved(chip) => assert_eq!(chip.seq, 1),
            other => panic!("{other:?} at {:?} in {:?}", editor.cursor(), editor.text()),
        }
        assert_eq!(editor.text(), "look !");
        assert!(editor.chips().is_empty());
        // A second RemoveChip finds nothing.
        assert_eq!(editor.handle(Key::RemoveChip), Outcome::Ignored);
    }

    #[test]
    fn a_chip_at_the_start_gets_no_leading_space_and_two_chips_are_numbered() {
        let mut editor = Editor::default();
        editor.insert_chip(Chip::new(1, "attachments/1-a.png", "[image: x]"));
        editor.handle(Key::Char(' '));
        editor.insert_chip(Chip::new(2, "attachments/2-b.png", "[image: y]"));
        assert_eq!(editor.text(), "[image 1]  [image 2] ");
        assert_eq!(editor.chips().len(), 2);
        // The second chip is the one the cursor is in: remove it by label.
        let chip = editor.remove_chip_at_cursor().expect("the cursor's chip");
        assert_eq!(chip.seq, 2);
        assert_eq!(editor.text(), "[image 1]  ");
    }

    #[test]
    fn a_submit_keeps_the_chip_list_for_the_caller_and_clears_the_buffer() {
        let mut editor = Editor::default();
        editor.insert_chip(Chip::new(1, "attachments/1-a.png", "[image: x]"));
        let outcome = editor.handle(Key::Enter);
        assert!(matches!(outcome, Outcome::Submitted(_)));
        assert!(editor.is_empty());
        assert_eq!(
            editor.chips().len(),
            1,
            "the caller needs the chip it just submitted"
        );
        assert_eq!(editor.take_chips().len(), 1);
        assert!(editor.chips().is_empty());
    }

    #[test]
    fn a_chip_cursor_math_counts_the_label_its_whole_width() {
        let mut editor = Editor::default();
        editor.insert_chip(Chip::new(3, "attachments/3-c.png", "[image: z]"));
        // Home, then Right five times, lands inside the label; RemoveChip from
        // there still takes the whole chip (it is one element).
        editor.handle(Key::Home);
        for _ in 0..5 {
            editor.handle(Key::Right);
        }
        let chip = editor
            .remove_chip_at_cursor()
            .expect("a chip at the cursor");
        assert_eq!(chip.seq, 3);
        assert_eq!(editor.text().trim(), "");
    }
}
