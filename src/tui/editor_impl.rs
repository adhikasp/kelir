//! The editor's line-loading helper. Split out of `editor.rs` so the production
//! part of that file stays under the shape gate's 800-line cap.
//! `load` replaces the buffer with a text, with the cursor at the end.

use super::editor::{Chip, Editor};

pub(super) fn load(editor: &mut Editor, text: &str) {
    editor.lines = text.split('\n').map(str::to_string).collect();
    editor.row = editor.lines.len() - 1;
    editor.col = editor.lines[editor.row].chars().count();
}

// ------------------------------------------------------------- chip geometry
//
// A chip is one element of the buffer, but its label is ordinary text with a
// space in it (`[image 1]`). Every rule that thinks in words has to be told
// where the chips are, or a word delete cuts a label in half and leaves a chip
// that no longer matches its text. These live here, next to `load`, because
// `editor.rs` is at the shape gate's line cap.

impl Editor {
    /// Grow `start..end` until it covers every chip it touches. A chip's label
    /// has a space in it (`[image 1]`), so a whitespace word never contains a
    /// whole chip: cutting the word alone would leave a broken half-label in
    /// the prompt and a chip that no longer matches its text.
    pub(super) fn widen_over_chips(&self, mut start: usize, mut end: usize) -> (usize, usize) {
        let line = &self.lines[self.row];
        let mut grew = true;
        while grew {
            grew = false;
            for chip in &self.chips {
                let Some((from, to)) = chip_span(line, chip) else {
                    continue;
                };
                if from < end && to > start {
                    let (new_start, new_end) = (start.min(from), end.max(to));
                    grew |= (new_start, new_end) != (start, end);
                    start = new_start;
                    end = new_end;
                }
            }
        }
        (start, end)
    }

    /// Move a word-motion landing point out of a chip: a chip is one element,
    /// so the cursor goes to the far edge of the chip it would land inside.
    pub(super) fn snap_out_of_chip(&self, col: usize, forward: bool) -> usize {
        let line = &self.lines[self.row];
        for chip in &self.chips {
            let Some((from, to)) = chip_span(line, chip) else {
                continue;
            };
            if col > from && col < to {
                return if forward { to } else { from };
            }
        }
        col
    }
}

/// Where a chip's label sits in `line`, as a character range.
pub(super) fn chip_span(line: &str, chip: &Chip) -> Option<(usize, usize)> {
    let at = line.find(&chip.label)?;
    let from = line[..at].chars().count();
    Some((from, from + chip.label.chars().count()))
}
