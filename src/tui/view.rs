//! The full-screen view: status/format helpers, the fixed layout and painting.
//! The transcript is owned by [`super::app::App`]; nothing here writes to the
//! terminal's own scrollback.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use serde_json::Value;

use crate::markdown::{Layout, NoHighlight};
use crate::scrollback;
use crate::theme::Theme;
use crate::tools_view;

use super::Status;
use super::app::{App, Entry};

/// The editor prompt's width (`› `), which the cursor's column includes.
pub const PROMPT_WIDTH: usize = 2;
/// A second Ctrl-C within this window quits instead of stopping.
pub const DOUBLE_CTRL_C: Duration = Duration::from_millis(1000);
pub const RETRY_COUNTDOWN_MAX: Duration = Duration::from_secs(6 * 60 * 60);
pub const TAIL_BYTES: usize = 2048;
/// The spinner changes this often while working; the elapsed text once a
/// second. Nothing ticks while idle.
pub const SPINNER_TICK: Duration = Duration::from_millis(100);
/// Frames are coalesced to at most one per this interval.
pub const FRAME_MIN: Duration = Duration::from_millis(33);
/// Editor rows never exceed this, even for a pasted paragraph.
pub const MAX_EDITOR_ROWS: u16 = 12;

/// Token counts as humans read them: `999`, `1.5k`, `12.1k`.
pub fn tokens(count: u64) -> String {
    if count < 1000 {
        count.to_string()
    } else {
        format!("{:.1}k", count as f64 / 1000.0)
    }
}

pub fn status_text(status: &Status) -> String {
    let word = status_word(status);
    if let Some((line, until)) = &status.retrying {
        let left = until.saturating_duration_since(Instant::now());
        // A slot wait has no deadline a person should read as a number; only a
        // real countdown is shown.
        if left < RETRY_COUNTDOWN_MAX {
            return format!(
                "kelir {word} · {line} · {} left",
                crate::retry::seconds_text(left)
            );
        }
        return format!("kelir {word} · {line}");
    }
    facts_text(status)
}

/// One state, one word: `done` reads as `idle` here (the state row's kind for it
/// is idle too, so the two rows cannot disagree), while `paused` and `error`
/// keep their own word because they are not idle.
fn status_word(status: &Status) -> &'static str {
    match status.phase {
        super::Phase::Done => super::Phase::Idle.word(),
        other => other.word(),
    }
}

/// The facts row when nothing is waiting: the session's own words, the model
/// and the input tokens of the last request (not the running sum).
fn facts_text(status: &Status) -> String {
    let mut parts = Vec::new();
    if !status.title.is_empty() {
        parts.push(status.title.clone());
    }
    if !status.model.is_empty() {
        parts.push(status.model.clone());
    }
    parts.push(format!("{} tok in", tokens(status.last_prompt_tokens)));
    parts.join(" · ")
}

/// The facts row trimmed for a narrow pane.
pub fn facts_for_width(status: &Status, width: usize) -> String {
    let full = status_text(status);
    if scrollback::display_width(&full) <= width {
        return full;
    }
    let shorter = status_text(&Status {
        model: String::new(),
        ..status.clone()
    });
    if scrollback::display_width(&shorter) <= width {
        return shorter;
    }
    fit(
        &format!(
            "{} · {} tok in",
            if status.title.is_empty() {
                "kelir".to_string()
            } else {
                status.title.clone()
            },
            tokens(status.last_prompt_tokens)
        ),
        width,
    )
}

/// The wait line as the pane shows it: typing `stop` into this prompt sends a
/// message to the model, so the pane names the key that actually stops it.
pub fn tui_wait_line(line: &str) -> String {
    line.replace("say stop to cancel", "esc stops")
        .replace(", say stop", ", esc stops")
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

/// `$ cargo test` / `edit src/a.rs`: the compact one-line identity of a call.
pub fn call_head(name: &str, input: &Value) -> String {
    if let Some(label) = tools_view::description(input) {
        return label;
    }
    match name {
        "bash" => match input.get("command").and_then(Value::as_str) {
            Some(command) => format!("$ {}", crate::util::one_line_visible(command, 60)),
            None => "$ bash".to_string(),
        },
        "edit" => match input.get("path").and_then(Value::as_str) {
            Some(path) => format!("edit {path}"),
            None => "edit".to_string(),
        },
        other => format!(
            "{other} {}",
            crate::util::one_line_visible(&input.to_string(), 60)
        ),
    }
}

/// The reason out of a pause message, for the state row's detail.
pub fn pause_reason(message: &str) -> Option<String> {
    let reason = message.split("; say continue").next().unwrap_or("");
    let reason = reason.trim();
    (!reason.is_empty()).then(|| reason.to_string())
}

/// `1 step`, `2 steps`: the count and its noun, never `1 steps`.
/// The one-line label for a running call in the state row.
pub fn call_label(name: &str, input: &Value) -> String {
    if let Some(label) = tools_view::description(input) {
        return label;
    }
    let detail = input
        .get("command")
        .and_then(Value::as_str)
        .or_else(|| input.get("path").and_then(Value::as_str))
        .or_else(|| input.get("focus").and_then(Value::as_str))
        .map(|text| text.lines().next().unwrap_or("").trim().to_string())
        .unwrap_or_default();
    if detail.is_empty() {
        name.to_string()
    } else {
        format!("{name}: {}", crate::util::truncate_chars(&detail, 60))
    }
}

/// The last `max` bytes of a call's output, as whole text lines.
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
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let window = max as u64 + 1;
    let start = len.saturating_sub(window);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes)?;
    Ok(tail_of(&bytes, max))
}

pub fn last_line(text: &str) -> String {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| crate::util::truncate_chars(line.trim(), 200))
        .unwrap_or_default()
}

/// The colour a transcript entry's plain text gets.
pub fn entry_style(app: &App, entry: &Entry) -> ratatui::style::Style {
    match entry {
        Entry::User(_) => app.theme.user(),
        Entry::Dim(_) => app.theme.dim(),
        Entry::Error(_) => app.theme.error(),
        Entry::Plain(_) | Entry::Markdown(_) | Entry::Settled { .. } | Entry::Call(_) => {
            app.theme.prose()
        }
    }
}

/// Render every transcript row at the current width. The streaming draft is
/// already an entry in the transcript (updated in place), so it appears once.
pub fn rows(app: &App) -> Vec<Line<'static>> {
    app.cached_rows.clone()
}

/// Markdown text as styled lines at a given width.
pub fn markdown_rows(text: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let layout = Layout::with_glyphs(width, theme.glyphs);
    crate::markdown::render(text, layout)
        .iter()
        .map(|row| row.line(theme, &NoHighlight))
        .collect()
}

/// One transcript entry, rendered at `width`.
pub fn entry_lines(app: &App, entry: &Entry, width: usize) -> Vec<Line<'static>> {
    match entry {
        Entry::Settled {
            width: at, lines, ..
        } if *at == width => lines.clone(),
        Entry::Settled { text, .. } => markdown_rows(text, width, app.theme),
        Entry::Markdown(text) => markdown_rows(text, width, app.theme),
        Entry::Call(call) => {
            let mut lines = vec![call.line(app.theme, width)];
            if call.failed()
                && let Some(tail) = call.tail_line(app.theme, width)
            {
                lines.push(tail);
            }
            lines
        }
        Entry::Plain(text) | Entry::User(text) | Entry::Dim(text) | Entry::Error(text) => {
            let style = entry_style(app, entry);
            scrollback::wrap(text, width)
                .into_iter()
                .map(|row| Line::from(Span::styled(row, style)))
                .collect()
        }
    }
}

// ------------------------------------------------------------------ layout

/// The full screen's four areas: transcript, state, editor, facts.
#[derive(Debug, Clone, Copy)]
pub struct Areas {
    pub transcript: Rect,
    pub state: Rect,
    pub editor: Rect,
    pub facts: Rect,
}

/// The layout: a transcript viewport that takes all the space above the fixed
/// rows; the editor grows with its wrapped content up to [`MAX_EDITOR_ROWS`].
pub fn layout(area: Rect, editor_rows: u16) -> Areas {
    if area.height == 0 {
        return Areas {
            transcript: area,
            state: area,
            editor: area,
            facts: area,
        };
    }
    let facts_h = u16::from(area.height >= 4);
    let state_h = u16::from(area.height >= 3);
    let editor_h = if area.height > 2 {
        (area.height - 2).min(editor_rows.clamp(1, MAX_EDITOR_ROWS))
    } else {
        1.min(area.height)
    };
    let transcript_h = area.height.saturating_sub(facts_h + state_h + editor_h);
    let transcript = Rect::new(area.x, area.y, area.width, transcript_h);
    let state = Rect::new(area.x, transcript.y + transcript_h, area.width, state_h);
    let editor = Rect::new(area.x, state.y + state_h, area.width, editor_h);
    let facts = Rect::new(area.x, editor.y + editor_h, area.width, facts_h);
    Areas {
        transcript,
        state,
        editor,
        facts,
    }
}

/// Paint one frame. Everything on screen comes from `App`; the terminal's own
/// scrollback is never touched, and the mouse is never captured.
pub fn render(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    app.set_size(area.width as usize, area.height as usize);
    let rows = rows(app);
    let areas = layout(area, app.editor.visual_rows().len() as u16);
    let top = app.scroll_top(rows.len(), areas.transcript.height as usize);
    frame.render_widget(Paragraph::new(rows).scroll((top, 0)), areas.transcript);
    if app.show_tail {
        render_tail(app, frame, area, areas.facts);
    } else {
        render_editor(app, frame, areas.editor);
        render_state(app, frame, areas.state);
    }
    render_facts(app, frame, areas.facts);
}

/// The editor: the window of rows that keeps the cursor visible, the prompt
/// dimmed while the agent works, the image chips reversed, and the cursor
/// itself counted in display columns.
fn render_editor(app: &mut App, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let theme = app.theme;
    let prompt_style = if app.activity.kind.working() {
        theme.dim()
    } else {
        theme.user()
    };
    app.editor.set_width(area.width as usize);
    let (first, visible) = app.editor.display_window(area.height as usize);
    let visual = app.editor.visual_rows();
    let mut text: Vec<Line<'static>> = Vec::new();
    for row in visual.iter().skip(first).take(visible) {
        let prefix = if row.first {
            theme.glyphs.prompt()
        } else {
            theme.glyphs.prompt_pad()
        };
        let mut spans = vec![Span::styled(prefix.to_string(), prompt_style)];
        let line: String = app.editor.lines()[row.logical]
            .chars()
            .skip(row.start)
            .take(row.end - row.start)
            .collect();
        spans.extend(chip_spans(app, &line, theme));
        text.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(text), area);
    let (row, column) = app.editor.cursor_display();
    let x = (area.x as usize + column).min(frame.area().width.saturating_sub(1) as usize) as u16;
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
        let mut earliest: Option<(usize, &crate::editor::Chip)> = None;
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
                    spans.push(Span::styled(rest[..at].to_string(), theme.user()));
                }
                spans.push(Span::styled(chip.label.clone(), theme.chip()));
                rest = &rest[at + chip.label.len()..];
            }
            None => {
                spans.push(Span::styled(rest.to_string(), theme.user()));
                break;
            }
        }
    }
    spans
}

/// The Ctrl-O overlay: the last call's header and output tail, dim.
fn render_tail(app: &App, frame: &mut Frame, screen: Rect, facts: Rect) {
    let height = facts.y.saturating_sub(screen.y);
    let area = Rect {
        x: screen.x,
        y: screen.y,
        width: screen.width,
        height,
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(call) = app.last_call() {
        lines.push(call.line(app.theme, area.width as usize));
    }
    lines.extend(app.tail.iter().map(|row| Line::from(row.clone())));
    let rows = app.tail.len() + usize::from(app.last_call().is_some());
    let scroll = rows
        .saturating_sub(area.height as usize)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(
        Paragraph::new(lines)
            .style(app.theme.dim())
            .scroll((scroll, 0)),
        area,
    );
}

/// The state row: glyph, word, elapsed, detail — dim while idle, cyan while
/// working, yellow while paused, red on an error. A scroll pause adds the
/// count of new rows the viewport has not followed yet.
fn render_state(app: &App, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let theme = app.theme;
    let mut line = if app.note.is_empty() {
        app.activity
            .line(theme, app.frame, Instant::now(), area.width as usize)
    } else {
        let text = fit(
            &format!(" {} · {}", app.activity.kind.word(), app.note),
            area.width as usize,
        );
        Line::from(Span::styled(text, theme.dim()))
    };
    if app.pending_new > 0 {
        let marker = fit(
            &format!(
                "{} · {} new lines · End follows",
                line_text(&line),
                app.pending_new
            ),
            area.width as usize,
        );
        line = Line::from(Span::styled(marker, theme.waiting()));
    }
    frame.render_widget(Paragraph::new(line), area);
}

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
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

/// The plain text of every rendered row, for copy/export.
pub fn plain_text(rows: &[Line<'static>]) -> String {
    let mut out = String::new();
    for row in rows {
        out.push_str(&line_text(row));
        out.push('\n');
    }
    out
}
