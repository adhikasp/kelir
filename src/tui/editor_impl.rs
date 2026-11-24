//! The editor's line-loading helper. Split out of `editor.rs` so the production
//! part of that file stays under the shape gate's 800-line cap.
//! `load` replaces the buffer with a text, with the cursor at the end.

use super::editor::Editor;

pub(super) fn load(editor: &mut Editor, text: &str) {
    editor.lines = text.split('\n').map(str::to_string).collect();
    editor.row = editor.lines.len() - 1;
    editor.col = editor.lines[editor.row].chars().count();
}
