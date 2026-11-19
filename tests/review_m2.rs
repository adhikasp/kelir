//! Reviewer-added tests for M2 (see docs/reviews/M2-review.md).
//!
//! R1 drives the real binary in a pty through many short turns and checks the
//! terminal byte stream itself: the Herdr status writer runs on the session task
//! and the TUI paints on its own task, both through `std::io::stdout()`, so an
//! OSC 7501 record can land in the middle of a CSI sequence. A cut CSI prints its
//! tail (`17;200H`) as text on the screen.

mod support;

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use support::{Mock, Reply, sse, text_turn};

const TURNS: usize = 40;

/// Every `ESC [` whose parameter bytes are followed by an ESC instead of a final
/// byte: a CSI sequence cut in two by another writer.
fn broken_csi(bytes: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || b";:?<=>".contains(&bytes[j])) {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == 0x1b {
                let end = (j + 12).min(bytes.len());
                found.push(
                    String::from_utf8_lossy(&bytes[i..end])
                        .escape_debug()
                        .to_string(),
                );
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    found
}

fn drive(
    base_url: String,
    state: std::path::PathBuf,
    cwd: std::path::PathBuf,
    turns: usize,
    resize: bool,
) -> (Vec<u8>, Option<u32>) {
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 40,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pty");
    let mut command = portable_pty::CommandBuilder::new(env!("CARGO_BIN_EXE_kelir"));
    support::scrub_pty(&mut command);
    command.cwd(&cwd);
    command.env("KELIR_STATE_DIR", &state);
    command.env("KELIR_HOME", state.join("home"));
    command.env("KELIR_CONFIG_DIR", state.join("no-config"));
    command.env("KELIR_BASE_URL", &base_url);
    command.env("OPENROUTER_API_KEY", "test-openrouter-key");
    command.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(command).expect("spawn kelir");
    drop(pair.slave);
    let writer = Arc::new(Mutex::new(pair.master.take_writer().expect("writer")));
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let screen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = screen.clone();
    let responder = writer.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    sink.lock().unwrap().extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&chunk[..n]).into_owned();
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
    let has = |needle: &str, limit: Duration| -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if String::from_utf8_lossy(&screen.lock().unwrap()).contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    };
    let send = |bytes: &[u8]| {
        let mut out = writer.lock().unwrap();
        out.write_all(bytes).unwrap();
        out.flush().unwrap();
    };
    assert!(has("kelir", Duration::from_secs(20)), "the TUI never drew");
    std::thread::sleep(Duration::from_millis(300));
    for i in 0..turns {
        if resize && i == 1 {
            for (rows, cols) in [(20u16, 50u16), (30, 120), (12, 30)] {
                pair.master
                    .resize(portable_pty::PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .expect("resize");
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        send(format!("m{i}").as_bytes());
        std::thread::sleep(Duration::from_millis(30));
        send(b"\r");
        // The answer is the unique token ANS<i>; the done record follows it.
        assert!(
            has(&format!("ANS{i}"), Duration::from_secs(20)),
            "turn {i} never answered"
        );
    }
    send(b"/quit");
    std::thread::sleep(Duration::from_millis(100));
    send(b"\r");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut code = None;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            code = Some(status.exit_code());
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let out = screen.lock().unwrap().clone();
    (out, code)
}

/// R1: the byte stream a terminal receives never contains a CSI sequence cut by
/// an OSC 7501 record (or anything else).
#[tokio::test]
async fn r1_status_records_never_cut_a_paint_sequence() {
    let script = (0..TURNS)
        .map(|i| Reply::Sse(sse(&text_turn(&format!("ANS{i}"), 0))))
        .collect();
    let mock = Mock::start(script).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let state = temp.path().join("state");
    let url = mock.base_url();
    let (bytes, _) = tokio::task::spawn_blocking(move || drive(url, state, cwd, TURNS, false))
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.matches("\u{1b}]7501;state=done").count() >= TURNS,
        "fewer done records than turns"
    );
    let broken = broken_csi(&bytes);
    assert!(
        broken.is_empty(),
        "{} CSI sequence(s) cut by another write, e.g. {:?}",
        broken.len(),
        &broken[..broken.len().min(5)]
    );
}

/// R2: resizing the terminal while the TUI runs (narrower, wider, very short)
/// does not kill it: the next turn still answers and `/quit` exits 0.
#[tokio::test]
async fn r2_resize_mid_session_keeps_the_tui_alive() {
    let script = (0..3)
        .map(|i| Reply::Sse(sse(&text_turn(&format!("ANS{i}"), 0))))
        .collect();
    let mock = Mock::start(script).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let state = temp.path().join("state");
    let url = mock.base_url();
    let (bytes, code) = tokio::task::spawn_blocking(move || drive(url, state, cwd, 3, true))
        .await
        .unwrap();
    assert_eq!(code, Some(0), "kelir did not exit 0 after a resize");
    assert!(String::from_utf8_lossy(&bytes).contains("ANS2"));
}
