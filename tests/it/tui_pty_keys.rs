//! PTY-driven TUI cases for the word keys and `/new` (spec 009, US1 and US2).
//!
//! These go through a real terminal because the two things worth proving are
//! only true there: the byte sequences a terminal actually sends for
//! Ctrl-Backspace and Alt-b, and the fact that `/new` leaves a live session and
//! starts a fresh process.
#![allow(clippy::pedantic, clippy::nursery)]

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::support::{Mock, Reply, sse, text_turn};

/// What happened in one PTY run.
struct Run {
    screen: String,
    /// Every session directory under `state`, sorted by name, after the run.
    sessions: Vec<std::path::PathBuf>,
}

/// What a step waits for before it types.
enum Ready {
    /// The TUI is up: it has enabled bracketed paste this many times (a `/new`
    /// execs a fresh process, so the second enable is the fresh TUI).
    Drawn(usize),
    /// Something reached the screen, e.g. the answer to the turn before.
    Showing(&'static str),
}

/// One thing to type, and when it is safe to type it.
struct Step {
    ready: Ready,
    keys: Vec<u8>,
}

fn drawn(keys: &[u8]) -> Step {
    Step {
        ready: Ready::Drawn(1),
        keys: keys.to_vec(),
    }
}

fn after_showing(needle: &'static str, keys: &[u8]) -> Step {
    Step {
        ready: Ready::Showing(needle),
        keys: keys.to_vec(),
    }
}

fn freshly_relaunched(times: usize, keys: &[u8]) -> Step {
    Step {
        ready: Ready::Drawn(times),
        keys: keys.to_vec(),
    }
}

fn drive(
    state: std::path::PathBuf,
    config: std::path::PathBuf,
    base_url: String,
    script: Vec<Step>,
) -> Run {
    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system
        .openpty(portable_pty::PtySize {
            rows: 40,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pty");
    let cwd = state.join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut command = portable_pty::CommandBuilder::new(env!("CARGO_BIN_EXE_kelir"));
    crate::support::scrub_pty(&mut command);
    command.cwd(&cwd);
    command.env("KELIR_CONFIG", &config);
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
    let text = |screen: &Arc<Mutex<Vec<u8>>>| -> String {
        String::from_utf8_lossy(&screen.lock().unwrap()).into_owned()
    };
    let wait_for = |needle: &str, limit: Duration| -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if text(&screen).contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    };
    let wait_for_count = |needle: &str, times: usize, limit: Duration| -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if text(&screen).matches(needle).count() >= times {
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
        wait_for("kelir", crate::support::CEILING),
        "the TUI never drew"
    );
    for step in &script {
        // The app takes the terminal over with bracketed paste on; keys sent
        // before that land in a terminal that is not in raw mode yet.
        let ready = match step.ready {
            Ready::Drawn(times) => wait_for_count("\u{1b}[?2004h", times, crate::support::CEILING),
            Ready::Showing(needle) => wait_for(needle, crate::support::CEILING),
        };
        assert!(
            ready,
            "the screen was not ready for {:?}: {:?}",
            String::from_utf8_lossy(&step.keys),
            text(&screen)
        );
        send(&step.keys);
        std::thread::sleep(Duration::from_millis(300));
    }
    // `/quit` typed in one go: a `/new` execs the new session, so the terminal
    // and its keystrokes are the same either way.
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
                panic!("kelir did not exit; screen: {:?}", text(&screen));
            }
        }
    }
    // A `/new` execs a child kelir; the pty is closed only when every writer is
    // gone, so wait for the sessions to settle rather than for EOF.
    std::thread::sleep(Duration::from_millis(300));
    let sessions = std::fs::read_dir(state.join("sessions"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    Run {
        screen: text(&screen),
        sessions,
    }
}

fn config(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("config.toml");
    std::fs::write(&path, "provider = \"openrouter\"\nmodel = \"test/model\"\n").unwrap();
    path
}

/// Ctrl-Backspace deletes the word before the cursor, and Ctrl-Left/Right step
/// by words: the chip is one word, and the label's own space does not split it.
#[tokio::test]
async fn ctrl_backspace_takes_a_whole_word_in_a_pty() {
    let temp = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let (config, base, state) = (
        config(temp.path()),
        mock.base_url(),
        temp.path().join("state"),
    );
    let run = tokio::task::spawn_blocking(move || {
        drive(
            state,
            config,
            base,
            vec![
                // "alpha beta", then Ctrl-Backspace, then Enter.
                drawn(b"alpha beta"),
                drawn(&[0x17]),
                drawn(b"\r"),
            ],
        )
    })
    .await
    .expect("the pty driver");
    assert!(
        run.screen.contains("alpha"),
        "Ctrl-Backspace took `beta`, not the whole line: {:?}",
        run.screen
    );
    let log = std::fs::read_to_string(run.sessions[0].join("events.jsonl")).unwrap();
    assert!(
        log.contains("\"text\":\"alpha\""),
        "the prompt was `alpha`: {log}"
    );
}

/// `/new` ends the session it was typed in and starts a fresh one: a second
/// session directory, its own log, and the old one closed.
#[tokio::test]
async fn new_ends_this_session_and_starts_a_fresh_one() {
    let temp = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first", 0))),
        Reply::Sse(sse(&text_turn("second", 0))),
    ])
    .await;
    let (config, base, state) = (
        config(temp.path()),
        mock.base_url(),
        temp.path().join("state"),
    );
    let run = tokio::task::spawn_blocking(move || {
        drive(
            state,
            config,
            base,
            vec![
                drawn(b"hello there\r"),
                // `/new` is a command, so it is typed once the answer to the
                // turn before it is on the screen.
                after_showing("first", b"/new\r"),
                // The fresh kelir is ready for keys when it enables bracketed
                // paste for the second time.
                freshly_relaunched(2, b"a new question\r"),
            ],
        )
    })
    .await
    .expect("the pty driver");
    assert!(
        run.sessions.len() >= 2,
        "`/new` did not start a second session: {:?}",
        run.sessions
    );
    let mut first = String::new();
    let mut second = String::new();
    for dir in &run.sessions {
        let log = std::fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default();
        if log.contains("hello there") {
            first = log.clone();
        }
        if log.contains("a new question") {
            second = log;
        }
    }
    assert!(
        first.contains("hello there"),
        "the first turn's log: {first}"
    );
    assert!(
        second.contains("a new question"),
        "the new session took the next question: {second}"
    );
    // The session that was left says how to come back to it.
    assert!(
        first.contains("kelir tui --resume"),
        "the old session names its resume command: {first}"
    );
}
