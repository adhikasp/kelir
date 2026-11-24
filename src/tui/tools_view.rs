//! How tool calls appear: one dim line per call, plus the append-only expansion for `/expand`.
//! This is the design note's central claim (TUI-v2 §4): **prose is the only default-weight text in
//! the transcript**.
//! A call is two columns of indent, a dim line, and a right-aligned dim verdict; a call that
//! failed keeps its line but goes red and gains one dim tail line, so the failure is visible

use std::time::Duration;

use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::tui::theme::Theme;

/// The maximum length of a tool call's human-readable `description`.
/// A label is a glance, never a second copy of the command.
pub const DESCRIPTION_MAX: usize = 120;

/// The `description` every native tool call supplies, cleaned for one-line display: folded whitespace,
/// capped at [`DESCRIPTION_MAX`] characters.
pub fn description(input: &Value) -> Option<String> {
    let raw = input.get("description")?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    // Model-supplied labels must never put control bytes into a terminal or a Herdr OSC record.
    let chars: Vec<char> = raw.chars().collect();
    let mut clean = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Drop OSC introducers (]8;;, ]7501;, ...) too: without the leading
        // ESC they are still text a viewer could mistake for a control record.
        if c == ']' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && j < chars.len() && chars[j] == ';' {
                i = j + 1;
                continue;
            }
        }
        clean.push(if c.is_control() { ' ' } else { c });
        i += 1;
    }
    // The cap is in characters, not bytes: a 60-column pane cares about what
    // the human sees, and a non-ASCII label is not shorter for being wide.
    let one_line: String = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= DESCRIPTION_MAX {
        return Some(one_line);
    }
    let cut: String = one_line.chars().take(DESCRIPTION_MAX - 1).collect();
    Some(format!("{cut}…"))
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallLine {
    /// Session-wide ordinal: what `/expand n` names.
    pub ordinal: u64,
    pub name: String,
    /// The call's one-line identity: its `description` when it gave one,
    /// otherwise `$ cargo test` / `edit src/lib.rs`.
    pub head: String,
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub bytes: u64,
    pub lines: u64,
    pub spilled: bool,
    pub out_path: String,
    pub tail: String,
    /// How long the call took, when the caller measured it.
    pub elapsed: Option<Duration>,
}

impl CallLine {
    /// The turn (or this call) was stopped while it ran. Not a failure.
    pub fn cancelled(&self) -> bool {
        self.outcome == "stopped"
    }

    /// A failure is an error outcome or a non-zero exit; a cancelled call is
    /// not a failure (the human asked for it).
    pub fn failed(&self) -> bool {
        !self.cancelled() && (self.outcome != "ok" || self.exit_code.is_some_and(|code| code != 0))
    }

    /// The right-hand dim verdict: `29 B`, `✗ exit 101`.
    pub fn verdict(&self, theme: Theme) -> String {
        let mut parts = Vec::new();
        if self.failed() {
            if let Some(code) = self.exit_code {
                parts.push(format!("{} exit {code}", theme.glyphs.error()));
            } else {
                parts.push(format!("{} {}", theme.glyphs.error(), self.outcome));
            }
        } else if self.cancelled() {
            parts.push(format!("{} cancelled", theme.glyphs.stopped()));
        }
        if self.bytes > 0 {
            parts.push(bytes_label(self.bytes));
        }
        parts.join(" · ")
    }

    /// The call's own line, `width` columns wide with the verdict right-aligned.
    /// The verdict survives: the head is clipped first.
    pub fn line(&self, theme: Theme, width: usize) -> Line<'static> {
        let style = if self.failed() {
            theme.error()
        } else {
            theme.dim()
        };
        let verdict = self.verdict(theme);
        let verdict_width = crate::tui::scrollback::display_width(&verdict);
        let head = format!("  {}", self.head);
        let room = width.saturating_sub(verdict_width + 1);
        let head = crate::tui::fit(&head, room.max(1));
        let pad = width
            .saturating_sub(crate::tui::scrollback::display_width(&head) + verdict_width)
            .max(1);
        let mut spans = vec![Span::styled(head, style)];
        if width > verdict_width + 4 {
            spans.push(Span::styled(" ".repeat(pad), theme.dim()));
            spans.push(Span::styled(
                verdict,
                if self.failed() {
                    theme.error()
                } else {
                    theme.dim()
                },
            ));
        }
        Line::from(spans)
    }

    /// The dim tail line under a failed call: the last thing it printed.
    pub fn tail_line(&self, theme: Theme, width: usize) -> Option<Line<'static>> {
        let tail = self.tail.trim();
        if tail.is_empty() {
            return None;
        }
        let text = crate::tui::fit(&format!("    {tail}"), width.max(1));
        Some(Line::from(Span::styled(text, theme.dim())))
    }

    /// The expansion block: dim, indented four, capped with a count of what was
    /// left out.
    pub fn expanded(
        &self,
        theme: Theme,
        width: usize,
        text: &str,
        cap: usize,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(Line::from(Span::styled(
            format!(
                "  #{} {} ({})",
                self.ordinal,
                self.head,
                self.verdict(theme)
            ),
            theme.dim(),
        )));
        let wrapped = crate::tui::scrollback::wrap(text, width.saturating_sub(4).max(1));
        let total = wrapped.len();
        for row in wrapped.into_iter().take(cap) {
            lines.push(Line::from(Span::styled(format!("    {row}"), theme.dim())));
        }
        if total > cap {
            lines.push(Line::from(Span::styled(
                format!(
                    "    {} more rows (/expand shows {} at a time)",
                    total - cap,
                    cap
                ),
                theme.dim(),
            )));
        }
        lines
    }
}

/// `29 B`, `4.1 KB`: output size as the transcript shows it.
pub fn bytes_label(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// How much of a call's output `/expand` appends.
pub const EXPAND_ROWS: usize = 40;

/// The rows for a set of settled calls: one dim line each, in order, with a
/// failure's tail under it. No folding — every call keeps its own line.
pub fn rows(theme: Theme, width: usize, calls: &[CallLine]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for call in calls {
        lines.push(call.line(theme, width));
        if call.failed()
            && let Some(tail) = call.tail_line(theme, width)
        {
            lines.push(tail);
        }
    }
    lines
}
