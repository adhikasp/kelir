//! Keyboard translation and the kitty keyboard-protocol handshake.
//! The keymap lives in one place so the app and the terminal setup cannot disagree.

use std::io::Write;

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseEvent,
    MouseEventKind, PushKeyboardEnhancementFlags,
};

use crate::tui::editor::Key;

/// A key press the TUI acts on (already translated from a terminal event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Press {
    Edit(Key),
    /// Backspace: the chip at the cursor (and its file) when there is one, a
    /// character otherwise.
    Backspace,
    CtrlC,
    ToggleTail,
    /// Esc: stop the turn, but only while working.
    Stop,
    Redraw,
    /// PageUp: scroll the transcript up one viewport.
    PageUp,
    /// PageDown: scroll the transcript down one viewport.
    PageDown,
    /// Home / End: the top / bottom of the transcript.
    Top,
    Bottom,
    /// Mouse wheel up / down: the transcript, a few rows at a time. A wheel is
    /// a nudge, not a page, so it moves [`WHEEL_ROWS`] rather than a viewport.
    ScrollUp,
    ScrollDown,
    /// Ctrl-Y: copy the last answer (OSC 52) or dump the whole transcript to a
    /// file in the session dir (with Shift).
    CopyLast,
    DumpTranscript,
}

/// Rows one wheel notch moves the transcript. A notch is a nudge: enough to
/// feel like the wheel is connected, small enough that a fast flick does not
/// jump past a whole answer.
pub const WHEEL_ROWS: usize = 3;

/// Translate one mouse event. Only the wheel is used: kelir takes normal
/// tracking (`?1000`) with SGR coordinates (`?1006`), so clicks and motion are
/// not reported at all, and a terminal never loses drag-selection or the
/// host's own scrolling to the app.
pub fn translate_mouse(mouse: MouseEvent) -> Option<Press> {
    match mouse.kind {
        MouseEventKind::ScrollUp => Some(Press::ScrollUp),
        MouseEventKind::ScrollDown => Some(Press::ScrollDown),
        _ => None,
    }
}

/// Ask the terminal for CSI-u key reporting so Shift-Enter survives a
/// multiplexer (Herdr forwards `ESC[13;2u` only when the pane asked for it).
pub fn push_enhancements(stdout: &mut impl Write) {
    let _ = ratatui::crossterm::execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
}

/// Translate one key event. `None` means "nothing to do".
pub fn translate(key: KeyEvent) -> Option<Press> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Char('c') if ctrl => Some(Press::CtrlC),
        KeyCode::Char('o') if ctrl => Some(Press::ToggleTail),
        KeyCode::Char('l') if ctrl => Some(Press::Redraw),
        KeyCode::Char('j') if ctrl => Some(Press::Edit(Key::Newline)),
        KeyCode::Char('k') if ctrl => Some(Press::Edit(Key::KillToEnd)),
        KeyCode::Char('u') if ctrl && !alt => Some(Press::Edit(Key::Clear)),
        KeyCode::Char('d') if ctrl && !alt => Some(Press::Edit(Key::KillNextWord)),
        KeyCode::Char('y') if ctrl && shift => Some(Press::DumpTranscript),
        KeyCode::Char('y') if ctrl => Some(Press::CopyLast),
        // Ctrl-Backspace arrives as raw 0x08 in a legacy terminal, which
        // crossterm decodes as Char('h')+CONTROL (0x08 = Ctrl-H).
        KeyCode::Char('h') if ctrl && !alt => Some(Press::Edit(Key::KillPrevWord)),
        KeyCode::Char(c) if !ctrl && !alt => Some(Press::Edit(Key::Char(c))),
        KeyCode::Char(c) if alt && !ctrl => match c {
            // Alt-Left/Alt-Right are the GUI word-jump keys, but some
            // terminals send them as Alt-h / Alt-l (readline muscle memory).
            'h' => Some(Press::Edit(Key::WordLeft)),
            'l' => Some(Press::Edit(Key::WordRight)),
            'd' => Some(Press::Edit(Key::KillNextWord)),
            _ => None,
        },
        KeyCode::Esc => Some(Press::Stop),
        // Enter sends; Shift-Enter / Alt-Enter (and Ctrl-J) start a new line.
        KeyCode::Enter if alt || shift => Some(Press::Edit(Key::Newline)),
        KeyCode::Enter => Some(Press::Edit(Key::Enter)),
        KeyCode::Backspace if ctrl => Some(Press::Edit(Key::KillPrevWord)),
        KeyCode::Backspace if alt => Some(Press::Edit(Key::KillNextWord)),
        KeyCode::Backspace => Some(Press::Backspace),
        KeyCode::Delete if ctrl => Some(Press::Edit(Key::KillNextWord)),
        KeyCode::Delete => Some(Press::Edit(Key::Delete)),
        KeyCode::Left if alt || ctrl => Some(Press::Edit(Key::WordLeft)),
        KeyCode::Right if alt || ctrl => Some(Press::Edit(Key::WordRight)),
        KeyCode::Left => Some(Press::Edit(Key::Left)),
        KeyCode::Right => Some(Press::Edit(Key::Right)),
        KeyCode::Up => Some(Press::Edit(Key::Up)),
        KeyCode::Down => Some(Press::Edit(Key::Down)),
        KeyCode::PageUp => Some(Press::PageUp),
        KeyCode::PageDown => Some(Press::PageDown),
        KeyCode::Home => Some(Press::Top),
        KeyCode::End => Some(Press::Bottom),
        _ => None,
    }
}
