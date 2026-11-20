//! How tool calls appear: one dim line per call, runs of three or more folded
//! into one checked line, and the append-only expansion for `/expand`.
//!
//! This is the design note's central claim (TUI-v2 §4): **prose is the only
//! default-weight text in the transcript**. A call is two columns of indent, a
//! dim line, and a right-aligned dim verdict; a call that failed keeps its line
//! but goes red and gains one dim tail line, so the failure is visible without
//! unfolding anything.
//!
//! Scrollback is append-only (`insert_before` cannot rewrite a committed row),
//! so there are two tiers, and no third:
//!
//! 1. While a call runs it lives in the live area (`⠹ $ cargo test · 12s`, from
//!    [`crate::activity`]).
//! 2. When it settles it joins the open [`Group`]. The group closes when prose
//!    starts, a user message arrives or the turn ends; then it commits: one
//!    line per call for one or two calls, `✓ 3 calls` for three or more, plus
//!    every failed call as its own line. The prose between runs is what stays
//!    at full weight.
//!
//! `/expand [n]` appends a call's output tail to the scrollback; it cannot
//! unfold the line that is already committed, and the help text says so.

use std::time::Duration;

use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// One settled call, as the transcript shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct CallLine {
    /// Session-wide ordinal: what `/expand n` names.
    pub ordinal: u64,
    pub name: String,
    /// `$ cargo test` / `edit src/lib.rs`.
    pub head: String,
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub bytes: u64,
    pub lines: u64,
    pub spilled: bool,
    pub out_path: String,
    /// The last non-empty output line, for a failed call's tail row.
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

    /// The right-hand dim verdict: `ok · 29 B`, `✗ exit 101`.
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
        } else {
            parts.push("ok".to_string());
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
        let verdict_width = crate::scrollback::display_width(&verdict);
        let head = format!("  {}", self.head);
        let room = width.saturating_sub(verdict_width + 1);
        let head = crate::tui::fit(&head, room.max(1));
        let pad = width
            .saturating_sub(crate::scrollback::display_width(&head) + verdict_width)
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
        let wrapped = crate::scrollback::wrap(text, width.saturating_sub(4).max(1));
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

/// Whether settled calls print on their own line or print their output too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Policy {
    /// One dim line per call, groups folded (the default).
    #[default]
    Collapsed,
    /// Every call's output block is committed as well.
    Full,
}

impl Policy {
    pub fn word(self) -> &'static str {
        match self {
            Policy::Collapsed => "collapsed",
            Policy::Full => "full",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Policy::Collapsed => Policy::Full,
            Policy::Full => Policy::Collapsed,
        }
    }
}

/// The open group of settled calls.
#[derive(Debug, Default)]
pub struct Group {
    calls: Vec<CallLine>,
}

impl Group {
    pub fn push(&mut self, call: CallLine) {
        self.calls.push(call);
    }

    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    pub fn len(&self) -> usize {
        self.calls.len()
    }

    /// The calls, in settlement order.
    pub fn calls(&self) -> &[CallLine] {
        &self.calls
    }

    /// The group as the live area shows it while it is still open: the last few
    /// call lines, folded the same way they will be committed.
    pub fn preview(&self, theme: Theme, width: usize, rows: usize) -> Vec<Line<'static>> {
        let lines = group_rows(theme, width, &self.calls);
        let start = lines.len().saturating_sub(rows);
        lines[start..].to_vec()
    }

    /// Close the group: the rows to commit, in order. One or two calls keep one
    /// line each; three or more collapse into one checked line (`✓ 3 calls`),
    /// with every failure still getting its own line and tail.
    pub fn close(&mut self, theme: Theme, width: usize) -> Vec<Line<'static>> {
        let calls = std::mem::take(&mut self.calls);
        group_rows(theme, width, &calls)
    }
}

fn group_rows(theme: Theme, width: usize, calls: &[CallLine]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    // Three or more calls between two pieces of prose collapse into one line;
    // the prose around them is what the reader is meant to see.
    let fold = calls.len() >= 3;
    let clean = calls.iter().filter(|call| !call.failed()).count();
    if fold && clean > 0 {
        let noun = if clean == 1 { "call" } else { "calls" };
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", theme.glyphs.done()), theme.ok()),
            Span::styled(format!("{clean} {noun}"), theme.dim()),
        ]));
    }
    for call in calls {
        // A failure stays visible even inside a fold: the harness must not hide
        // the one line that says something went wrong.
        if fold && !call.failed() {
            continue;
        }
        lines.push(call.line(theme, width));
        if call.failed()
            && let Some(tail) = call.tail_line(theme, width)
        {
            lines.push(tail);
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(head: &str, name: &str) -> CallLine {
        CallLine {
            ordinal: 1,
            name: name.to_string(),
            head: head.to_string(),
            outcome: "ok".into(),
            exit_code: Some(0),
            bytes: 29,
            lines: 1,
            spilled: false,
            out_path: String::new(),
            tail: String::new(),
            elapsed: Some(Duration::from_millis(200)),
        }
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn one_call_is_one_dim_line_with_a_right_aligned_verdict() {
        let theme = Theme::plain();
        let line = call("$ git --version", "bash").line(theme, 60);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with("  $ git --version"), "{text:?}");
        assert!(text.ends_with("ok · 29 B"), "{text:?}");
        assert_eq!(crate::scrollback::display_width(&text), 60, "right-aligned");
        // Dim, not the prose weight: the asymmetry is the point.
        assert!(
            line.spans[0]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::DIM)
        );
    }

    #[test]
    fn a_failed_call_is_red_keeps_its_line_and_gains_one_dim_tail() {
        let theme = Theme::plain();
        let mut failed = call("$ cargo test", "bash");
        failed.outcome = "error".into();
        failed.exit_code = Some(101);
        failed.tail = "error[E0432]: unresolved import `foo`".into();
        let line = failed.line(theme, 80);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("✗ exit 101"), "{text:?}");
        assert_eq!(line.spans[0].style.fg, Some(ratatui::style::Color::Red));
        let tail = failed.tail_line(theme, 80).expect("a tail row");
        let tail_text: String = tail.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(tail_text.starts_with("    error[E0432]"), "{tail_text:?}");
        assert!(
            tail.spans[0]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::DIM)
        );
    }

    #[test]
    fn a_cancelled_call_says_so_instead_of_ok() {
        let theme = Theme::plain();
        let mut cancelled = call("$ sleep 60", "bash");
        cancelled.outcome = "stopped".into();
        cancelled.exit_code = None;
        let text: String = cancelled
            .line(theme, 60)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("⏹ cancelled"), "{text:?}");
    }

    #[test]
    fn three_or_more_calls_collapse_but_errors_stay_visible() {
        let theme = Theme::plain();
        let mut group = Group::default();
        group.push(call("$ git --version", "bash"));
        group.push(call("$ python --version", "bash"));
        group.push(call("$ cargo fmt", "bash"));
        let rows = text(&group.close(theme, 80));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].starts_with('✓'), "{rows:?}");
        assert!(rows[0].contains("3 calls"), "{rows:?}");
        assert!(!rows[0].contains('·'), "no duration: {rows:?}");

        // A failure stays visible as its own line, plus its tail; it is not
        // counted in the collapsed line.
        let mut group = Group::default();
        group.push(call("$ a", "bash"));
        group.push(call("$ b", "bash"));
        let mut failed = call("$ cargo test", "bash");
        failed.outcome = "error".into();
        failed.exit_code = Some(101);
        failed.tail = "boom".into();
        group.push(failed);
        group.push(call("$ d", "bash"));
        let rows = text(&group.close(theme, 80));
        assert!(rows[0].contains("3 calls"), "{rows:?}");
        assert!(rows[1].contains("$ cargo test"), "{rows:?}");
        assert!(rows[1].contains("✗ exit 101"), "{rows:?}");
        assert!(rows[2].starts_with("    boom"), "{rows:?}");

        // An edit is a call like any other: it joins the fold.
        let mut group = Group::default();
        group.push(call("$ a", "bash"));
        group.push(call("edit src/lib.rs", "edit"));
        group.push(call("$ c", "bash"));
        let rows = text(&group.close(theme, 80));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(rows[0].contains("3 calls"), "{rows:?}");
    }

    #[test]
    fn two_calls_keep_one_line_each() {
        let theme = Theme::plain();
        let mut group = Group::default();
        group.push(call("$ git --version", "bash"));
        group.push(call("$ python --version", "bash"));
        let rows = text(&group.close(theme, 80));
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows[0].contains("$ git --version"));
        assert!(rows[1].contains("$ python --version"));
    }

    #[test]
    fn the_head_is_clipped_before_the_verdict_is() {
        let theme = Theme::plain();
        let long = format!("$ {}", "x".repeat(200));
        let mut wide = call(&long, "bash");
        wide.elapsed = None;
        let text: String = wide
            .line(theme, 40)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("ok · 29 B"), "{text:?}");
        assert!(text.contains('…'), "{text:?}");
        assert_eq!(crate::scrollback::display_width(&text), 40);
    }

    #[test]
    fn an_expansion_is_an_append_only_block_with_a_count_of_what_was_left_out() {
        let theme = Theme::plain();
        let many = (0..100)
            .map(|i| format!("row {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = call("$ seq", "bash").expanded(theme, 40, &many, EXPAND_ROWS);
        let rows = text(&lines);
        assert!(rows[0].contains("#1 $ seq"), "{rows:?}");
        assert_eq!(rows.len(), EXPAND_ROWS + 2, "header + rows + notice");
        assert!(rows.last().unwrap().contains("more rows"), "{rows:?}");
        assert!(rows[1].starts_with("    row 0"));
    }

    #[test]
    fn the_expand_policy_toggles_and_names_itself() {
        assert_eq!(Policy::default(), Policy::Collapsed);
        assert_eq!(Policy::Collapsed.word(), "collapsed");
        assert_eq!(Policy::Collapsed.toggled().word(), "full");
        assert_eq!(Policy::Full.toggled(), Policy::Collapsed);
    }

    #[test]
    fn sizes_read_as_bytes_or_kilobytes() {
        assert_eq!(bytes_label(0), "0 B");
        assert_eq!(bytes_label(1023), "1023 B");
        assert_eq!(bytes_label(1536), "1.5 KB");
        assert_eq!(bytes_label(3 * 1024 * 1024), "3.0 MB");
    }
}
