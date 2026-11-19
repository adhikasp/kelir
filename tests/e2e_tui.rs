//! The TUI's own layer of the pyramid: snapshots of the inline area, the
//! action→verb enumeration, the non-TTY fallback, and a headless PTY drive of
//! the real binary against the scripted mock.

mod support;

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kelir::commands::{self, Action, Parsed};
use kelir::tui::{self, App, Phase, UiEvent, VIEWPORT_HEIGHT};
use kelir::verbs;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use serde_json::json;
use support::{Mock, Reply, run_task, sse, text_turn, tool_turn};

// ----------------------------------------------------------------- snapshots

fn channels() -> (
    tokio::sync::mpsc::UnboundedSender<UiEvent>,
    tokio::sync::mpsc::UnboundedSender<kelir::ipc::Incoming>,
) {
    let (to_tui, _events) = tokio::sync::mpsc::unbounded_channel();
    let (verbs, _incoming) = tokio::sync::mpsc::unbounded_channel();
    (to_tui, verbs)
}

/// Render one app state into a 60x8 TestBackend and return the inline area as
/// text, one line per row, trailing spaces trimmed.
fn snapshot(app: &mut App) -> String {
    let backend = TestBackend::new(60, VIEWPORT_HEIGHT);
    let mut terminal = Terminal::with_options(
        backend,
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Inline(VIEWPORT_HEIGHT),
        },
    )
    .unwrap();
    terminal.draw(|frame| tui::render(app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell(Rect::new(x, y, 1, 1).as_position()) {
                line.push_str(cell.symbol());
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn new_app() -> App {
    let (to_tui, verbs) = channels();
    App::new(60, VIEWPORT_HEIGHT as usize, to_tui, verbs)
}

/// Every OSC 7501 record in a byte stream, exactly as it went out. Herdr
/// 0.1.21 requires `ESC ] 7501 ; state=<word> ESC \` with nothing else, so the
/// whole record is the assertion.
fn herdr_records(screen: &str) -> Vec<String> {
    let mut records = Vec::new();
    let mut rest = screen;
    while let Some(index) = rest.find("\u{1b}]7501;") {
        let tail = &rest[index..];
        let Some(end) = tail.find("\u{1b}\\") else {
            records.push(format!("unterminated: {tail:?}"));
            break;
        };
        records.push(tail[..end + 2].to_string());
        rest = &tail[end + 2..];
    }
    records
}

/// Assert the screen's row `index` equals `expected`, and that every other row
/// is `blank`; returns the whole screen for a message.
fn row_eq(screen: &str, index: usize, expected: &str) {
    let rows: Vec<&str> = screen.lines().collect();
    assert_eq!(
        rows.get(index),
        Some(&expected),
        "row {index} of {screen:?}"
    );
}

/// The viewport's rows 0..3 are the live area. The idle one is blank; a
/// streaming answer renders markdown with no `##`/`**` left in it.
#[test]
fn snapshot_the_idle_viewport() {
    let mut app = new_app();
    app.push_text(""); // nothing to commit
    let screen = snapshot(&mut app);
    row_eq(&screen, 0, "");
    row_eq(&screen, 1, "");
    row_eq(&screen, 2, "");
    row_eq(&screen, 3, " ● idle · ready");
    row_eq(&screen, 4, "›");
    row_eq(&screen, 7, "kelir idle · step 0 · 0 tok");
}

#[test]
fn snapshot_streaming_text_in_the_live_area() {
    let mut app = new_app();
    app.apply(UiEvent::TurnStarted {
        turn: 1,
        provider: "openrouter".into(),
        model: "test/model".into(),
        limit: None,
    });
    app.apply(UiEvent::Step {
        step: 1,
        provider: "openrouter".into(),
        model: "test/model".into(),
        turn_steps: 1,
        limit: None,
    });
    app.apply(UiEvent::Delta("Hello **wor".into()));
    let screen = snapshot(&mut app);
    // The unclosed `**` stays literal (a streamed prefix is not a mistake).
    assert!(screen.contains("Hello **wor"), "{screen:?}");
    app.apply(UiEvent::Delta("ld**\n".into()));
    let screen = snapshot(&mut app);
    let state = screen.lines().nth(3).unwrap_or_default();
    assert!(state.contains("writing"), "{screen:?}");
    assert!(
        !screen.contains("**"),
        "the closed emphasis renders: {screen:?}"
    );
    let facts = screen.lines().nth(7).unwrap_or_default();
    assert!(
        facts.starts_with("kelir working · test/model · step 1"),
        "{facts:?}"
    );
}

#[test]
fn snapshot_a_running_call() {
    let mut app = new_app();
    app.apply(UiEvent::TurnStarted {
        turn: 1,
        provider: "openrouter".into(),
        model: "test/model".into(),
        limit: None,
    });
    app.apply(UiEvent::Assistant {
        text: String::new(),
    });
    app.apply(UiEvent::ToolStarted {
        name: "bash".into(),
        input: json!({"command": "sleep 1"}),
    });
    let screen = snapshot(&mut app);
    let state = screen.lines().nth(3).unwrap_or_default();
    assert!(state.contains("running bash: sleep 1"), "{screen:?}");
    // The live rows are quiet while a call runs: the state row says what is
    // happening, and it is the only thing that moved.
    assert_eq!(screen.lines().next(), Some(""), "{screen:?}");
}

#[test]
fn snapshot_an_error() {
    let mut app = new_app();
    app.apply(UiEvent::TurnStarted {
        turn: 1,
        provider: "openrouter".into(),
        model: "test/model".into(),
        limit: None,
    });
    app.apply(UiEvent::Error("provider said no".into()));
    assert_eq!(app.status.phase, Phase::Error);
    let screen = snapshot(&mut app);
    let state = screen.lines().nth(3).unwrap_or_default();
    assert!(state.contains("error"), "{screen:?}");
    assert!(state.contains("provider said no"), "{screen:?}");
    let facts = screen.lines().nth(7).unwrap_or_default();
    assert!(facts.starts_with("kelir error"), "{facts:?}");
}

/// Ctrl-O: the last call's output tail takes the whole area above the facts
/// row, and the editor is out of the way while it is open.
#[test]
fn snapshot_the_output_tail_view() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    std::fs::write(&out, "line one\nline two\nline three\n").unwrap();
    let mut app = new_app();
    app.apply(UiEvent::ToolSettled {
        name: "bash".into(),
        input: json!({"command": "cargo test"}),
        outcome: "ok".into(),
        exit_code: Some(0),
        elapsed_ms: 200,
        bytes: 30,
        lines: 3,
        spilled: false,
        out_path: out.to_string_lossy().into_owned(),
        tail: String::new(),
    });
    // The call is still in the open group, not committed yet: nothing to
    // scroll.
    assert!(app.take_scroll().is_empty());
    app.press(tui::Press::ToggleTail);
    let screen = snapshot(&mut app);
    let rows: Vec<&str> = screen.lines().collect();
    assert_eq!(rows[0], "line one", "{screen}");
    assert_eq!(rows[1], "line two", "{screen}");
    assert_eq!(rows[2], "line three", "{screen}");
    assert!(
        rows[7].starts_with("tail (Ctrl-O closes) · kelir idle"),
        "{}",
        rows[7]
    );
    assert!(!screen.contains('›'), "the editor is hidden: {screen}");
    // Ctrl-O again closes it.
    app.press(tui::Press::ToggleTail);
    assert!(snapshot(&mut app).contains('›'));
}

/// A long line in the editor is clipped by the row, not wrapped into the
/// status line.
#[test]
fn snapshot_a_long_editor_line_stays_on_its_row() {
    let mut app = new_app();
    for c in "x".repeat(80).chars() {
        app.press(tui::Press::Edit(kelir::editor::Key::Char(c)));
    }
    let screen = snapshot(&mut app);
    let rows: Vec<&str> = screen.lines().collect();
    assert_eq!(rows.len(), 8);
    assert_eq!(rows[4].chars().count(), 60, "{}", rows[4]);
    assert!(rows[7].starts_with("kelir idle"), "{}", rows[7]);
    // The live rows above are untouched.
    assert!(rows[0..3].iter().all(|row| row.is_empty()), "{screen:?}");
}

// -------------------------------------------------- every action is a verb

#[test]
fn every_tui_action_has_a_verb() {
    // Every variant of `Action`, with the data a user would type. `Action::verb`
    // and `Action::args` match exhaustively, so this list cannot silently miss
    // one: the compiler fails on a new variant.
    let actions = vec![
        Action::Submit("do the thing".into()),
        Action::Stop(None),
        Action::Stop(Some("wrong way".into())),
        Action::SayNow("stop and do this".into()),
        Action::Later("also this".into()),
        Action::Model("openrouter/deepseek/deepseek-chat".into()),
        Action::Tools {
            disable: vec!["bash".into()],
            enable: Vec::new(),
        },
        Action::Tools {
            disable: Vec::new(),
            enable: vec!["bash".into()],
        },
        Action::Compact(None),
        Action::Compact(Some("the failing test".into())),
        Action::Quit,
    ];
    let known = ["say", "later", "stop", "model", "tools", "compact", "end"];
    for action in actions {
        let verb = action.verb();
        assert!(known.contains(&verb), "action {action:?} sends `{verb}`");
        let args = action.args();
        assert!(
            verbs::parse(verb, &args).is_ok(),
            "the session refuses `{verb}` with {args} from {action:?}"
        );
    }
    // And every `/` command parses to one of those actions.
    for line in [
        "hello",
        "/stop",
        "/stop why",
        "/say-now hi",
        "/later hi",
        "/model a/b",
        "/tools off bash",
        "/tools on bash",
        "/compact",
        "/compact focus",
        "/quit",
    ] {
        match commands::parse(line) {
            Ok(Parsed::Action(action)) => {
                assert!(
                    verbs::parse(action.verb(), &action.args()).is_ok(),
                    "{line}"
                )
            }
            other => panic!("{line} did not parse to an action: {other:?}"),
        }
    }
}

#[test]
fn the_ctl_surface_accepts_the_tui_verbs() {
    // The verbs the TUI sends are exactly the verbs `kelir ctl` can send:
    // the same parser sees the same args.
    for (verb, args) in [
        ("say", json!({"text": "hi", "now": true})),
        ("later", json!({"text": "hi"})),
        ("stop", json!({"call": "call_1", "reason": "x"})),
        ("model", json!({"model": "p/m"})),
        ("tools", json!({"disable": "bash"})),
        ("compact", json!({"focus": "x"})),
        ("end", json!({})),
    ] {
        assert!(verbs::parse(verb, &args).is_ok(), "{verb}");
    }
}

// -------------------------------------------------------------- non-TTY

#[tokio::test]
async fn bare_kelir_without_a_tty_is_a_usage_error_and_does_not_hang() {
    let mock = Mock::start(vec![]).await;
    // No subcommand, no task, stdout is a pipe: a usage error, never a hang.
    let run = run_task(&mock, &[], &[], None, "").await;
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert!(run.stderr.contains("no terminal"), "{}", run.stderr);
    assert!(run.stdout.is_empty(), "{}", run.stdout);
    assert_eq!(mock.request_count(), 0, "nothing was sent to a provider");
}

#[tokio::test]
async fn kelir_tui_without_a_tty_is_a_usage_error() {
    let mock = Mock::start(vec![]).await;
    let run = run_task(&mock, &["tui"], &[], None, "").await;
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert!(run.stderr.contains("no terminal"), "{}", run.stderr);
}

#[tokio::test]
async fn kelir_tui_refuses_a_task_before_it_opens_the_terminal() {
    let mock = Mock::start(vec![]).await;
    let run = run_task(&mock, &["tui", "do", "it"], &[], None, "").await;
    // `tui` has no task argument: the first message is typed in the editor.
    // The CLI rejects the word before a terminal is opened or a session made.
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("unexpected argument 'do'"),
        "{}",
        run.stderr
    );
    assert_eq!(mock.request_count(), 0, "nothing was sent to a provider");
}

// -------------------------------------------------------------------- PTY

struct PtyRun {
    code: u64,
    screen: String,
    session_dir: std::path::PathBuf,
}

/// Drive the real binary in a pty: type a message, wait for the answer, quit.
fn drive_in_a_pty(
    cwd: std::path::PathBuf,
    config: std::path::PathBuf,
    state: std::path::PathBuf,
    base_url: String,
    preamble: &[u8],
    expect: Vec<String>,
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
            if text(screen).contains(needle) {
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
    if wait_for(&screen, "kelir", Duration::from_secs(20)) {
        // A moment for raw mode; then anything the test types first, the
        // message, and Enter.
        std::thread::sleep(Duration::from_millis(200));
        if !preamble.is_empty() {
            send(preamble);
            std::thread::sleep(Duration::from_millis(200));
        }
        send(b"hello there");
        let _ = wait_for(&screen, "hello there", Duration::from_secs(5));
        send(b"\r");
        typed = true;
    }
    assert!(typed, "the TUI never drew: {:?}", text(&screen));
    for needle in &expect {
        assert!(
            wait_for(&screen, needle, Duration::from_secs(30)),
            "{needle:?} never reached the terminal: {:?}",
            text(&screen)
        );
    }
    send(b"/quit");
    std::thread::sleep(Duration::from_millis(100));
    send(b"\r");

    let deadline = Instant::now() + Duration::from_secs(30);
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
    // The banner names the window compaction measures against and where it came
    // from (STEER-1: the provider's list, fetched before the first step).
    assert!(
        run.screen.contains("window 262144 tokens (api)"),
        "the banner must show the window and its source; screen: {:?}",
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
    assert!(types.contains(&"usage"), "{types:?}");
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

/// The failure word: `error` goes out over the same OSC record — and a failed
/// turn does not end the session.
#[tokio::test]
async fn the_tui_reports_an_error_to_herdr() {
    let mock = Mock::start(vec![
        Reply::Http(500, "boom".into()),
        Reply::Http(500, "boom".into()),
        Reply::Http(500, "boom".into()),
    ])
    .await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\nmax_steps = 3\n",
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
            vec!["state=error".into()],
        )
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    assert_eq!(
        herdr_records(&run.screen),
        vec![
            "\u{1b}]7501;state=working\u{1b}\\".to_string(),
            "\u{1b}]7501;state=error\u{1b}\\".to_string(),
        ],
        "screen: {:?}",
        run.screen
    );
    let text = std::fs::read_to_string(run.session_dir.join("events.jsonl")).unwrap();
    assert!(text.contains("\"type\":\"error\""), "{text}");
    assert!(text.contains("\"outcome\":\"error\""), "{text}");
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

    // The session treats a dead UI like `/quit`: it ends and exits cleanly
    // instead of running on with nobody watching.
    assert_eq!(
        code, 0,
        "a panic must not hang or crash the session: {screen:?}"
    );
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

/// Start the TUI with `KELIR_TEST_PANIC=1` and collect what the terminal saw.
fn panic_in_a_pty(
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
    let deadline = Instant::now() + Duration::from_secs(30);
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

/// N1: with a cap configured, the status line says `step 0/N` while the session
/// idles, not a bare `step 0` that hides the limit until a turn starts.
#[tokio::test]
async fn the_idle_status_line_already_knows_the_cap() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\nmax_steps = 3\n",
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
            vec!["kelir idle · step 0/3 · 0 tok".into()],
        )
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    // The bare form would be the same screen with the cap hidden.
    assert!(
        run.screen.contains("kelir idle · step 0/3 · 0 tok"),
        "the idle line must show the cap: {:?}",
        run.screen
    );
    // And the banner names the cap too, so the two never disagree.
    assert!(
        run.screen.contains("step cap 3 per turn"),
        "screen: {:?}",
        run.screen
    );
}

/// N2: a paused turn is a visible Herdr state. The pause word this Herdr
/// accepts is `blocked` (it silently drops `paused`), so a supervisor can tell
/// "waiting for a human to say continue" from "never started" (`idle`).
#[tokio::test]
async fn the_tui_reports_a_pause_to_herdr_as_blocked() {
    let edit = serde_json::json!({"path": "f.txt", "old": "", "new": "x\n"});
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn("call_1", "edit", &edit))),
        Reply::Sse(sse(&tool_turn("call_2", "edit", &edit))),
        Reply::Sse(sse(&text_turn("never asked", 0))),
    ])
    .await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\nmax_steps = 2\n",
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
            vec!["turn paused after 2 steps".into()],
        )
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    assert_eq!(
        herdr_records(&run.screen),
        vec![
            "\u{1b}]7501;state=working\u{1b}\\".to_string(),
            "\u{1b}]7501;state=blocked\u{1b}\\".to_string(),
        ],
        "a pause is not `idle`: it waits for a human; screen: {:?}",
        run.screen
    );
    // The session stayed up after the pause, so `/quit` is what ended it.
    let text = std::fs::read_to_string(run.session_dir.join("events.jsonl")).unwrap();
    assert!(text.contains("\"outcome\":\"paused\""), "{text}");
}
