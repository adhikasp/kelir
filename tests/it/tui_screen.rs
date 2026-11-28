//! A screen, not a byte stream: replay the terminal escapes a TUI writes and
//! read back what a person would see. `plain()` (in `tui_output`) throws the
//! cursor movements away, which cannot tell a repainted row from a stale one —
//! and a full-screen TUI repaints by moving the cursor and overwriting.
//!
//! Deliberately small: absolute cursor addressing (`CUP`/`CHA`/`VPA`), erase in
//! line/screen, the SGR sequences a style change emits (dropped), and printable
//! text put at the cursor. No scrolling regions, no alternate screen, no
//! reflow — a 40x100 pane repainted in place needs none of it.
#![allow(clippy::pedantic, clippy::nursery)]

/// A terminal screen built from a byte stream.
pub(super) struct Screen {
    width: usize,
    height: usize,
    cells: Vec<Vec<char>>,
    row: usize,
    col: usize,
}

impl Screen {
    pub(super) fn replay(bytes: &[u8]) -> Self {
        let mut screen = Self::new(100, 40);
        screen.feed(&String::from_utf8_lossy(bytes));
        screen
    }

    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            cells: vec![vec![' '; width]; height],
            row: 0,
            col: 0,
        }
    }

    pub(super) fn feed(&mut self, text: &str) {
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\u{1b}' => match chars.peek() {
                    Some('[') => {
                        chars.next();
                        let mut params = String::new();
                        let mut final_byte = '\0';
                        for c in chars.by_ref() {
                            if ('@'..='~').contains(&c) {
                                final_byte = c;
                                break;
                            }
                            params.push(c);
                        }
                        self.csi(&params, final_byte);
                    }
                    Some(']') => {
                        // OSC: skip to BEL or ST.
                        let mut last = '\0';
                        for c in chars.by_ref() {
                            if c == '\u{7}' || (last == '\u{1b}' && c == '\\') {
                                break;
                            }
                            last = c;
                        }
                    }
                    _ => {
                        chars.next();
                    }
                },
                '\r' => self.col = 0,
                '\n' => self.down(1),
                c if (c as u32) < 0x20 => {}
                c => self.put(c),
            }
        }
    }

    /// The one `CSI` sequence a TUI needs: position and erase. A parameter
    /// list is 1-based on the wire and may be empty (`ESC[2J`).
    fn csi(&mut self, params: &str, final_byte: char) {
        let numbers: Vec<usize> = params
            .split(';')
            .map(|part| {
                // Private-mode introducers (`?25h`) are not numbers.
                part.trim_start_matches(['?', '<', '>', '='])
                    .parse()
                    .unwrap_or(0)
            })
            .collect();
        let first = numbers.first().copied().unwrap_or(0);
        let second = numbers.get(1).copied().unwrap_or(0);
        // A zero is "unset" in every sequence here, which means the default.
        let row = |value: usize| value.saturating_sub(1);
        match final_byte {
            'H' | 'f' => {
                self.row = row(first).min(self.height - 1);
                self.col = row(second).min(self.width - 1);
            }
            'A' => self.row = self.row.saturating_sub(first.max(1)),
            'B' => self.down(first.max(1)),
            'C' => self.col = (self.col + first.max(1)).min(self.width - 1),
            'D' => self.col = self.col.saturating_sub(first.max(1)),
            'G' => self.col = row(first).min(self.width - 1),
            'd' => self.row = row(first).min(self.height - 1),
            'J' => {
                let mode = first;
                if mode == 2 || mode == 3 {
                    self.cells = vec![vec![' '; self.width]; self.height];
                } else {
                    for col in self.col..self.width {
                        self.cells[self.row][col] = ' ';
                    }
                    for r in self.row + 1..self.height {
                        self.cells[r] = vec![' '; self.width];
                    }
                }
            }
            'K' => {
                let (from, to) = match first {
                    1 => (0, self.col.min(self.width - 1) + 1),
                    2 => (0, self.width),
                    _ => (self.col.min(self.width), self.width),
                };
                let row = &mut self.cells[self.row];
                for cell in &mut row[from..to] {
                    *cell = ' ';
                }
            }
            _ => {}
        }
    }

    fn down(&mut self, rows: usize) {
        self.row = (self.row + rows).min(self.height - 1);
    }

    fn put(&mut self, c: char) {
        if self.col >= self.width {
            return;
        }
        self.cells[self.row][self.col] = c;
        self.col += 1;
    }

    /// The screen, one trimmed line per row, trailing blank rows dropped.
    pub(super) fn text(&self) -> String {
        let mut lines: Vec<String> = self
            .cells
            .iter()
            .map(|row| row.iter().collect::<String>().trim_end().to_string())
            .collect();
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }
}
