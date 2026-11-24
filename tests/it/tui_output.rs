//! Full-screen TUI output: rendered snapshots, status rows and action verbs.
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::{ApplyEv as _, Ev, Mock, run_task};
use kelir::tui::commands::{self, Action, Parsed};
use kelir::tui::{self, App, Phase};

/// Full-screen height the snapshot tests render at.
pub(super) const SCREEN: u16 = 20;
use kelir::core::verbs;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use serde_json::json;

// ----------------------------------------------------------------- snapshots

pub(super) fn channels() -> (
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
pub(super) fn snapshot(app: &mut App) -> String {
    render_into(app, 60, SCREEN)
}

/// Render `app` into a `width`x`SCREEN` full-screen TestBackend.
pub(super) fn render_at(app: &mut App, width: u16) -> String {
    render_into(app, width, SCREEN)
}

pub(super) fn render_into(app: &mut App, width: u16, height: u16) -> String {
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
pub(super) fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

pub(super) fn new_app() -> App {
    let (to_tui, verbs) = channels();
    App::new(60, SCREEN as usize, to_tui, verbs, String::new())
}

/// The text of a pty stream, with every CSI/OSC escape removed: the assertion
/// is about what a reader sees, not about the bytes that paint it.
pub(super) fn plain(screen: &str) -> String {
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
pub(super) fn herdr_records(screen: &str) -> Vec<String> {
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
pub(super) fn state_row(screen: &str) -> &str {
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
pub(super) fn facts_row(screen: &str) -> &str {
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
        app.press(tui::Press::Edit(kelir::tui::editor::Key::Char(c)));
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
                kelir::tui::scrollback::display_width(row) <= width as usize,
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
