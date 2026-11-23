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
struct IdleRun {
    code: u64,
    bytes_while_idle: usize,
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

/// The facts row stays calm even with a cap: no step counter is ambient, and
/// the banner is where the cap is named.

#[tokio::test]
async fn the_tui_runs_a_turn_in_a_pty() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("PARROT", 0)))]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();
    let state = temp.path().join("state");
    let base_url = mock.base_url();
    let run = tokio::task::spawn_blocking(move || {
        drive_in_a_pty(cwd, config, state, base_url, b"", vec!["PARROT".into()])
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    // The answer was rendered (the scrollback ends up in the pty's output).
    assert!(run.screen.contains("PARROT"), "screen: {:?}", run.screen);
    // Every frame is wrapped in synchronised output, so a frame is never torn
    // (`ESC[?2026h` … `ESC[?2026l`). Terminals that do not know the pair ignore
    // it, so nothing has to probe for it.
    assert!(
        run.screen.contains("\u{1b}[?2026h"),
        "frames are not synchronised: {:?}",
        run.screen
    );
    assert!(
        run.screen.contains("\u{1b}[?2026l"),
        "a sync frame is never closed: {:?}",
        run.screen
    );
    // The banner no longer carries the window: the startup answer is silent
    // (it is still in `state`, for anyone who asks).
    assert!(
        !run.screen.contains("window 262144 tokens"),
        "the window is not ambient in the banner; screen: {:?}",
        run.screen
    );
    // Herdr's status: exactly one record per change, in the only form this
    // Herdr accepts (`state=<word>` and nothing else).
    assert_eq!(
        herdr_records(&run.screen),
        vec![
            "\u{1b}]7501;state=working\u{1b}\\".to_string(),
            "\u{1b}]7501;state=done\u{1b}\\".to_string(),
        ],
        "screen: {:?}",
        run.screen
    );

    // The log is the same as any other session's: a `say` from the TUI, the
    // answer, the turn, and the end.
    let text = std::fs::read_to_string(run.session_dir.join("events.jsonl")).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let types: Vec<&str> = events
        .iter()
        .map(|event| event["type"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(types.first(), Some(&"session_started"), "{types:?}");
    assert_eq!(
        types.iter().filter(|t| **t == "user").count(),
        1,
        "{types:?}"
    );
    let user = events
        .iter()
        .find(|event| event["type"] == "user")
        .expect("a user event");
    assert_eq!(user["text"], "hello there");
    assert_eq!(user["from"], "tui", "the message came from the TUI");
    assert!(types.contains(&"assistant"), "{types:?}");
    assert!(types.contains(&"assistant"), "{types:?}");
    assert!(types.contains(&"turn_ended"), "{types:?}");
    assert_eq!(types.last(), Some(&"session_ended"), "{types:?}");
    let ended = events
        .iter()
        .find(|event| event["type"] == "turn_ended")
        .unwrap();
    assert_eq!(ended["outcome"], "done");
    let verb = events
        .iter()
        .find(|event| event["type"] == "verb")
        .expect("the say was logged as a verb");
    assert_eq!(verb["from"], "tui");
    assert_eq!(verb["name"], "say");
}

/// A markdown answer arrives rendered: the `##`, `**` and `|---|` never reach
/// the screen, and the answer lands in the terminal's scrollback.
#[tokio::test]
async fn the_tui_renders_a_markdown_answer_and_commits_it() {
    let answer = "## Files\n\nSome **bold** words and a `code` span.\n\n\
                  | a | b |\n|---|---|\n| 1 | 2 |\n\n```sh\ncargo test\n```\n";
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn(answer, 0)))]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();
    let state = temp.path().join("state");
    let base_url = mock.base_url();
    let run = tokio::task::spawn_blocking(move || {
        drive_in_a_pty(
            cwd,
            config,
            state,
            base_url,
            b"",
            vec!["cargo test".into(), "Files".into()],
        )
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    for markdown in ["## Files", "**bold**", "|---", "```"] {
        assert!(
            !run.screen.contains(markdown),
            "raw markdown {markdown:?} reached the screen: {:?}",
            run.screen
        );
    }
    // The plain words are there, and the code block keeps its line. The pty
    // stream interleaves escape sequences with the text, so the assertion is
    // on the stripped text (a reader sees the stripped text).
    let text = plain(&run.screen);
    // A differential repaint only writes changed cells, so spaces that were
    // already on screen are absent from the byte stream. Compare words.
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(compact.contains("Someboldwords"), "{text:?}");
    assert!(compact.contains("cargotest"), "{text:?}");
    // A done turn ends on its prose: no check mark line.
    // (the state row's idle text is checked by the snapshot tests; the pty's
    // repaint diff only rewrites what changed, so it need not appear here)
}

/// Idle writes nothing: after the turn ends, the TUI writes nothing for two
/// seconds (no spinner, no elapsed tick). A constant-rate repaint would fail it.
#[tokio::test]
async fn an_idle_tui_writes_nothing_for_two_seconds() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("DONE", 0)))]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();
    let state = temp.path().join("state");
    let base_url = mock.base_url();
    let quiet = tokio::task::spawn_blocking(move || {
        drive_and_count_idle_bytes(cwd, config, state, base_url)
    })
    .await
    .expect("the pty driver");
    assert_eq!(quiet.code, 0, "{quiet:?}");
    assert_eq!(
        quiet.bytes_while_idle, 0,
        "the idle TUI wrote {} bytes in two seconds",
        quiet.bytes_while_idle
    );
}
/// Ctrl-C with nothing running is a no-op: it must not stop the next turn at
/// its first boundary (the verb is dropped in idle, not queued).
#[tokio::test]
async fn a_ctrl_c_before_the_first_message_does_not_stop_the_turn() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("PARROT", 0)))]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();
    let state = temp.path().join("state");
    let base_url = mock.base_url();
    let run = tokio::task::spawn_blocking(move || {
        drive_in_a_pty(cwd, config, state, base_url, b"\x03", vec!["PARROT".into()])
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    let text = std::fs::read_to_string(run.session_dir.join("events.jsonl")).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let stopped = events
        .iter()
        .any(|event| event["type"] == "turn_ended" && event["outcome"] == "stopped");
    assert!(!stopped, "the Ctrl-C stopped the first turn: {text}");
    let done = events
        .iter()
        .find(|event| event["type"] == "turn_ended")
        .expect("a turn ended");
    assert_eq!(done["outcome"], "done", "{text}");
    // The stop verb was still logged: it is a verb like any other.
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "verb" && event["name"] == "stop"),
        "{text}"
    );
}

/// N5: a panic while the TUI owns the terminal must leave the terminal usable.
/// `KELIR_TEST_PANIC` panics right after raw mode is on, so this drives the
/// real panic path: the hook has to turn bracketed paste off, show the cursor,
/// leave the alternate screen (ratatui's own hook) and drop raw mode.
#[tokio::test]
async fn a_panic_in_the_tui_restores_the_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();
    let state = temp.path().join("state");
    let (screen, code) = tokio::task::spawn_blocking(move || panic_in_a_pty(cwd, config, state))
        .await
        .expect("the pty driver");

    // The TUI panic hook restores the terminal before the panic is printed.
    // `cfg!(debug_assertions)` distinguishes the test build (`panic =
    // "unwind"`, session ends cleanly) from the release binary (`panic =
    // "abort"`, non-zero exit). Both must leave the terminal usable (#23).
    if cfg!(debug_assertions) {
        assert_eq!(
            code, 0,
            "a panic must not hang or crash the session: {screen:?}"
        );
    } else {
        assert_ne!(code, 0, "a release panic must abort: {screen:?}");
    }
    let message = screen
        .find("KELIR_TEST_PANIC: injected panic")
        .unwrap_or_else(|| panic!("no panic message: {screen:?}"));
    // The teardown ran: bracketed paste was turned off after being turned on,
    // before the message was printed. (Cursor visibility is a WinAPI call on
    // Windows, so it is not in the byte stream; the live drive in
    // `scripts/live/m2_status_check.sh` checks the shell is usable after an
    // exit, and `a_ctrl_c...`/`the_tui_runs_a_turn_in_a_pty` check the clean
    // path.)
    let paste_on = screen.rfind("\u{1b}[?2004h").expect("paste was never on");
    let paste_off = screen
        .rfind("\u{1b}[?2004l")
        .expect("bracketed paste left on");
    assert!(
        paste_on < paste_off && paste_off < message,
        "the teardown did not run before the panic message: {screen:?}"
    );
}
