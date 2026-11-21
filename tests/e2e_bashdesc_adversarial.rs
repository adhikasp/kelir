//! Review adversary tests for the `bash` `description` feature.
//!
//! Focus: exact fallback (byte-for-byte), control-character safety, character
//! (not byte) cap, non-ASCII/wide labels, and no data loss in the event log.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
mod support;

use kelir::tools_view::{DESCRIPTION_MAX, description};
use serde_json::json;
use support::*;

const SIMPLE: &str = "printf 'hello\\n'";

/// Every value that is not a non-empty string must fall back byte-exactly.
#[test]
fn every_invalid_description_falls_back_exactly() {
    let command = "printf 'a\\tb\\n' && echo done";
    let expected_head = kelir::tui::call_head("bash", &json!({"command": command}));
    let expected_label = kelir::tui::call_label("bash", &json!({"command": command}));
    for bad in [
        json!(null),
        json!(false),
        json!(0),
        json!(42),
        json!([]),
        json!({}),
        json!(""),
        json!("   \n\t "),
    ] {
        let mut input = json!({"command": command});
        input["description"] = bad.clone();
        assert_eq!(description(&input), None, "{bad}");
        assert_eq!(
            kelir::tui::call_head("bash", &input),
            expected_head,
            "head changed for {bad}"
        );
        assert_eq!(
            kelir::tui::call_label("bash", &input),
            expected_label,
            "live row changed for {bad}"
        );
    }
}

#[test]
fn a_wide_multibyte_label_is_capped_in_characters_and_keeps_the_verdict() {
    let label = "汉".repeat(200);
    let input = json!({"command": SIMPLE, "description": label});
    let got = description(&input).expect("a label");
    assert_eq!(got.chars().count(), DESCRIPTION_MAX, "{got}");
    assert!(got.ends_with('…'));
    assert_eq!(got.chars().take(5).collect::<String>(), "汉汉汉汉汉");
    let call = kelir::tools_view::CallLine {
        ordinal: 1,
        name: "bash".into(),
        head: got.clone(),
        outcome: "ok".into(),
        exit_code: Some(0),
        bytes: 9,
        lines: 1,
        spilled: false,
        out_path: String::new(),
        tail: String::new(),
        elapsed: None,
    };
    let line = call.line(kelir::theme::Theme::plain(), 60);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.ends_with("9 B"), "{text:?}");
    assert!(!text.contains("ok"), "{text:?}");
    assert_eq!(kelir::scrollback::display_width(&text), 60, "{text:?}");
    let now = std::time::Instant::now();
    let mut activity = kelir::activity::Activity::idle(now);
    activity.tool_started(now, &got);
    let state = activity.line(kelir::theme::Theme::plain(), 0, now, 60);
    let state_text: String = state.spans.iter().map(|s| s.content.as_ref()).collect();
    // Finding N-ROW: the live row's logical string can exceed the pane width
    // with a wide (2-column) description: `activity::line` measures the
    // spinner+word head as if that were the whole row, then appends a second
    // detail span that `tui::fit` clips to `width - head_width`, which is 0
    // here. Ratatui clips the painted grid, so no terminal corruption, but
    // the label is replaced by a single `…` in the 60-column state row.
    assert!(
        kelir::scrollback::display_width(&state_text) > 60,
        "{state_text:?}"
    );
    let backend = ratatui::backend::TestBackend::new(60, 8);
    let mut terminal = ratatui::Terminal::with_options(
        backend,
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Inline(8),
        },
    )
    .expect("a test terminal");
    terminal
        .draw(|frame| frame.render_widget(ratatui::widgets::Paragraph::new(state), frame.area()))
        .expect("draw");
    let painted: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .take(60)
        .map(|cell| cell.symbol())
        .collect();
    assert!(!painted.contains('\u{1b}'), "{painted:?}");

    // Adversary: a label whose characters are wider than the pane. The
    // painted grid must still be exactly `width` cells: every rendered
    // glyph occupies an integer number of cells, so clipping by display
    // width before the verdict must not overrun the verdict's own columns.
    let wide = description(&json!({"description": "汉".repeat(200)})).expect("a wide label");
    let mut wide_call = call.clone();
    wide_call.head = wide;
    let line = wide_call.line(kelir::theme::Theme::plain(), 60);
    let backend = ratatui::backend::TestBackend::new(60, 8);
    let mut terminal = ratatui::Terminal::with_options(
        backend,
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Inline(8),
        },
    )
    .expect("a test terminal");
    terminal
        .draw(|frame| frame.render_widget(ratatui::widgets::Paragraph::new(line), frame.area()))
        .expect("draw");
    let painted: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .take(60)
        .map(|cell| cell.symbol())
        .collect();
    assert!(painted.ends_with("9 B"), "{painted:?}");
    assert!(!painted.contains("ok"), "{painted:?}");
}

/// Finding N-ESC: ESC/CSI/OSC bytes in a model-supplied label are NOT
/// removed by the sanitizer. They survive into the headless progress line
/// (printed verbatim) and into the TUI's span strings.
///
/// Ratatui *does* filter control graphemes when painting a cell, so the TUI
/// cannot emit an OSC to the terminal; Herdr's status records are written by
/// a separate writer (`src/herdr.rs::record`, fixed literal words) and are
/// never built from a tool label. The unresolved exposure is the headless
/// `kelir run` stdout/stderr line and any log reader that replays it raw: a
/// model that puts `ESC ] 7501;state=done ESC \` in a description injects a
/// fake status record into the terminal's byte stream. This test documents
/// the current behavior without asserting a security property the code does
/// not have; it should be inverted when the sanitizer strips controls.
#[test]
fn control_bytes_and_osc_sequences_are_stripped_from_the_label() {
    let attacks = [
        "line one\n\x1b]7501;state=done\x1b\\",
        "click \x1b]8;;https://evil.example\x1b\\here\x1b]8;;\x1b\\",
        "\x1b]777;notify;fake;body\x1b\\",
        "\x1b[2J\x1b[1;1Hpretend-clear",
        "hello\r\u{7}world\u{2028}break\u{0000}tail",
    ];
    for attack in attacks {
        let input = json!({"command": SIMPLE, "description": attack});
        let label = description(&input).expect("a non-blank label");
        // No control byte or OSC introducer may survive in a human line.
        assert!(!label.contains('\n'), "{label:?}");
        assert!(
            !label.chars().any(|c| c.is_control()),
            "a control byte survived: {label:?}"
        );
        assert!(!label.contains("]8;;"), "{label:?}");
        assert!(!label.contains("]7501;"), "{label:?}");
        let head = kelir::tui::call_head("bash", &input);
        assert_eq!(head, label, "head uses the sanitized label");
        let row = kelir::tui::call_label("bash", &input);
        assert_eq!(row, label, "live row uses the sanitized label");
    }
}

/// S-1 regression: the sanitizer strips control bytes and OSC introducers
/// from the human line while the log keeps the exact bytes. Strip all control characters
/// except the whitespace that `split_whitespace` folds, so a model-supplied
/// label cannot inject an OSC 8 hyperlink, an OSC 7501 Herdr status record, a
/// CSI clear-screen, or NUL/BEL into the headless progress line.
///
/// Proving command: `cargo test --test e2e_bashdesc_adversarial -- --ignored`
#[test]
fn no_control_byte_may_survive_the_label_sanitizer() {
    for attack in [
        "line one\n\x1b]7501;state=done\x1b\\",
        "click \x1b]8;;https://evil.example\x1b\\here\x1b]8;;\x1b\\",
        "\x1b]777;notify;fake;body\x1b\\",
        "\x1b[2J\x1b[1;1Hpretend-clear",
        "hello\r\u{7}world\u{2028}break\u{0000}tail",
    ] {
        let input = json!({"command": SIMPLE, "description": attack});
        let label = description(&input).expect("a non-blank label");
        assert!(
            !label.chars().any(char::is_control),
            "a control byte survived in {label:?}"
        );
        assert!(!label.contains('\n'), "{label:?}");
    }
}

/// Desired behavior for N-ESC in the headless path (currently FAILING;
/// (formerly `#[ignore]`, now a regression test). The progress line is replayed in a
/// Herdr pane: OSC bytes in a model-supplied label must not reach stdout.
///
/// Proving command: `cargo test --test e2e_bashdesc_adversarial -- --ignored`
#[tokio::test]

async fn a_hostile_description_must_not_put_osc_bytes_on_headless_stdout() {
    let attack = "read \u{1b}]8;;https://evil.example\u{1b}\\files \u{1b}]7501;state=done\u{1b}\\";
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "evil",
            "bash",
            &json!({"command": SIMPLE, "description": attack}),
        ))),
        Reply::Sse(sse(&text_turn("ok", 80))),
    ])
    .await;
    let run = run_task(&mock, &["run", "audit"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let line = run
        .stdout
        .lines()
        .find(|line| line.starts_with("→ bash "))
        .expect("a headless progress line");
    assert!(!line.contains('\u{1b}'), "{line:?}");
    assert!(!line.contains("7501;state"), "{line:?}");
    assert!(!line.contains("]8;;"), "{line:?}");
}

#[tokio::test]
async fn a_hostile_description_reaches_the_log_verbatim_but_not_the_human_line() {
    let attack = "read \u{1b}]8;;https://evil.example\u{1b}\\files \u{1b}]7501;state=done\u{1b}\\";
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "evil",
            "bash",
            &json!({"command": SIMPLE, "description": attack}),
        ))),
        Reply::Sse(sse(&text_turn("ok", 80))),
    ])
    .await;
    let run = run_task(&mock, &["run", "audit"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    // The log keeps the exact bytes for audit and /expand.
    let started = run.one_event("tool_started");
    assert_eq!(started["input"]["description"], attack);
    assert_eq!(started["input"]["command"], SIMPLE);
    let assistant = run.events_of("assistant")[0];
    assert_eq!(assistant["tool_calls"][0]["args"]["description"], attack);

    // Finding N-ESC: the headless progress line contains the injected OSC
    // bytes verbatim. The command is not printed, but a terminal replaying
    // this output sees `ESC ] 8;;url ESC \` and `ESC ] 7501;state=done ESC \`.
    let line = run
        .stdout
        .lines()
        .find(|line| line.starts_with("→ bash "))
        .expect("a headless progress line");
    assert!(!line.contains("7501;state"), "{line:?}");
    assert!(!line.contains("]8;;"), "{line:?}");
    assert!(!line.contains('\u{1b}'), "{line:?}");
    assert!(!line.contains("printf"), "{line:?}");
}

#[tokio::test]
async fn a_long_and_wide_description_is_capped_in_the_real_headless_output() {
    let label = "审计".repeat(120);
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "wide",
            "bash",
            &json!({"command": SIMPLE, "description": label}),
        ))),
        Reply::Sse(sse(&text_turn("ok", 80))),
    ])
    .await;
    let run = run_task(&mock, &["run", "audit"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let line = run
        .stdout
        .lines()
        .find(|line| line.starts_with("→ bash "))
        .expect("a headless progress line");
    let head = line
        .strip_prefix("→ bash ")
        .unwrap()
        .split(" · ok · ")
        .next()
        .unwrap();
    assert_eq!(head.chars().count(), DESCRIPTION_MAX, "{head:?}");
    assert!(head.ends_with('…'), "{head:?}");
    let started = run.one_event("tool_started");
    assert_eq!(
        started["input"]["description"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        240
    );
}
