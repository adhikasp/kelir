//! Terminal setup and teardown. The TUI owns the whole screen: the alternate
//! buffer, raw mode, bracketed paste and the kitty keyboard protocol are all
//! entered here and all left here, on a clean exit, an error or a panic.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ratatui::crossterm::cursor::MoveTo;
use ratatui::crossterm::event::{Event, KeyEventKind};
use ratatui::crossterm::terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use tokio::sync::mpsc;

use super::keys;
use super::{BannerLine, Tui, View};
use crate::ipc::Incoming;
use crate::tui::theme::Theme;

/// How long the input thread waits for a key before checking again.
const KEY_POLL: Duration = Duration::from_millis(30);

/// Ask for wheel reporting only: normal tracking (`?1000`) with SGR coordinates
/// (`?1006`). Deliberately not `?1002`/`?1003` (drag and any-motion tracking):
/// those turn every hover into an event and take a pane's own drag-selection
/// away from the terminal. Written as bytes rather than as crossterm commands,
/// whose `EnableMouseCapture` asks for all five modes.
const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1006h";
const MOUSE_OFF: &[u8] = b"\x1b[?1006l\x1b[?1000l";

/// Turn wheel reporting on. Best effort: a terminal that does not know the
/// modes ignores them and the keys still scroll.
fn mouse_on(stdout: &mut impl Write) {
    let _ = stdout.write_all(MOUSE_ON);
}

/// Restores the terminal on every exit path, including a panic between the
/// setup and the loop. Crossterm's teardown sequences are idempotent, so a
/// normal exit and a panic hook may both run it.
pub struct RestoreGuard;

impl RestoreGuard {
    pub fn new() -> Self {
        Self
    }
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        restore();
    }
}

/// Set by whichever exit path restores first, so a panic hook and the Drop
/// guard never emit the teardown twice.
static RESTORED: AtomicBool = AtomicBool::new(false);

/// Put the screen back the way a person expects after a full-screen app: a
/// blank screen, one line saying how to come back.
///
/// Called after [`restore`] has left the alternate buffer, so this paints on
/// the shell's own screen. Only the visible rows are cleared: the person's
/// scrollback is theirs, and wiping it (`3J`) to tidy up after a TUI would
/// destroy history they never asked to lose.
pub fn clear_and_hint(session_id: Option<&str>) {
    let mut stdout = std::io::stdout();
    let _ = ratatui::crossterm::execute!(stdout, MoveTo(0, 0), Clear(ClearType::All));
    if let Some(id) = session_id {
        let _ = ratatui::crossterm::execute!(
            stdout,
            ratatui::crossterm::style::Print(format!("resume: kelir tui --resume {id}\r\n"))
        );
    }
    let _ = stdout.flush();
}

/// Leave the alternate screen, raw mode, paste, the mouse and kitty keyboard mode.
pub fn restore() {
    if RESTORED.swap(true, Ordering::AcqRel) {
        return;
    }
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(MOUSE_OFF);
    let _ = ratatui::crossterm::execute!(
        stdout,
        ratatui::crossterm::event::DisableBracketedPaste,
        ratatui::crossterm::event::PopKeyboardEnhancementFlags,
        LeaveAlternateScreen,
        ratatui::crossterm::cursor::Show,
    );
    let _ = stdout.flush();
    let _ = ratatui::crossterm::terminal::disable_raw_mode();
}

/// Install a panic hook that restores the terminal before the previous hook
/// prints the panic. Called after `try_init`, which installs ratatui's own
/// full-screen restore hook; ours adds paste and keyboard handshake cleanup.
pub fn install_panic_hook(previous: Box<dyn Fn(&std::panic::PanicHookInfo) + Send + Sync>) {
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}

/// Take the terminal (raw mode, alternate screen, panic hook, keyboard), the
/// event stream and the banner. The caller has already checked stdout is a TTY.
pub fn start(
    events: mpsc::UnboundedReceiver<View>,
    to_tui: mpsc::UnboundedSender<View>,
    verbs: mpsc::UnboundedSender<Incoming>,
    banner: &[BannerLine],
    cap: Option<u32>,
    token: String,
    theme: Theme,
) -> std::io::Result<Tui> {
    let (width, height) = ratatui::crossterm::terminal::size().unwrap_or((80, 24));
    // Capture the hook from before ratatui's own one, so teardown happens
    // exactly once: ours chains to the hook that was there first, not to the
    // one `try_init_with_options` installs.
    let previous_hook = std::panic::take_hook();
    // `try_init_with_options` enables raw mode but, unlike `try_init`, does not
    // enter the alternate screen. The TUI paints every row, so without it the
    // app is drawn over the shell's scrollback — and `restore`'s
    // `LeaveAlternateScreen` then hands back a terminal the person never left.
    let mut stdout = std::io::stdout();
    ratatui::crossterm::execute!(stdout, EnterAlternateScreen)?;
    let _ = stdout.flush();
    let terminal = match ratatui::try_init_with_options(ratatui::TerminalOptions {
        viewport: ratatui::Viewport::Fullscreen,
    }) {
        Ok(terminal) => terminal,
        Err(error) => {
            // Raw mode may be on and the alternate buffer is entered: leave
            // both before the caller reports the failure.
            restore();
            return Err(error);
        }
    };
    install_panic_hook(previous_hook);
    keys::push_enhancements(&mut stdout);
    mouse_on(&mut stdout);
    let _ = ratatui::crossterm::execute!(stdout, ratatui::crossterm::event::EnableBracketedPaste);
    let _ = stdout.flush();

    let mut app = super::App::new(width as usize, height as usize, to_tui, verbs, token);
    app.theme = theme;
    for line in banner {
        if line.dim {
            app.push_dim_text(&line.text);
        } else {
            app.push_text(&line.text);
        }
    }
    app.status.limit = cap;

    let (key_tx, key_rx) = mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        loop {
            match ratatui::crossterm::event::poll(KEY_POLL) {
                Ok(true) => match ratatui::crossterm::event::read() {
                    Ok(event) => {
                        let keep = match &event {
                            Event::Key(key) => key.kind != KeyEventKind::Release,
                            // The wheel only: clicks are reported because the
                            // terminal reports the wheel that way, not because
                            // the TUI wants them.
                            Event::Mouse(mouse) => keys::translate_mouse(*mouse).is_some(),
                            Event::Paste(_) | Event::Resize(_, _) => true,
                            _ => false,
                        };
                        if keep && key_tx.send(event).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                },
                Ok(false) => {}
                Err(_) => return,
            }
        }
    });

    Ok(Tui {
        app,
        terminal,
        events,
        keys: key_rx,
        tick: None,
        last_paint: None,
        guard: RestoreGuard::new(),
    })
}
