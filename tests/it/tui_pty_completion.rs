//! The `/` popup and Tab in a real terminal: the bytes a terminal actually
//! sends (Tab is `0x09`, Esc `0x1b`) and the fact that the real binary paints
//! the list. The ranking and the keymap are unit-tested; this proves the wiring
//! end to end, reading the *screen* the escapes add up to rather than the byte
//! stream, which a diffing repaint makes unreadable.
#![allow(clippy::pedantic, clippy::nursery)]

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::tui_screen::Screen;

/// The marker on the highlighted row, spelled out rather than imported: this
/// file is also run against the base commit, where the TUI has no such symbol
/// and the test must fail for one reason only — no popup was ever drawn.
const MARK: &str = "▸ ";

/// What the screen held at each step.
struct Shots {
    /// The idle screen, before anything is typed.
    before: String,
    /// After `/s`: the popup is up.
    open: String,
    /// After Tab: the command in the line, the list gone.
    accepted: String,
    /// After Esc on a fresh `/m`: the line kept, the list gone.
    dismissed: String,
}

fn drive() -> Shots {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().to_path_buf();
    let cwd = state.join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = state.join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();

    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system
        .openpty(portable_pty::PtySize {
            rows: 40,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pty");
    let mut command = portable_pty::CommandBuilder::new(env!("CARGO_BIN_EXE_kelir"));
    crate::support::scrub_pty(&mut command);
    command.cwd(&cwd);
    command.env("KELIR_CONFIG", &config);
    command.env("KELIR_CONFIG_DIR", state.join("no-config"));
    command.env("KELIR_STATE_DIR", &state);
    command.env("KELIR_HOME", state.join("home"));
    // Nothing here talks to a model: the idle TUI only has to draw.
    command.env("KELIR_BASE_URL", "http://127.0.0.1:9");
    command.env("OPENROUTER_API_KEY", "test-openrouter-key");
    command.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(command).expect("spawn kelir");
    drop(pair.slave);
    let writer = Arc::new(Mutex::new(pair.master.take_writer().expect("pty writer")));
    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let screen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = screen.clone();
    // The test is the terminal: answer the queries the TUI makes.
    let responder = writer.clone();
    let _reader = std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    let bytes = &chunk[..n];
                    sink.lock().unwrap().extend_from_slice(bytes);
                    let text = String::from_utf8_lossy(bytes);
                    let mut replies = Vec::new();
                    for _ in 0..text.matches("\u{1b}[6n").count() {
                        replies.extend_from_slice(b"\x1b[1;1R");
                    }
                    for _ in 0..text.matches("\u{1b}[c").count() {
                        replies.extend_from_slice(b"\x1b[?1;2c");
                    }
                    if !replies.is_empty() {
                        let mut out = responder.lock().unwrap();
                        let _ = out.write_all(&replies);
                        let _ = out.flush();
                    }
                }
            }
        }
    });
    let bytes = |screen: &Arc<Mutex<Vec<u8>>>| -> Vec<u8> { screen.lock().unwrap().clone() };
    let shown = |screen: &Arc<Mutex<Vec<u8>>>| -> String { Screen::replay(&bytes(screen)).text() };
    let wait_raw = |needle: &str, limit: Duration| -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if String::from_utf8_lossy(&bytes(&screen)).contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    };
    let wait_screen = |needle: &str, limit: Duration| -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if shown(&screen).contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    };
    let send = |bytes: &[u8]| {
        let mut out = writer.lock().unwrap();
        out.write_all(bytes).unwrap();
        out.flush().unwrap();
    };
    assert!(
        wait_raw("\u{1b}[?2004h", crate::support::CEILING),
        "the TUI never took the terminal over"
    );
    assert!(
        wait_screen("›", crate::support::CEILING),
        "no prompt: {:?}",
        shown(&screen)
    );
    let before = shown(&screen);
    assert!(
        !before.contains("/stop [reason]"),
        "the popup was up before anything was typed: {before:?}"
    );

    send(b"/s");
    assert!(
        wait_screen("/stop [reason]", crate::support::CEILING),
        "the popup never drew: {:?}",
        shown(&screen)
    );
    // The list is a real overlay: the `/stop` description came with it, and
    // the row is marked as the highlighted one.
    assert!(
        wait_screen("stop the turn now", crate::support::CEILING),
        "the popup drew labels without descriptions"
    );
    let open = shown(&screen);
    assert!(
        open.contains(&format!("{}/stop [reason]", MARK)),
        "the highlight mark is missing: {open:?}"
    );
    assert!(open.contains("› /s"), "the line is `/s`: {open:?}");

    // Tab accepts the highlighted row: `/stop ` in the line, the list gone.
    send(b"\t");
    assert!(
        wait_screen("› /stop", crate::support::CEILING),
        "Tab did not type the command: {:?}",
        shown(&screen)
    );
    assert!(
        !shown(&screen).contains(MARK),
        "the popup stayed open after Tab: {:?}",
        shown(&screen)
    );
    let accepted = shown(&screen);

    // Esc closes the list and leaves the line alone.
    send(b"\x15");
    send(b"/m");
    assert!(
        wait_screen("/model <p/m>", crate::support::CEILING),
        "the popup did not reopen: {:?}",
        shown(&screen)
    );
    send(b"\x1b");
    std::thread::sleep(Duration::from_millis(400));
    let dismissed = shown(&screen);
    assert!(
        !dismissed.contains("/model <p/m>"),
        "Esc did not close the popup: {dismissed:?}"
    );
    assert!(
        dismissed.contains("› /m"),
        "Esc ate the line: {dismissed:?}"
    );

    // Clear the line so `/quit` is a command, then quit.
    send(b"\x15");
    send(b"/quit\r");
    let deadline = Instant::now() + crate::support::CEILING;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.clone_killer().kill();
                panic!("kelir did not exit; screen: {:?}", shown(&screen));
            }
        }
    }
    Shots {
        before,
        open,
        accepted,
        dismissed,
    }
}

#[test]
fn the_real_binary_paints_the_popup_and_tab_fills_the_line() {
    let shots = drive();
    // The popup is an overlay above the editor: the rows it lists appear and
    // the editor keeps the line it had, on the screen's last rows.
    let listed = shots
        .open
        .lines()
        .filter(|line| line.contains("  /"))
        .count();
    assert!(
        listed >= 3,
        "the list drew {} rows:\n{}",
        listed,
        shots.open
    );
    assert!(shots.open.ends_with("› /s\n0 tok in"), "{}", shots.open);
    // The idle screen had no list and the same last two rows.
    assert!(shots.before.ends_with("›\n0 tok in"), "{}", shots.before);
    assert!(!shots.before.contains("/skill <name>"));
    // Tab put the command in the line; the list is gone.
    assert!(
        shots.accepted.ends_with("› /stop\n0 tok in"),
        "{}",
        shots.accepted
    );
    assert!(
        !shots.accepted.contains("/skill <name>"),
        "{}",
        shots.accepted
    );
    // Esc closed the list and left the line.
    assert!(
        shots.dismissed.ends_with("› /m\n0 tok in"),
        "{}",
        shots.dismissed
    );
    assert!(
        !shots.dismissed.contains("/model <p/m>"),
        "{}",
        shots.dismissed
    );
}
