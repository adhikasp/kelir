//! Full-screen TUI driven in a real PTY against the scripted mock: turns,
//! status reports, images, key handling and panic recovery.
#![allow(clippy::pedantic, clippy::nursery)]

use super::tui_output::{herdr_records, plain};
use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::support::{Mock, Reply, sse, text_turn, tool_turn};

// -------------------------------------------------------------------- PTY

pub(super) struct PtyRun {
    pub(super) code: u64,
    pub(super) screen: String,
    pub(super) session_dir: std::path::PathBuf,
}

/// Drive the real binary in a pty: type a message, wait for the answer, quit.
pub(super) fn drive_in_a_pty(
    cwd: std::path::PathBuf,
    config: std::path::PathBuf,
    state: std::path::PathBuf,
    base_url: String,
    preamble: &[u8],
    expect: Vec<String>,
) -> PtyRun {
    drive_pty_in_idle_or_turn(
        cwd,
        config,
        state,
        base_url,
        Some(b"hello there"),
        preamble,
        expect,
        PtyInput {
            submit: b"\r",
            message_echo: "hello there",
        },
    )
}

/// The same, but the session only idles: nothing is typed except `preamble`, so
/// the turn machinery never runs and whatever the session says on its own (the
/// banner, an idle metadata fetch landing) is all that reaches the screen.
pub(super) fn drive_idle_in_a_pty(
    cwd: std::path::PathBuf,
    config: std::path::PathBuf,
    state: std::path::PathBuf,
    base_url: String,
    expect: Vec<String>,
) -> PtyRun {
    drive_pty_in_idle_or_turn(
        cwd,
        config,
        state,
        base_url,
        None,
        b"",
        expect,
        PtyInput {
            submit: b"\r",
            message_echo: "",
        },
    )
}

/// True once some session under `state` has a line in its `events.jsonl`
/// containing `needle` (a verb the TUI sent itself, for one).
pub(super) fn wait_for_session_log(state: &std::path::Path, needle: &str, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        let found = std::fs::read_dir(state.join("sessions"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| {
                std::fs::read_to_string(entry.path().join("events.jsonl"))
                    .map(|text| text.contains(needle))
                    .unwrap_or(false)
            });
        if found {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The part of a PTY drive that only the Shift-Enter scenario varies.
pub(super) struct PtyInput<'a> {
    pub(super) submit: &'a [u8],
    pub(super) message_echo: &'a str,
}

#[expect(
    clippy::too_many_arguments,
    reason = "test-only PTY driver; every knob is explicit"
)]
pub(super) fn drive_pty_in_idle_or_turn(
    cwd: std::path::PathBuf,
    config: std::path::PathBuf,
    state: std::path::PathBuf,
    base_url: String,
    message: Option<&[u8]>,
    preamble: &[u8],
    expect: Vec<String>,
    input: PtyInput<'_>,
) -> PtyRun {
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
    // The pty inherits this process's environment: a kelir session's own
    // `KELIR_SOCK`, a `TERM_PROGRAM=herdr-*` or a stray test hook would change
    // what the run under test is. Before the env below, never after.
    crate::support::scrub_pty(&mut command);
    command.cwd(&cwd);
    command.env("KELIR_CONFIG", &config);
    command.env("KELIR_CONFIG_DIR", state.join("no-config"));
    command.env("KELIR_STATE_DIR", &state);
    command.env("KELIR_HOME", state.join("home"));
    command.env("KELIR_BASE_URL", &base_url);
    command.env("OPENROUTER_API_KEY", "test-openrouter-key");
    command.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(command).expect("spawn kelir");
    // The slave handle must go, or the pty never reports EOF.
    drop(pair.slave);
    let writer = Arc::new(Mutex::new(pair.master.take_writer().expect("pty writer")));
    let mut reader = pair.master.try_clone_reader().expect("pty reader");

    let screen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = screen.clone();
    // The test is the terminal here, so it has to answer the terminal queries
    // ConPTY makes (a cursor-position report above all); a real terminal, or
    // Herdr's pane, does the same.
    let responder = writer.clone();
    let reader_task = std::thread::spawn(move || {
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

    let text = |screen: &Arc<Mutex<Vec<u8>>>| -> String {
        String::from_utf8_lossy(&screen.lock().unwrap()).into_owned()
    };
    let wait_for = |screen: &Arc<Mutex<Vec<u8>>>, needle: &str, limit: Duration| -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            let raw = text(screen);
            if raw.contains(needle) || (!needle.contains('\u{1b}') && plain(&raw).contains(needle))
            {
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
    let mut typed = false;
    if wait_for(&screen, "kelir", crate::support::CEILING) {
        // A moment for raw mode; then anything the test types first, the
        // message, and Enter.
        // A bracketed paste is only delivered as a paste once the app has
        // enabled the mode, and that mode switch is the last thing the TUI does
        // when it takes the terminal over: wait for the terminal to *show* it
        // (STEER-9; this was a fixed 600 ms). A terminal layer that swallows
        // the sequence costs the short fallback, not a failure.
        let _ = wait_for(&screen, "\u{1b}[?2004h", Duration::from_secs(3));
        if !preamble.is_empty() {
            send(preamble);
            if preamble == b"\x03" {
                // Ctrl-C is a verb: it has been dealt with when the session
                // logged it. Typing the message before that races the stop
                // against the turn it must not stop (the old 200 ms sleep).
                assert!(
                    wait_for_session_log(&state, "\"name\":\"stop\"", crate::support::CEILING),
                    "the Ctrl-C never reached the session: {:?}",
                    text(&screen)
                );
            } else {
                // Anything else is text: it has been taken when it is echoed.
                let tail: String = String::from_utf8_lossy(preamble)
                    .chars()
                    .rev()
                    .take(6)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let _ = wait_for(&screen, &tail, Duration::from_secs(3));
            }
        }
        if let Some(message) = message {
            send(message);
            let _ = wait_for(&screen, input.message_echo, Duration::from_secs(20));
            send(input.submit);
        }
        typed = true;
    }
    assert!(typed, "the TUI never drew: {:?}", text(&screen));
    for needle in &expect {
        assert!(
            wait_for(&screen, needle, crate::support::CEILING),
            "{needle:?} never reached the terminal: {:?}",
            text(&screen)
        );
    }
    send(b"/quit");
    let _ = wait_for(&screen, "/quit", Duration::from_secs(5));
    send(b"\r");

    let deadline = Instant::now() + crate::support::CEILING;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.exit_code() as u64,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.clone_killer().kill();
                panic!("kelir did not exit; screen: {:?}", text(&screen));
            }
        }
    };
    // The reader thread is blocked on the pty master; it dies with the
    // process, so it is not joined (joining would wait for an EOF that only
    // closing the writer can bring, and the writer is still held above).
    drop(reader_task);
    let screen = text(&screen);
    let sessions = state.join("sessions");
    let session_dir = std::fs::read_dir(&sessions)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .next()
        .expect("a session dir");
    PtyRun {
        code,
        screen,
        session_dir,
    }
}

#[derive(Debug)]
pub(super) struct IdleRun {
    pub(super) code: u64,
    pub(super) bytes_while_idle: usize,
}

/// Drive one turn, wait for it to settle, then count every byte the TUI writes
/// while it sits idle for two seconds.
pub(super) fn drive_and_count_idle_bytes(
    cwd: std::path::PathBuf,
    config: std::path::PathBuf,
    state: std::path::PathBuf,
    base_url: String,
) -> IdleRun {
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
    // A kelir session's own env would change what the run under test does.
    crate::support::scrub_pty(&mut command);
    command.cwd(&cwd);
    command.env("KELIR_CONFIG", &config);
    command.env("KELIR_CONFIG_DIR", state.join("no-config"));
    command.env("KELIR_STATE_DIR", &state);
    command.env("KELIR_HOME", state.join("home"));
    command.env("KELIR_BASE_URL", &base_url);
    command.env("OPENROUTER_API_KEY", "test-openrouter-key");
    command.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(command).expect("spawn kelir");
    drop(pair.slave);
    let writer = Arc::new(Mutex::new(pair.master.take_writer().expect("pty writer")));
    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let screen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = screen.clone();
    let responder = writer.clone();
    let reader_task = std::thread::spawn(move || {
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
    let text = |screen: &Arc<Mutex<Vec<u8>>>| -> String {
        String::from_utf8_lossy(&screen.lock().unwrap()).into_owned()
    };
    let wait_for = |screen: &Arc<Mutex<Vec<u8>>>, needle: &str, limit: Duration| -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            let raw = text(screen);
            if raw.contains(needle) || (!needle.contains('\u{1b}') && plain(&raw).contains(needle))
            {
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
        wait_for(&screen, "kelir", crate::support::CEILING),
        "the TUI never drew"
    );
    std::thread::sleep(Duration::from_millis(200));
    send(b"hi");
    std::thread::sleep(Duration::from_millis(100));
    send(b"\r");
    assert!(
        wait_for(&screen, "DONE", crate::support::CEILING),
        "the answer never arrived: {:?}",
        text(&screen)
    );
    // Wait for the state row to say idle again (the turn is over and the
    // spinner is gone).
    assert!(
        wait_for(&screen, "idle", crate::support::CEILING),
        "the TUI never went idle: {:?}",
        text(&screen)
    );
    std::thread::sleep(Duration::from_millis(300));
    let before = screen.lock().unwrap().len();
    std::thread::sleep(Duration::from_secs(2));
    let after = screen.lock().unwrap().len();
    send(b"/quit");
    std::thread::sleep(Duration::from_millis(100));
    send(b"\r");
    let deadline = Instant::now() + crate::support::CEILING;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.exit_code() as u64,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.clone_killer().kill();
                panic!("kelir did not exit");
            }
        }
    };
    drop(reader_task);
    IdleRun {
        code,
        bytes_while_idle: after - before,
    }
}

/// A bracketed paste of a PNG path (which is what a terminal sends for a drag
/// and drop or a clipboard path) becomes a chip, the prompt carries the
/// pointer, and no image bytes are in it.

/// A 4x3 PNG: bytes a real decoder would accept as a header.
pub(super) fn tiny_png() -> Vec<u8> {
    let mut bytes: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&4u32.to_be_bytes());
    bytes.extend_from_slice(&3u32.to_be_bytes());
    bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
    bytes
}

/// The failure word: `error` goes out over the same OSC record — and a failed
/// turn does not end the session.
///
/// A provider that is *down* is a pause, not a failure (STEER-5), so the
/// failure here is a provider that refuses the model: 400 is an answer, not an
/// outage, and it is never retried.

/// Start the TUI with `KELIR_TEST_PANIC=1` and collect what the terminal saw.
pub(super) fn panic_in_a_pty(
    cwd: std::path::PathBuf,
    config: std::path::PathBuf,
    state: std::path::PathBuf,
) -> (String, u64) {
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 40,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pty");
    let mut command = portable_pty::CommandBuilder::new(env!("CARGO_BIN_EXE_kelir"));
    // The pty inherits this process's environment: a kelir session's own
    // `KELIR_SOCK`, a `TERM_PROGRAM=herdr-*` or a stray test hook would change
    // what the run under test is. Before the env below, never after.
    crate::support::scrub_pty(&mut command);
    command.cwd(&cwd);
    command.env("KELIR_CONFIG", &config);
    command.env("KELIR_CONFIG_DIR", state.join("no-config"));
    command.env("KELIR_STATE_DIR", &state);
    command.env("KELIR_HOME", state.join("home"));
    command.env("KELIR_BASE_URL", "http://127.0.0.1:9");
    command.env("OPENROUTER_API_KEY", "test-openrouter-key");
    command.env("TERM", "xterm-256color");
    command.env("KELIR_TEST_PANIC", "1");
    let mut child = pair.slave.spawn_command(command).expect("spawn kelir");
    drop(pair.slave);
    let writer = Arc::new(Mutex::new(pair.master.take_writer().expect("pty writer")));
    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let screen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = screen.clone();
    let responder = writer.clone();
    let reader_task = std::thread::spawn(move || {
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
    let deadline = Instant::now() + crate::support::CEILING;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.exit_code() as u64,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.clone_killer().kill();
                panic!(
                    "kelir did not exit after the panic: {:?}",
                    String::from_utf8_lossy(&screen.lock().unwrap())
                );
            }
        }
    };
    drop(reader_task);
    // The panic message comes from the default hook after ours; give the pty
    // reader a moment to drain it before the snapshot.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline
        && !String::from_utf8_lossy(&screen.lock().unwrap()).contains("KELIR_TEST_PANIC")
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    (
        String::from_utf8_lossy(&screen.lock().unwrap()).into_owned(),
        code,
    )
}
