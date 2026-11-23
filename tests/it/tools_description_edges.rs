//! Review adversary tests for the `bash` `description` feature.
//!
//! Focus: exact fallback (byte-for-byte), control-character safety, character
//! (not byte) cap, non-ASCII/wide labels, and no data loss in the event log.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::*;
use kelir::tools_view::{DESCRIPTION_MAX, description};
use serde_json::json;

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
    let started = run.one_event("call_started");
    assert_eq!(started["info"]["input"]["description"], attack);
    assert_eq!(started["info"]["input"]["command"], SIMPLE);
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
