//! Full-screen TUI scroll, resize and restore driven in a real PTY.
#![allow(clippy::pedantic, clippy::nursery)]

use super::tui_output::plain;
use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::support::{Mock, Reply, sse, text_turn};
struct PtyRun {
    code: u64,
    screen: String,
}

// ------------------------------------------------- full-screen PTY regression

/// Reconstruct the visible grid from the bytes a ratatui frame emitted. Only
/// the subset ratatui uses is needed: cursor positioning, SGR, modes and
/// printable text. This lets a test assert what a human would see at a given
/// scroll position, not merely what bytes happened to pass through the pty.
fn screen_text(raw: &str, from: usize, rows: usize, cols: usize) -> String {
    let mut grid = vec![vec![' '; cols]; rows];
    let (mut row, mut col) = (0usize, 0usize);
    let start = raw.get(..from).map_or(0, |prefix| prefix.chars().count());
    let mut chars = raw.chars().skip(start).peekable();
    let mut params = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    params.clear();
                    while let Some(&next) = chars.peek() {
                        if next.is_ascii_digit() || matches!(next, ';' | '?' | '>' | '<') {
                            params.push(next);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    let Some(final_byte) = chars.next() else {
                        break;
                    };
                    match final_byte {
                        'H' | 'f' => {
                            let mut parts = params
                                .split(';')
                                .filter(|p| p.chars().all(|c| c.is_ascii_digit()));
                            let r = parts
                                .next()
                                .and_then(|p| p.parse::<usize>().ok())
                                .unwrap_or(1);
                            let c = parts
                                .next()
                                .and_then(|p| p.parse::<usize>().ok())
                                .unwrap_or(1);
                            row = r.saturating_sub(1).min(rows.saturating_sub(1));
                            col = c.saturating_sub(1).min(cols.saturating_sub(1));
                        }
                        'J' => {
                            for line in &mut grid {
                                line.fill(' ');
                            }
                        }
                        'K' => {
                            for cell in grid[row].iter_mut().skip(col) {
                                *cell = ' ';
                            }
                        }
                        'A' => row = row.saturating_sub(1),
                        'B' => row = (row + 1).min(rows.saturating_sub(1)),
                        'C' => col = (col + 1).min(cols.saturating_sub(1)),
                        'D' => col = col.saturating_sub(1),
                        _ => {}
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' => col = 0,
            '\n' => row = (row + 1).min(rows.saturating_sub(1)),
            ch if !ch.is_control() && row < rows => {
                grid[row][col] = ch;
                col += 1;
                if col >= cols {
                    col = 0;
                    row = (row + 1).min(rows.saturating_sub(1));
                }
            }
            _ => {}
        }
    }
    grid.iter()
        .map(|line| line.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Kill the child even if an assertion inside the driver panics.
struct KillPty(Box<dyn portable_pty::ChildKiller + Send>);
impl Drop for KillPty {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

#[tokio::test]
async fn full_screen_scroll_resize_and_restore_in_a_real_pty() {
    // Three turns whose answers are long enough to push the first turn off a
    // 30-row viewport: the scroll keys must bring it back from the app's own
    // transcript, not from the terminal's scrollback.
    let turns = [
        ("FIRST_PROMPT", "FIRST_ANSWER"),
        ("SECOND_PROMPT", "SECOND_ANSWER"),
        ("THIRD_PROMPT", "THIRD_ANSWER"),
    ];
    let replies: Vec<_> = turns
        .iter()
        .map(|(_, answer)| {
            let body = format!("{answer} {}", "word ".repeat(220));
            Reply::Sse(sse(&text_turn(&body, 0)))
        })
        .collect();
    let mock = Mock::start(replies).await;
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
        drive_full_screen_pty(cwd, config, state, base_url, &turns)
    })
    .await
    .expect("the pty driver");
    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    // Leaving the alternate screen and restoring the cursor is what makes the
    // user's own scrollback come back intact.
    assert!(
        run.screen.contains("\u{1b}[?1049l"),
        "alternate screen not restored: {:?}",
        run.screen
    );
    assert!(
        run.screen.contains("\u{1b}[?25h"),
        "cursor not restored: {:?}",
        run.screen
    );
}

/// Drive three turns through the real binary, press real scroll keys, resize
/// the pty, then quit. Each snapshot is the bytes painted *after* the key, so
/// it is the visible viewport at that position, not the whole byte stream.
fn drive_full_screen_pty(
    cwd: std::path::PathBuf,
    config: std::path::PathBuf,
    state: std::path::PathBuf,
    base_url: String,
    turns: &[(&str, &str)],
) -> PtyRun {
    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system
        .openpty(portable_pty::PtySize {
            rows: 30,
            cols: 90,
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
    command.env("KELIR_BASE_URL", &base_url);
    command.env("OPENROUTER_API_KEY", "test-openrouter-key");
    command.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(command).expect("spawn kelir");
    drop(pair.slave);
    let _killer = KillPty(child.clone_killer());
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
    let raw = || String::from_utf8_lossy(&screen.lock().unwrap()).into_owned();
    // A full-screen repaint can split one word across several cursor moves;
    // plain() inserts padding spaces for those. Compare only non-whitespace.
    let squash =
        |text: &str| -> String { plain(text).chars().filter(|c| !c.is_whitespace()).collect() };
    let wait_for = |needle: &str, limit: Duration| -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            let value = raw();
            if value.contains(needle) || squash(&value).contains(needle) {
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
    // Reconstruct the visible grid from the pty stream and wait until the
    // requested answer is actually on screen.
    let wait_screen = |from: usize, rows: usize, cols: usize, label: &str| -> String {
        let deadline = Instant::now() + crate::support::CEILING;
        loop {
            let view = screen_text(&raw(), from, rows, cols);
            if view.contains(label) {
                return view;
            }
            assert!(
                Instant::now() < deadline,
                "{label:?} never on screen: {view:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    assert!(
        wait_for("kelir", crate::support::CEILING),
        "no TUI: {}",
        raw()
    );
    let _ = wait_for("\u{1b}[?2004h", Duration::from_secs(3));
    for (prompt, answer) in turns {
        send(prompt.as_bytes());
        let suffix = &prompt[prompt.len().saturating_sub(6)..];
        assert!(
            wait_for(suffix, Duration::from_secs(10)),
            "no echo of {prompt}: {}",
            raw()
        );
        send(b"\r");
        assert!(
            wait_for(answer, crate::support::CEILING),
            "no {answer}: {}",
            raw()
        );
    }

    // One PageUp shows the previous page (turn two), not the tail. Poll for
    // both halves of that state: the repaint is asynchronous, so a single
    // snapshot can still be the old frame.
    send(b"\x1b[5~");
    let deadline = Instant::now() + crate::support::CEILING;
    let up_one = loop {
        let view = screen_text(&raw(), 0, 30, 90);
        if view.contains("SECOND_ANSWER")
            && view.contains("SECOND_PROMPT")
            && !view.contains("THIRD_ANSWER")
        {
            break view;
        }
        assert!(
            Instant::now() < deadline,
            "PageUp did not reach turn two: {view:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        !up_one.contains("THIRD_ANSWER"),
        "PageUp did not reach turn two: {up_one:?}"
    );

    // Home jumps to the oldest turn.
    send(b"\x1b[H");
    let up_two = wait_screen(0, 30, 90, "FIRST_ANSWER");
    assert!(
        up_two.contains("FIRST_PROMPT"),
        "first prompt not visible: {up_two:?}"
    );

    // PageDown returns toward the tail.
    send(b"\x1b[6~");
    let down_one = wait_screen(0, 30, 90, "SECOND_ANSWER");
    assert!(
        down_one.contains("SECOND_PROMPT"),
        "second prompt not visible: {down_one:?}"
    );
    send(b"\x1b[6~");
    let down_two = wait_screen(0, 30, 90, "THIRD_ANSWER");
    assert!(
        down_two.contains("THIRD_PROMPT"),
        "last prompt not visible: {down_two:?}"
    );

    // Resize the real pty: the app reflows the owned transcript and keeps the
    // reader's scroll position. Parse only the frames after the resize, at the
    // new geometry.
    let resize_at = screen.lock().unwrap().len();
    pair.master
        .resize(portable_pty::PtySize {
            rows: 24,
            cols: 60,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("resize the pty");
    let resized = wait_screen(resize_at, 24, 60, "THIRD_ANSWER");
    assert!(
        resized.contains("THIRD_PROMPT"),
        "tail lost on resize: {resized:?}"
    );
    send(b"\x1b[5~");
    let up_again = wait_screen(resize_at, 24, 60, "SECOND_ANSWER");
    assert!(
        !up_again.contains("THIRD_ANSWER"),
        "resize lost the scroll position: {up_again:?}"
    );
    // The prompt for that answer is one page further up at this width.
    send(b"\x1b[5~");
    let up_prompt = wait_screen(resize_at, 24, 60, "SECOND_PROMPT");
    assert!(
        !up_prompt.contains("THIRD_ANSWER"),
        "the second prompt must be reachable after resize: {up_prompt:?}"
    );
    send(b"\x1b[H");
    let top_again = wait_screen(resize_at, 24, 60, "FIRST_ANSWER");
    assert!(
        top_again.contains("FIRST_PROMPT"),
        "resize could not reach the first turn: {top_again:?}"
    );

    send(b"/quit");
    let _ = wait_for("/quit", Duration::from_secs(5));
    send(b"\r");
    let deadline = Instant::now() + crate::support::CEILING;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.exit_code() as u64,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.clone_killer().kill();
                panic!("kelir did not exit: {}", raw());
            }
        }
    };
    drop(reader_task);
    PtyRun {
        code,
        screen: raw(),
    }
}
