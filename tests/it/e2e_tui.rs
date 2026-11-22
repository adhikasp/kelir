//! The full-screen TUI: snapshots of the owned viewport and status rows, the
//! action→verb enumeration, the non-TTY fallback, and PTY drives of the real
//! binary against the scripted mock.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::support::{ApplyEv as _, Ev, Mock, Reply, run_task, sse, text_turn, tool_turn};
use kelir::commands::{self, Action, Parsed};
use kelir::tui::{self, App, Phase};

/// Full-screen height the snapshot tests render at.
pub(crate) const SCREEN: u16 = 20;
use kelir::verbs;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use serde_json::json;

// ----------------------------------------------------------------- snapshots

fn channels() -> (
    tokio::sync::mpsc::UnboundedSender<kelir::tui::View>,
    tokio::sync::mpsc::UnboundedSender<kelir::ipc::Incoming>,
) {
    let (to_tui, _events) = tokio::sync::mpsc::unbounded_channel();
    let (verbs, _incoming) = tokio::sync::mpsc::unbounded_channel();
    (to_tui, verbs)
}

/// Render one app state into a fixed 60x20 full-screen TestBackend and return
/// the text, one line per row, trailing spaces trimmed. The full-screen TUI
/// always renders the entire terminal, unlike the old inline viewport.
fn snapshot(app: &mut App) -> String {
    render_into(app, 60, SCREEN)
}

/// Render `app` into a `width`x`SCREEN` full-screen TestBackend.
fn render_at(app: &mut App, width: u16) -> String {
    render_into(app, width, SCREEN)
}

fn render_into(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
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

/// A styled line's text, tones dropped.
fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn new_app() -> App {
    let (to_tui, verbs) = channels();
    App::new(60, SCREEN as usize, to_tui, verbs, String::new())
}

/// The text of a pty stream, with every CSI/OSC escape removed: the assertion
/// is about what a reader sees, not about the bytes that paint it.
fn plain(screen: &str) -> String {
    let mut out = String::new();
    let mut chars = screen.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                // CSI: drop the `[`, then parameters until a final byte in
                // 0x40..=0x7e (the `[` itself is in that range, so it has to go
                // first or the scan stops on it).
                chars.next();
                let mut final_byte = '\0';
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        final_byte = c;
                        break;
                    }
                }
                // A differential repaint writes runs at absolute cursor
                // positions; consecutive runs are separate screen words.
                if matches!(final_byte, 'H' | 'f' | 'A' | 'B') {
                    out.push(' ');
                }
            }
            Some(']') => {
                // OSC: until BEL or ST.
                let mut last = '\0';
                for c in chars.by_ref() {
                    if c == '\u{7}' || (last == '\u{1b}' && c == '\\') {
                        break;
                    }
                    last = c;
                }
            }
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
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

/// The state row carries an activity word and a hint; find it by the glyphs
/// used for idle/working/paused/error, since its screen position depends on
/// the number of editor rows.
fn state_row(screen: &str) -> &str {
    screen
        .lines()
        .find(|r| {
            [
                "idle", "writing", "thinking", "running", "paused", "error", "done",
            ]
            .iter()
            .any(|word| r.contains(word))
        })
        .unwrap_or("")
}

/// The facts row is the last non-empty line of a full-screen snapshot.
fn facts_row(screen: &str) -> &str {
    screen.lines().last().unwrap_or("")
}

/// The full-screen view keeps the input and facts at the bottom, with the
/// transcript above; a streaming answer renders markdown (no `##`/`**`).
#[test]
fn snapshot_the_idle_viewport() {
    let mut app = new_app();
    app.push_text(""); // nothing to commit
    let screen = snapshot(&mut app);
    let rows: Vec<&str> = screen.lines().collect();
    assert_eq!(rows.len(), SCREEN as usize, "a full screen: {screen:?}");
    assert!(
        rows.iter().any(|r| r.starts_with(" ● idle · ready")),
        "state row: {screen:?}"
    );
    assert!(rows.contains(&"›"));
    assert!(rows.last().unwrap().starts_with("0 tok in"));
}

/// A settled answer lives in the owned scrollback and is visible above the
/// input. The view is deterministic across paints, and shows no markdown
/// markers or blank gap in the transcript.
#[test]
fn a_settled_long_answer_leaves_no_blank_gap_above_the_input() {
    let mut app = new_app();
    let answer = "## Notes\n\n".to_string()
        + &"A long paragraph that wraps across several editor columns and scrolls \
            through the terminal as it is committed, with enough words to fill many rows. "
            .repeat(12);
    app.push(Ev::Delta(answer.clone()));
    app.push(Ev::Assistant {
        text: answer.clone(),
    });
    app.push(Ev::TurnEnded {
        outcome: "done".into(),
        final_text: answer,
    });
    assert!(
        app.transcript_text().contains("Notes"),
        "the answer is an owned entry"
    );
    let screen = snapshot(&mut app);
    assert_eq!(screen.lines().count(), SCREEN as usize);
    // The viewport follows the bottom of a long answer, so the tail is visible
    // (the whole answer is in `transcript_text`, checked above).
    assert!(
        screen.contains("committed, with enough words"),
        "answer in scrollback: {screen:?}"
    );
    assert!(!screen.contains("##"), "no raw markdown: {screen:?}");
    let screen2 = snapshot(&mut app);
    assert_eq!(screen, screen2, "the view is stable across paints");
}

#[test]
fn snapshot_streaming_text_in_the_live_area() {
    let mut app = new_app();
    app.push(Ev::TurnStarted {
        turn: 1,
        provider: "openrouter".into(),
        model: "test/model".into(),
        limit: None,
    });
    app.push(Ev::Step {
        step: 1,
        provider: "openrouter".into(),
        model: "test/model".into(),
        turn_steps: 1,
        limit: None,
    });
    app.push(Ev::Delta("Hello **wor".into()));
    let screen = snapshot(&mut app);
    // The unclosed `**` stays literal (a streamed prefix is not a mistake).
    assert!(screen.contains("Hello **wor"), "{screen:?}");
    app.push(Ev::Delta("ld**\n\n".into()));
    let screen = snapshot(&mut app);
    let state = state_row(&screen);
    assert!(state.contains("writing"), "{screen:?}");
    // A closed block stays in the owned transcript and renders without markers.
    let screen = snapshot(&mut app);
    assert!(
        screen.contains("Hello world"),
        "the closed emphasis renders: {screen:?}"
    );
    assert!(!screen.contains("**"), "no literal markers: {screen:?}");
    let facts = facts_row(&screen);
    assert!(facts.starts_with("test/model"), "{facts:?}");
}

#[test]
fn snapshot_a_running_call() {
    let mut app = new_app();
    app.push(Ev::TurnStarted {
        turn: 1,
        provider: "openrouter".into(),
        model: "test/model".into(),
        limit: None,
    });
    app.push(Ev::Assistant {
        text: String::new(),
    });
    app.push(Ev::ToolStarted {
        name: "bash".into(),
        input: json!({"command": "sleep 1"}),
    });
    let screen = snapshot(&mut app);
    let state = state_row(&screen);
    assert!(state.contains("running bash: sleep 1"), "{screen:?}");
    // No live rows while a call runs: the state row is the first row, right
    // under the last committed scrollback line.
    assert!(state.contains("running"), "{screen:?}");
}

#[test]
fn snapshot_an_error() {
    let mut app = new_app();
    app.push(Ev::TurnStarted {
        turn: 1,
        provider: "openrouter".into(),
        model: "test/model".into(),
        limit: None,
    });
    app.push(Ev::Error("provider said no".into()));
    assert_eq!(app.status.phase, Phase::Error);
    let screen = snapshot(&mut app);
    let state = state_row(&screen);
    assert!(state.contains("error"), "{screen:?}");
    assert!(state.contains("provider said no"), "{screen:?}");
    let facts = facts_row(&screen);
    assert_eq!(facts.trim(), "test/model · 0 tok in", "{facts:?}");
}

/// Ctrl-O: the last call's output tail takes the whole area above the facts
/// row, and the editor is out of the way while it is open.
#[test]
fn snapshot_the_output_tail_view() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    std::fs::write(&out, "line one\nline two\nline three\n").unwrap();
    let mut app = new_app();
    app.push(Ev::ToolSettled {
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
    // The call's line commits as it settles; the tail overlay is independent.
    app.take_scroll();
    app.press(tui::Press::ToggleTail);
    let screen = snapshot(&mut app);
    let rows: Vec<&str> = screen.lines().collect();
    // The header names the call (design note §4), then the tail itself.
    assert!(rows[0].contains("$ cargo test"), "{screen}");
    assert_eq!(rows[1], "line one", "{screen}");
    assert_eq!(rows[2], "line two", "{screen}");
    assert_eq!(rows[3], "line three", "{screen}");
    // The facts row is last and says how to close the overlay.
    let facts = rows.last().unwrap();
    assert!(facts.starts_with("tail (Ctrl-O closes)"), "{screen}");
    assert!(
        facts.starts_with("tail (Ctrl-O closes) · 0 tok in"),
        "{facts}"
    );
    assert!(!screen.contains('›'), "the editor is hidden: {screen}");
    // Ctrl-O again closes it.
    app.press(tui::Press::ToggleTail);
    assert!(snapshot(&mut app).contains('›'));
}

/// A long line in the editor wraps across its rows: text is never clipped or
/// spilled into the facts row, and the prompt appears only once.
#[test]
fn snapshot_a_long_editor_line_wraps_across_its_rows() {
    let mut app = new_app();
    for c in "x".repeat(80).chars() {
        app.press(tui::Press::Edit(kelir::editor::Key::Char(c)));
    }
    let screen = snapshot(&mut app);
    let rows: Vec<&str> = screen.lines().collect();
    assert_eq!(rows.len(), SCREEN as usize, "{screen:?}");
    // The editor sits above the facts row at the bottom of the screen. It has
    // two rows: the prompt on the first, a continuation indent on the second.
    let prompt = rows.iter().position(|r| r.starts_with("› ")).unwrap();
    assert!(
        rows[prompt + 1].starts_with("  "),
        "row: {:?}",
        rows[prompt + 1]
    );
    assert_eq!(rows[prompt].chars().count(), 60, "{}", rows[prompt]);
    assert_eq!(
        rows[prompt + 1].trim_end().chars().count(),
        24,
        "{}",
        rows[prompt + 1]
    );
    assert_eq!(rows[prompt + 2], "0 tok in", "{}", rows[prompt + 2]);
    assert!(rows.iter().any(|r| r.contains("idle")), "{screen:?}");
}

/// 80, 60 and 40 columns: the state row, the facts row and a tool line all fit
/// the width and never spill onto a second row.
#[test]
fn snapshot_narrow_panes_keep_every_row_inside_its_width() {
    for width in [80u16, 60, 40, 30, 20] {
        let (to_tui, _events) = tokio::sync::mpsc::unbounded_channel();
        let (verbs, _incoming) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            width as usize,
            SCREEN as usize,
            to_tui,
            verbs,
            String::new(),
        );
        app.push(Ev::TurnStarted {
            turn: 1,
            provider: "openrouter".into(),
            model: "deepseek/deepseek-v4.1-flash-1017".into(),
            limit: None,
        });
        app.push(Ev::ToolStarted {
            name: "bash".into(),
            input: json!({"command": "cargo test --all-targets -- --nocapture"}),
        });
        let screen = render_at(&mut app, width);
        for (index, row) in screen.lines().enumerate() {
            assert!(
                kelir::scrollback::display_width(row) <= width as usize,
                "width {width}, row {index}: {row:?}"
            );
        }
        let state = state_row(&screen);
        assert!(state.contains("running"), "width {width}: {state:?}");
    }
}

/// The state row and the prompt tell idle from working without colour: the
/// glyph and the word change, and the prompt dims.
#[test]
fn snapshot_the_idle_and_working_rows_are_visibly_different() {
    let mut app = new_app();
    let idle = render_at(&mut app, 60);
    app.push(Ev::TurnStarted {
        turn: 1,
        provider: "p".into(),
        model: "m".into(),
        limit: None,
    });
    let working = render_at(&mut app, 60);
    let idle_state = state_row(&idle).to_string();
    let working_state = state_row(&working).to_string();
    assert!(idle_state.contains("idle"), "{idle_state:?}");
    assert!(working_state.contains("thinking"), "{working_state:?}");
    assert_ne!(
        idle_state.trim_start_matches(' ').chars().next(),
        working_state.trim_start_matches(' ').chars().next(),
        "the glyph differs too: {idle_state:?} vs {working_state:?}"
    );
}

/// Each call commits its own line the moment it settles; there is no fold.
#[test]
fn every_tool_call_commits_its_line_without_a_fold() {
    let mut app = new_app();
    for command in ["git --version", "python --version", "cargo fmt"] {
        app.push(Ev::ToolSettled {
            name: "bash".into(),
            input: json!({"command": command}),
            outcome: "ok".into(),
            exit_code: Some(0),
            elapsed_ms: 300,
            bytes: 29,
            lines: 1,
            spilled: false,
            out_path: String::new(),
            tail: String::new(),
        });
    }
    let committed: Vec<String> = app.take_scroll().iter().map(line_text).collect();
    assert_eq!(committed.len(), 3, "{committed:?}");
    assert!(committed[0].contains("git --version"), "{committed:?}");
    assert!(committed[1].contains("python --version"), "{committed:?}");
    assert!(committed[2].contains("cargo fmt"), "{committed:?}");
    assert!(
        !committed.iter().any(|row| row.contains("calls")),
        "{committed:?}"
    );
    assert!(
        !committed.iter().any(|row| row.contains(" ok")),
        "{committed:?}"
    );
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
fn drive_idle_in_a_pty(
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
fn wait_for_session_log(state: &std::path::Path, needle: &str, limit: Duration) -> bool {
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
struct PtyInput<'a> {
    submit: &'a [u8],
    message_echo: &'a str,
}

#[expect(
    clippy::too_many_arguments,
    reason = "test-only PTY driver; every knob is explicit"
)]
fn drive_pty_in_idle_or_turn(
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

#[derive(Debug)]
struct IdleRun {
    code: u64,
    bytes_while_idle: usize,
}

/// Drive one turn, wait for it to settle, then count every byte the TUI writes
/// while it sits idle for two seconds.
fn drive_and_count_idle_bytes(
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
#[tokio::test]
async fn a_pasted_image_path_becomes_a_pointer_in_the_prompt() {
    let answer = "described";
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn(answer, 0)))]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let image = cwd.join("shot.png");
    std::fs::write(&image, tiny_png()).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();
    let state = temp.path().join("state");
    let base_url = mock.base_url();
    // A quoted path, as a Windows drag and drop (or a clipboard paste of a
    // file) puts on the input line. It is typed as keystrokes here: crossterm
    // 0.29 only decodes `ESC[200~` on Unix, so on Windows a bracketed paste
    // arrives as characters. The submit-time path scan is what makes both
    // platforms a pointer (`App::absorb_image_paths`).
    let paste = format!("\"{}\"", image.display());
    let run = tokio::task::spawn_blocking(move || {
        drive_in_a_pty(cwd, config, state, base_url, paste.as_bytes(), vec![])
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    // The saved file is in the session's attachments, unmodified.
    let log = std::fs::read_to_string(run.session_dir.join("events.jsonl")).unwrap();
    let attachments = run.session_dir.join("attachments");
    if !attachments.is_dir() {
        panic!(
            "no attachments dir.
log: {log}
screen: {:?}",
            run.screen
        );
    }
    let files: Vec<_> = std::fs::read_dir(&attachments)
        .expect("an attachments dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(std::fs::read(&files[0]).unwrap(), tiny_png());
    // The `user` event carries the pointer, not the path and not the bytes.
    let log = std::fs::read_to_string(run.session_dir.join("events.jsonl")).unwrap();
    let user = log
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|event| event["type"] == "user")
        .expect("a user event");
    let text = user["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("[image: attachments/1-shot.png"),
        "the prompt carries the pointer: {text:?}"
    );
    assert!(
        text.contains("hello there"),
        "the typed text is still there: {text:?}"
    );
    assert!(!text.contains("shot.png\""), "no raw path: {text:?}");
    assert!(
        !text.contains("iVBOR"),
        "no image bytes in the prompt: {text:?}"
    );
}

/// The Windows paste path end to end, in a pty: the kernel delivers the pasted
/// path as a stream of key events there too, so the chip has to appear before
/// Enter — the review's finding was that on Windows it appeared only at submit,
/// as raw path text (`S8`).
#[tokio::test]
async fn a_typed_image_path_is_a_chip_in_the_editor_before_it_is_sent() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("PARROT", 0)))]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let image = cwd.join("shot.png");
    std::fs::write(&image, tiny_png()).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();
    let state = temp.path().join("state");
    let base_url = mock.base_url();
    let typed = image.display().to_string();
    let run = tokio::task::spawn_blocking(move || {
        drive_in_a_pty(cwd, config, state, base_url, typed.as_bytes(), vec![])
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    // The screen showed the chip (the path itself was replaced), which is what
    // a Backspace then removes whole; the submit-time scan is no longer the only
    // thing that makes the pointer.
    let plain = plain(&run.screen);
    assert!(plain.contains("[image 1]"), "the chip was drawn: {plain:?}");
    let user = std::fs::read_to_string(run.session_dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|event| event["type"] == "user")
        .expect("a user event");
    let text = user["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("[image: attachments/1-shot.png"),
        "the prompt carries the pointer once: {text:?}"
    );
    assert!(!text.contains("iVBOR"), "no bytes: {text:?}");
}

/// A 4x3 PNG: bytes a real decoder would accept as a header.
fn tiny_png() -> Vec<u8> {
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
#[tokio::test]
async fn the_tui_reports_an_error_to_herdr() {
    let mock = Mock::start(vec![
        Reply::Http(400, "unknown model".into()),
        Reply::Http(400, "unknown model".into()),
    ])
    .await;
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

/// A provider that stays down past the waiting budget pauses the turn: the
/// status line says "paused", Herdr gets `blocked` (the one word this Herdr
/// shows for "waiting for a human", the same rule as a step-cap pause), and
/// the log's `reason` says `provider` with a message saying how to go on.
#[tokio::test]
async fn the_tui_reports_a_paused_turn_to_herdr() {
    let mock = Mock::start(vec![
        Reply::Http(503, "busy".into()),
        Reply::Http(503, "busy".into()),
        Reply::Http(503, "busy".into()),
        Reply::Http(503, "busy".into()),
    ])
    .await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n[retry]\nbase_ms = 50\ncap_ms = 100\nmax_attempts = 2\nmax_wait = 5\n",
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
            vec!["provider unavailable".into(), "say continue".into()],
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
        "a provider pause waits for a human too: screen {:?}",
        run.screen
    );
    let text = std::fs::read_to_string(run.session_dir.join("events.jsonl")).unwrap();
    assert!(text.contains("\"outcome\":\"paused\""), "{text}");
    assert!(text.contains("\"reason\":\"provider\""), "{text}");
    assert!(!text.contains("\"outcome\":\"error\""), "{text}");
}

/// STEER-5's retry countdown reaches tui2's rows: a 429 shows the wait line
/// and its countdown in the facts row and the `rate limited` state in the
/// state row, both with the attempt number, and the turn then goes through.
#[tokio::test]
async fn the_tui_shows_the_retry_countdown_and_recovers() {
    let mock = Mock::start(vec![
        Reply::Http(429, "slow down".into()),
        Reply::Sse(sse(&text_turn("RECOVERED", 0))),
    ])
    .await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n[retry]\nbase_ms = 3000\ncap_ms = 3000\nmax_attempts = 3\n",
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
            vec![
                // The facts row carries the whole wait line, the state row the
                // countdown and the attempt. Both name this pane's key: `stop`
                // typed here is a `say` the model receives (`R5`).
                "rate limited (429): retrying in".into(),
                "esc stops".into(),
                // The facts row's own countdown, read from the wall clock.
                "s left".into(),
                "attempt 1".into(),
                "RECOVERED".into(),
            ],
        )
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    // The wait is in the log as an `error` event, and the turn that followed it
    // ended `done`: a retried 429 is not a failure.
    let text = std::fs::read_to_string(run.session_dir.join("events.jsonl")).unwrap();
    assert!(text.contains("\"status\":429"), "{text}");
    assert!(text.contains("\"outcome\":\"done\""), "{text}");
    assert!(!text.contains("\"outcome\":\"error\""), "{text}");
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
async fn the_idle_status_line_stays_calm_even_with_a_cap() {
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
        drive_in_a_pty(cwd, config, state, base_url, b"", vec!["0 tok in".into()])
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    assert!(
        run.screen.contains("0 tok in"),
        "the idle line is phase, model and tokens: {:?}",
        run.screen
    );
    // And the banner names the cap too, so the two never disagree.
    let compact: String = plain(&run.screen)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(
        compact.contains("stepcap3perturn"),
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

/// F-window: the banner stays silent about the window the provider answered,
/// and says one dim line when the start could not read the list — because that
/// fallback silently sets the compaction threshold (STEER-1 review N4). A list
/// endpoint that answers 503 is the offline case, made deterministic here.
#[tokio::test]
async fn the_tui_banner_names_a_window_only_when_it_fell_back() {
    let mock = Mock::start_with_models(
        vec![Reply::Sse(sse(&text_turn("PARROT", 0)))],
        crate::support::ModelsAnswer::http(503, "later"),
    )
    .await;
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
            vec![
                "window 128000 tokens (default)".into(),
                "the model list: http 503".into(),
                "PARROT".into(),
            ],
        )
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    // One line, not two: the same fallback is never announced twice.
    let screen = plain(&run.screen);
    assert_eq!(
        screen.matches("window 128000 tokens").count(),
        1,
        "one fallback line: {:?}",
        run.screen
    );
}

/// F-window: the *normal* window is not in the banner. A start whose list
/// answers must not print a window line at all, at any width.
#[tokio::test]
async fn the_tui_banner_is_silent_about_the_providers_own_window() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
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
        drive_in_a_pty(cwd, config, state, base_url, b"", vec!["kelir".into()])
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    assert!(
        !run.screen.contains("tokens ("),
        "the banner carries no window line when the list answered: {:?}",
        run.screen
    );
}

/// A metadata fetch that lands after the start's grace must still be applied to
/// an *idle* session: an idle `--keep`/TUI session used to poll the fetch once
/// and then block on the verb channel, so a slow answer was never applied until
/// a turn began — `state`, and the banner's note about a fallback, stayed on
/// "fetching" for as long as nobody typed. The list here answers 503 after 1.5 s
/// (well past the 250 ms start grace), so the only way its line reaches the
/// screen is the idle loop applying it by itself.
#[tokio::test]
async fn a_slow_list_is_applied_to_an_idle_tui_session() {
    let mock = Mock::start_with_models(
        Vec::new(),
        crate::support::ModelsAnswer::http(503, "later").slow(Duration::from_millis(1500)),
    )
    .await;
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
        // Nothing is typed but `/quit`: the session only idles. The line's form
        // is not the point (the banner's if the answer beat the grace, the `»`
        // one if it did not); that it appears at all while idle is.
        drive_idle_in_a_pty(
            cwd,
            config,
            state,
            base_url,
            vec![
                "window 128000 tokens (default)".into(),
                "the model list: http 503".into(),
            ],
        )
    })
    .await
    .expect("the pty driver");

    let screen = &run.screen;
    let plain_screen = plain(screen);
    assert!(
        plain_screen.matches("window 128000 tokens").count() == 1,
        "the late answer is applied once, while idle: {screen:?}"
    );
}

/// F-window: the brief's offline case, without a mock at all — a base URL on a
/// dead port. The list cannot answer, the window falls back to the default, and
/// the banner says so with the reason instead of staying quiet about a number
/// that decides when the session compacts (STEER-1 review N4).
#[tokio::test]
async fn an_offline_start_says_the_window_it_fell_back_to() {
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
    // Port 9 is the discard port: nothing listens on it, so a connect is
    // refused and the fetch fails. An idle session applies the answer as soon
    // as it lands (it does not wait for a turn), so this test needs no mock and
    // no message: the fallback line appears while the session sits at the
    // prompt. Which of the two forms it takes depends on how fast the refusal
    // is, and both are correct: refused inside the start's 250 ms grace and the
    // *banner* carries the line; slower (Windows takes about 2 s) and the banner
    // could only say "fetching", so `apply_list` writes the `»` line when the
    // answer lands. The assertions below are the invariant, not the form: the
    // number, its source and the reason, once.
    let base_url = "http://127.0.0.1:9".to_string();
    let run = tokio::task::spawn_blocking(move || {
        drive_idle_in_a_pty(
            cwd,
            config,
            state,
            base_url,
            vec![
                "window 128000 tokens (default)".into(),
                "the model list: offline".into(),
            ],
        )
    })
    .await
    .expect("the pty driver");

    let screen = &run.screen;
    let plain_screen = plain(screen);
    assert!(
        plain_screen.matches("window 128000 tokens").count() == 1,
        "one fallback line, not two: {screen:?}"
    );
    assert!(
        screen.contains("the model list: offline"),
        "the reason reaches the screen, not just the log: {screen:?}"
    );
}

/// F-window: a `/model` swap announces the window it moved to, once, on the
/// line above the editor — the banner has no window line to update, so without
/// this a person would never learn the compaction threshold changed.
#[tokio::test]
async fn a_model_swap_announces_the_new_window() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    // A configured window for the *start* model, so the swap has a number to
    // move away from; the new model comes from the mock's list (200000).
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n[model_windows]\n\"test/model\" = 64000\n",
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
            b"/model anthropic/claude-sonnet-4.5\r",
            vec![
                "» model anthropic/claude-sonnet-4.5".into(),
                "» window 200000 tokens (".into(),
            ],
        )
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    let screen = &run.screen;
    // The source is `cache` or `api` depending on whether the swap's fetch had
    // landed; the point is that the number is said at all, once.
    let plain_screen = plain(screen);
    assert!(
        plain_screen.matches("» window 200000 tokens").count() == 1,
        "the swap's window is announced, once: {screen:?}"
    );
}

/// Shift-Enter arrives as a CSI-u `ESC[13;2u` sequence (Ghostty, or Herdr
/// forwarding one after the TUI negotiated kitty keyboard mode). It must add
/// a newline to the prompt, not submit the message early: the logged user
/// text still contains the whole two-line input.
#[cfg(not(windows))] // Windows crossterm reads Win32 keys, not CSI-u byte streams.
#[tokio::test]
async fn a_csi_u_shift_enter_adds_a_newline_instead_of_submitting() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("OK", 0)))]).await;
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
    // "first line" + Shift-Enter + "second line", then a plain Enter to
    // submit the whole thing as one message.
    let message = b"first line\x1b[13;2usecond line";
    let run = tokio::task::spawn_blocking(move || {
        drive_pty_in_idle_or_turn(
            cwd,
            config,
            state,
            base_url,
            Some(message),
            b"",
            vec!["OK".into()],
            PtyInput {
                submit: b"\r",
                message_echo: "first line",
            },
        )
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    // The TUI negotiated kitty keyboard mode on startup (this is what makes
    // Herdr forward Shift-Enter instead of flattening it to a bare CR).
    // On Windows crossterm's push is a no-op (no ANSI keyboard protocol).
    #[cfg(not(windows))]
    assert!(
        run.screen.contains("\u{1b}[>1u"),
        "no kitty keyboard push on startup: {:?}",
        run.screen
    );
    let text = std::fs::read_to_string(run.session_dir.join("events.jsonl")).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let user = events
        .iter()
        .find(|event| event["type"] == "user")
        .expect("a user event");
    assert_eq!(user["text"], "first line\nsecond line");
}

/// The regression that motivated the full-screen rewrite: three turns, each
/// with a long answer and a tool call, must all remain reachable from the
/// viewport. The second answer in particular used to vanish into the
/// terminal's scrollback when the inline view handed rows away.
#[test]
fn three_turns_with_calls_and_long_answers_are_all_reachable() {
    let mut app = new_app();
    let mut answers = Vec::new();
    for turn in 1..=3 {
        app.push(Ev::TurnStarted {
            turn,
            provider: "openrouter".into(),
            model: "test/model".into(),
            limit: None,
        });
        app.submit(&format!("question {turn}"));
        let answer = format!(
            "answer {turn}: {}",
            "a long sentence that wraps across several rows of the viewport ".repeat(6)
        );
        answers.push(answer.clone());
        app.push(Ev::Delta(answer.clone()));
        app.push(Ev::ToolStarted {
            name: "bash".into(),
            input: serde_json::json!({"command": format!("echo {turn}")}),
        });
        app.push(Ev::ToolSettled {
            name: "bash".into(),
            input: serde_json::json!({"command": format!("echo {turn}")}),
            outcome: "ok".into(),
            exit_code: Some(0),
            elapsed_ms: 1,
            bytes: 2,
            lines: 1,
            spilled: false,
            out_path: String::new(),
            tail: String::new(),
        });
        app.push(Ev::Assistant {
            text: answer.clone(),
        });
        app.push(Ev::TurnEnded {
            outcome: "done".into(),
            final_text: answer,
        });
    }
    // Bottom follows the last answer; Home reaches the first question.
    let bottom = snapshot(&mut app);
    assert!(bottom.contains("answer 3"), "{bottom}");
    app.press(tui::Press::Top);
    let top = snapshot(&mut app);
    assert!(
        top.contains("question 1"),
        "first prompt unreachable: {top}"
    );
    app.press(tui::Press::PageDown);
    let middle = snapshot(&mut app);
    assert!(
        middle.contains("answer 1") || middle.contains("answer 2"),
        "an earlier answer is unreachable when scrolling down: {middle}"
    );
    app.press(tui::Press::Bottom);
    assert!(snapshot(&mut app).contains("answer 3"));
}

/// Resize keeps the reader's logical scroll position: the first visible row
/// maps from the old rows/viewport to the new ones, and a deliberate scroll is
/// never silently turned back into "follow the bottom".
#[test]
fn resize_keeps_the_scroll_position_and_the_new_lines_marker() {
    let mut app = new_app();
    for i in 0..40 {
        app.push_text(&format!(
            "line {i} with enough words to wrap across a narrow pane"
        ));
    }
    app.set_size(60, 20);
    app.press(tui::Press::Top);
    assert_eq!(app.scroll_position(), Some(0));
    // A wider window reflows into fewer rows; the top stays at the start.
    app.set_size(80, 20);
    assert!(app.scroll_position().is_some(), "resize cleared the scroll");
    assert_eq!(app.scroll_position(), Some(0));
    // Following at the bottom stays following across a resize.
    app.press(tui::Press::Bottom);
    assert_eq!(app.scroll_position(), None);
    app.set_size(60, 24);
    assert_eq!(app.scroll_position(), None);
}

/// The "N new lines" marker belongs to a reader who scrolled away; sitting at
/// the bottom must not accumulate it.
#[test]
fn the_new_lines_marker_only_counts_output_after_scrolling_away() {
    let mut app = new_app();
    for i in 0..40 {
        app.push_text(&format!("settled line {i}"));
    }
    assert_eq!(app.pending_new, 0, "the marker appeared while following");
    app.press(tui::Press::Top);
    app.push_text("a late line while scrolled away");
    assert!(
        app.pending_new > 0,
        "no marker after output while scrolled away"
    );
    app.press(tui::Press::Bottom);
    assert_eq!(app.pending_new, 0);
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
        session_dir: std::fs::read_dir(state.join("sessions"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path(),
    }
}
