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
    /// Esc: close the completion popup when one is open, otherwise stop the
    /// turn (and only while working).
    Stop,
    /// Tab: accept the highlighted completion row.
    Complete,
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
        // Delete the previous word. Four spellings reach this, all meaning the
        // same key to a person: Ctrl-W (0x17, readline and Herdr's own
        // `ctrl-backspace` binding), Ctrl-Backspace in a kitty terminal
        // (`CSI 127;5u`, decoded as Backspace+CONTROL below), Ctrl-H (0x08,
        // what a legacy terminal sends for Ctrl-Backspace), and Alt-Backspace.
        KeyCode::Char('w') if ctrl && !alt => Some(Press::Edit(Key::KillPrevWord)),
        // Ctrl-Backspace arrives as raw 0x08 in a legacy terminal, which
        // crossterm decodes as Char('h')+CONTROL (0x08 = Ctrl-H).
        KeyCode::Char('h') if ctrl && !alt => Some(Press::Edit(Key::KillPrevWord)),
        KeyCode::Char(c) if !ctrl && !alt => Some(Press::Edit(Key::Char(c))),
        KeyCode::Char(c) if alt && !ctrl => match c {
            // Alt-Left/Alt-Right are the GUI word-jump keys, but some
            // terminals and apps send them as Alt-h / Alt-l (readline
            // muscle memory) or Alt-b / Alt-f (emacs, and Herdr's own
            // `ctrl-left` / `ctrl-right` bindings).
            'h' | 'b' => Some(Press::Edit(Key::WordLeft)),
            'l' | 'f' => Some(Press::Edit(Key::WordRight)),
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
        KeyCode::Tab => Some(Press::Complete),
        KeyCode::PageUp => Some(Press::PageUp),
        KeyCode::PageDown => Some(Press::PageDown),
        KeyCode::Home => Some(Press::Top),
        KeyCode::End => Some(Press::Bottom),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key a terminal event decodes to, as `translate` sees it.
    fn at(code: KeyCode, modifiers: KeyModifiers) -> Option<Press> {
        translate(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn every_word_delete_spelling_deletes_the_previous_word() {
        let want = Some(Press::Edit(Key::KillPrevWord));
        // Ctrl-W: readline, and what Herdr's `ctrl-backspace` sends.
        assert_eq!(at(KeyCode::Char('w'), KeyModifiers::CONTROL), want);
        // Ctrl-H: the raw 0x08 a legacy terminal sends for Ctrl-Backspace.
        assert_eq!(at(KeyCode::Char('h'), KeyModifiers::CONTROL), want);
        // Ctrl-Backspace (kitty `CSI 127;5u`) and Alt-Backspace.
        assert_eq!(at(KeyCode::Backspace, KeyModifiers::CONTROL), want);
    }

    #[test]
    fn every_word_motion_spelling_moves_by_a_word() {
        let left = Some(Press::Edit(Key::WordLeft));
        let right = Some(Press::Edit(Key::WordRight));
        // Ctrl-Left/Right: `CSI 1;5D` / `CSI 1;3D` through crossterm's modifier path.
        assert_eq!(at(KeyCode::Left, KeyModifiers::CONTROL), left);
        assert_eq!(at(KeyCode::Right, KeyModifiers::CONTROL), right);
        assert_eq!(at(KeyCode::Left, KeyModifiers::ALT), left);
        assert_eq!(at(KeyCode::Right, KeyModifiers::ALT), right);
        // Alt-h/l (readline) and Alt-b/f (emacs; Herdr's `ctrl-left`/`ctrl-right`).
        assert_eq!(at(KeyCode::Char('h'), KeyModifiers::ALT), left);
        assert_eq!(at(KeyCode::Char('b'), KeyModifiers::ALT), left);
        assert_eq!(at(KeyCode::Char('l'), KeyModifiers::ALT), right);
        assert_eq!(at(KeyCode::Char('f'), KeyModifiers::ALT), right);
    }

    #[test]
    fn word_keys_do_not_swallow_plain_typing() {
        // The same characters, unmodified, are text: `w`, `b` and `f` must type.
        for c in ['w', 'b', 'f', 'h', 'l'] {
            assert_eq!(
                at(KeyCode::Char(c), KeyModifiers::NONE),
                Some(Press::Edit(Key::Char(c))),
                "{c} must type as text"
            );
        }
        // Ctrl-B and Ctrl-F are not claimed: `Ctrl-F` would read as a forward
        // word motion in emacs, but a plain `f` is not a word key here and
        // claiming two more control chords for no gain is not worth it.
        assert_eq!(at(KeyCode::Char('b'), KeyModifiers::CONTROL), None);
        assert_eq!(at(KeyCode::Char('f'), KeyModifiers::CONTROL), None);
    }

    #[test]
    fn tab_completes_and_esc_is_one_key() {
        assert_eq!(at(KeyCode::Tab, KeyModifiers::NONE), Some(Press::Complete));
        // Shift-Tab is not a reverse cycle: the list is a ring and Up is the
        // key that goes back.
        assert_eq!(at(KeyCode::Tab, KeyModifiers::SHIFT), Some(Press::Complete));
        assert_eq!(at(KeyCode::Esc, KeyModifiers::NONE), Some(Press::Stop));
    }

    #[test]
    fn a_release_event_is_ignored() {
        let mut key = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        key.kind = KeyEventKind::Release;
        assert_eq!(translate(key), None);
    }
}
