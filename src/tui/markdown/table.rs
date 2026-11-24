//! The agent's markdown, rendered into styled rows.
//! Three pieces, each testable alone:

use super::*;

// ------------------------------------------------------------------ tables

/// A table, boxed when it fits and as plain `a | b | c` rows when it does not
/// (narrow panes and wide records), per's rule.
pub(crate) fn render_table(
    table: &TableState,
    layout: Layout,
    glyphs: crate::tui::theme::Glyphs,
) -> Vec<Row> {
    let columns = table
        .body
        .iter()
        .chain(std::iter::once(&table.head))
        .map(Vec::len)
        .max()
        .unwrap_or(0);
    if columns == 0 {
        return Vec::new();
    }
    let mut widths = vec![1usize; columns];
    for (index, cell) in table.head.iter().enumerate() {
        widths[index] = widths[index].max(cell_width(cell));
    }
    for row in &table.body {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell_width(cell));
        }
    }
    let boxed = layout.width >= 30;
    // Total width a boxed table needs: the borders, the padding and the cells.
    let borders = if boxed {
        columns * 3 + 1
    } else {
        columns * 3 - 1
    };
    if boxed && widths.iter().sum::<usize>() + borders <= layout.width {
        boxed_table(table, &widths, glyphs)
    } else {
        // Shrink the widest columns first, down to 4 columns each.
        let mut widths = widths.clone();
        let budget = layout.width.saturating_sub(borders);
        while widths.iter().sum::<usize>() > budget {
            let Some((index, _)) = widths
                .iter()
                .enumerate()
                .filter(|(_, width)| **width > 4)
                .max_by_key(|(_, width)| **width)
            else {
                break;
            };
            widths[index] -= 1;
        }
        if boxed && widths.iter().sum::<usize>() + borders <= layout.width {
            boxed_table(table, &widths, glyphs)
        } else {
            plain_table(table, layout.width, glyphs)
        }
    }
}

fn cell_width(cell: &[Run]) -> usize {
    cell.iter().map(|run| text_width(&run.text)).sum()
}

fn boxed_table(
    table: &TableState,
    widths: &[usize],
    glyphs: crate::tui::theme::Glyphs,
) -> Vec<Row> {
    let pieces = glyphs.table();
    let mut rows = Vec::new();
    rows.push(border(widths, pieces.tl, pieces.tm, pieces.tr, pieces.h));
    if !table.head.is_empty() {
        rows.push(table_row(
            &table.head,
            widths,
            pieces.v,
            Some(Tone::Strong),
            glyphs,
        ));
        rows.push(border(widths, pieces.ml, pieces.mm, pieces.mr, pieces.h));
    }
    for row in &table.body {
        rows.push(table_row(row, widths, pieces.v, None, glyphs));
    }
    rows.push(border(widths, pieces.bl, pieces.bm, pieces.br, pieces.h));
    rows
}

fn border(widths: &[usize], left: char, mid: char, right: char, horizontal: char) -> Row {
    let mut text = String::new();
    text.push(left);
    for (index, width) in widths.iter().enumerate() {
        text.push_str(&horizontal.to_string().repeat(width + 2));
        text.push(if index + 1 == widths.len() {
            right
        } else {
            mid
        });
    }
    Row::plain(text, Tone::Dim)
}

fn table_row(
    cells: &[Vec<Run>],
    widths: &[usize],
    separator: char,
    tone: Option<Tone>,
    glyphs: crate::tui::theme::Glyphs,
) -> Row {
    let mut runs = vec![Run::new(separator.to_string(), Tone::Dim)];
    for (index, width) in widths.iter().enumerate() {
        let cell = cells.get(index).map(Vec::as_slice).unwrap_or(&[]);
        let mut text = String::new();
        for run in cell {
            text.push_str(&run.text);
        }
        text = text.replace('\n', " ");
        let clipped = clip_columns(&text, *width, glyphs.ellipsis());
        let pad = width.saturating_sub(text_width(&clipped));
        let cell_tone = tone
            .or_else(|| cell.first().map(|run| run.tone))
            .unwrap_or(Tone::Prose);
        runs.push(Run::new(
            format!(" {clipped}{} ", " ".repeat(pad)),
            cell_tone,
        ));
        runs.push(Run::new(separator.to_string(), Tone::Dim));
    }
    Row { runs }
}

/// No borders: `a | b | c` with a bold header row. Cells are clipped so one
/// record stays one row, which is what makes a table readable at 40 columns.
fn plain_table(table: &TableState, width: usize, glyphs: crate::tui::theme::Glyphs) -> Vec<Row> {
    if width < 8 {
        // Below a single cell plus one separator there is no table to show:
        // one clipped cell per row is the honest degradation.
        let mut rows = Vec::new();
        for cells in std::iter::once(&table.head).chain(table.body.iter()) {
            let mut text = String::new();
            for cell in cells {
                for run in cell {
                    text.push_str(&run.text);
                }
                text.push(' ');
            }
            let text = text.trim_end().replace('\n', " ");
            rows.push(Row::plain(
                clip_columns(&text, width, glyphs.ellipsis()),
                Tone::Prose,
            ));
        }
        return rows;
    }
    plain_table_rows(table, width, glyphs)
}

fn plain_table_rows(
    table: &TableState,
    width: usize,
    glyphs: crate::tui::theme::Glyphs,
) -> Vec<Row> {
    // The cells and the ` | ` between them must fit the pane, not the other way round: a fixed minimum
    // cell width made a 3-column table one column too wide at narrow widths.
    let separators = 3usize;
    let mut columns = table
        .body
        .iter()
        .chain(std::iter::once(&table.head))
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .max(1);
    // As many columns as fit with at least two columns of text each; one when
    // even that is too wide.
    while columns > 1 && columns * 2 + (columns - 1) * separators > width {
        columns -= 1;
    }
    let budget = width
        .saturating_sub((columns - 1) * separators)
        .max(columns);
    let each = (budget / columns).max(1);
    let cells =
        |cells: &[Vec<Run>]| -> Vec<Vec<Run>> { cells.iter().take(columns).cloned().collect() };
    let mut rows = Vec::new();
    if !table.head.is_empty() {
        rows.push(plain_row(&cells(&table.head), each, true, glyphs));
    }
    for row in &table.body {
        rows.push(plain_row(&cells(row), each, false, glyphs));
    }
    rows
}

fn plain_row(
    cells: &[Vec<Run>],
    each: usize,
    strong: bool,
    glyphs: crate::tui::theme::Glyphs,
) -> Row {
    let mut runs = Vec::new();
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            runs.push(Run::new(" | ", Tone::Dim));
        }
        let mut text = String::new();
        for run in cell {
            text.push_str(&run.text);
        }
        let text = text.replace('\n', " ");
        let tone = if strong {
            Tone::Strong
        } else {
            cell.first().map(|run| run.tone).unwrap_or(Tone::Prose)
        };
        runs.push(Run::new(clip_columns(&text, each, glyphs.ellipsis()), tone));
    }
    Row { runs }
}
