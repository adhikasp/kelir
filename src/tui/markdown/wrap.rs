//! The agent's markdown, rendered into styled rows.
//! Three pieces, each testable alone:

use super::*;

// ---------------------------------------------------------------- wrapping

/// Wrap `runs` to `width`, prefixing the first row with `first` and every continuation row with `cont`.
#[optimize(speed)]
pub fn wrap_runs(first: &[Run], cont: &[Run], runs: &[Run], width: usize) -> Vec<Row> {
    let width = width.max(1);
    let first_width = prefix_width(first);
    let cont_width = prefix_width(cont);
    if width <= cont_width {
        // Narrower than the indent itself: show the prefix and clip the text.
        let room = width.saturating_sub(cont_width).max(1);
        let text = runs.iter().map(|run| run.text.as_str()).collect::<String>();
        let mut row = Row {
            runs: cont.to_vec(),
        };
        row.runs.push(Run::new(
            clip_columns(&text, room, '…'),
            runs.first().map(|run| run.tone).unwrap_or(Tone::Prose),
        ));
        return vec![row];
    }
    let cells: Vec<(char, Tone)> = runs
        .iter()
        .flat_map(|run| run.text.chars().map(move |c| (c, run.tone)))
        .collect();
    let mut rows = Vec::new();
    let mut rest: &[(char, Tone)] = &cells;
    let mut prefix = first;
    let mut room = width - first_width;
    while !rest.is_empty() {
        // How many characters fit in `room` columns.
        let mut used = 0usize;
        let mut fit = 0usize;
        for (c, _) in rest {
            let c_width = width_of(*c);
            if used + c_width > room && fit > 0 {
                break;
            }
            used += c_width;
            fit += 1;
        }
        if fit == rest.len() {
            let mut row = Row {
                runs: prefix.to_vec(),
            };
            row.runs.extend(coalesce(rest));
            trim_end(&mut row);
            rows.push(row);
            break;
        }
        // Break at the last space inside the window, else mid-word.
        let window = &rest[..fit];
        let (take, skip) = match window.iter().rposition(|(c, _)| *c == ' ') {
            Some(space) if space > 0 => (space, 1),
            _ => (fit, 0),
        };
        let mut row = Row {
            runs: prefix.to_vec(),
        };
        row.runs.extend(coalesce(&rest[..take]));
        trim_end(&mut row);
        rows.push(row);
        rest = &rest[take + skip..];
        while rest.first().is_some_and(|(c, _)| *c == ' ') {
            rest = &rest[1..];
        }
        prefix = cont;
        room = width.saturating_sub(cont_width);
    }
    if rows.is_empty() {
        rows.push(Row {
            runs: first.to_vec(),
        });
    }
    rows
}

/// Cut `prefix` back to `room` columns, dropping whole characters from its end.
/// An indent is decoration; the text it decorates is not.
pub(crate) fn fit_prefix(prefix: Vec<Run>, room: usize) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    let mut used = 0usize;
    for run in prefix {
        let mut text = String::new();
        for c in run.text.chars() {
            let width = width_of(c);
            if used + width > room {
                break;
            }
            used += width;
            text.push(c);
        }
        if !text.is_empty() {
            out.push(Run::new(text, run.tone));
        }
        if used >= room {
            break;
        }
    }
    out
}

pub(crate) fn prefix_width(prefix: &[Run]) -> usize {
    prefix.iter().map(|run| text_width(&run.text)).sum()
}

/// Consecutive characters with one tone, as runs.
pub(crate) fn coalesce(cells: &[(char, Tone)]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (c, tone) in cells {
        match runs.last_mut() {
            Some(last) if last.tone == *tone => last.text.push(*c),
            _ => runs.push(Run::new(c.to_string(), *tone)),
        }
    }
    runs
}

pub(crate) fn trim_end(row: &mut Row) {
    while let Some(last) = row.runs.last_mut() {
        let trimmed = last.text.trim_end().len();
        if trimmed == 0 {
            row.runs.pop();
        } else {
            last.text.truncate(trimmed);
            break;
        }
    }
}

/// Clip `text` to `width` columns with `marker` at the end when something was
/// cut.
pub fn clip_columns(text: &str, width: usize, marker: char) -> String {
    if text_width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let c_width = width_of(c);
        if used + c_width > width.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += c_width;
    }
    out.push(marker);
    out
}
