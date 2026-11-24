//! App methods: layout.
use crate::tui::app::*;

impl App {
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
    pub(super) fn append(&mut self, entry: Entry) {
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

    pub(super) fn scroll_by(&mut self, delta: isize, viewport: usize) {
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

    pub(super) fn scroll_to(&mut self, top: usize, viewport: usize) {
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
    pub fn live_view(&self) -> Vec<crate::tui::markdown::Row> {
        if self.draft.trim().is_empty() {
            return Vec::new();
        }
        let layout = crate::tui::markdown::Layout::with_glyphs(self.width, self.theme.glyphs);
        let mut rows = crate::tui::markdown::open_rows(&self.draft, None, self.draft.len(), layout);
        if rows.is_empty() {
            rows = crate::tui::markdown::render(&self.draft, layout);
        }
        rows
    }

    pub fn live_lines(&self) -> Vec<Line<'static>> {
        self.live_view()
            .iter()
            .map(|row| row.line(self.theme, &crate::tui::markdown::NoHighlight))
            .collect()
    }

    pub fn wanted_live_rows(&self) -> u16 {
        self.live_view().len().min(3) as u16
    }
}
