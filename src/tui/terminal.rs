//! Terminal setup and teardown. The TUI owns the whole screen: the alternate
//! buffer, raw mode, bracketed paste and the kitty keyboard protocol are all
//! entered here and all left here, on a clean exit, an error or a panic.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ratatui::crossterm::event::{Event, KeyEventKind};
use ratatui::crossterm::terminal::LeaveAlternateScreen;
use tokio::sync::mpsc;

use super::keys;
use super::{BannerLine, Tui, View};
use crate::ipc::Incoming;
use crate::tui::theme::Theme;

/// How long the input thread waits for a key before checking again.
const KEY_POLL: Duration = Duration::from_millis(30);

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

/// Leave the alternate screen, raw mode, paste and kitty keyboard mode.
pub fn restore() {
    if RESTORED.swap(true, Ordering::AcqRel) {
        return;
    }
    let mut stdout = std::io::stdout();
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
    // ratatui::try_init enables raw mode, enters the alternate screen and
    // installs its own restore-on-panic hook. Our hook (below) chains to it.
    // ratatui::try_init installs its own restore-on-panic hook. Capture the
    // hook from before it and chain to that, so teardown happens exactly once.
    let previous_hook = std::panic::take_hook();
    let terminal = ratatui::try_init_with_options(ratatui::TerminalOptions {
        viewport: ratatui::Viewport::Fullscreen,
    })?;
    install_panic_hook(previous_hook);
    let mut stdout = std::io::stdout();
    keys::push_enhancements(&mut stdout);
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
