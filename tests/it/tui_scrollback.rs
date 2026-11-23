//! TUI scrollback, folds and outcome rows, probed at the app-model level.
#![allow(clippy::pedantic, clippy::nursery)]

use super::tui_output::{new_app, snapshot};
use crate::support::{ApplyEv as _, Ev};
use kelir::tui::{self, App, Phase, Status, facts_for_width, status_text};
use serde_json::json;

fn app(width: usize) -> App {
    let (to_tui, _events) = tokio::sync::mpsc::unbounded_channel();
    let (verbs, _incoming) = tokio::sync::mpsc::unbounded_channel();
    App::new(width, 8, to_tui, verbs, String::new())
}

fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn call(name: &str, input: serde_json::Value, outcome: &str, exit: Option<i32>) -> Ev {
    Ev::ToolSettled {
        name: name.into(),
        input,
        outcome: outcome.into(),
        exit_code: exit,
        elapsed_ms: 100,
        bytes: 4,
        lines: 1,
        spilled: false,
        out_path: String::new(),
        tail: if exit.is_some_and(|c| c != 0) {
            "boom".into()
        } else {
            String::new()
        },
    }
}

fn ok(command: &str) -> Ev {
    call("bash", json!({"command": command}), "ok", Some(0))
}

fn bad(command: &str, code: i32) -> Ev {
    call("bash", json!({"command": command}), "ok", Some(code))
}

fn rows(app: &mut App) -> Vec<String> {
    app.take_scroll().iter().map(line_text).collect()
}

fn prose(app: &mut App, text: &str) {
    app.push(Ev::Delta(text.into()));
    app.push(Ev::Assistant { text: text.into() });
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

/// Every call keeps its own line AND its failed ones keep their tail, wherever
/// they sit in the run. There is no fold to hide them in.
#[test]
fn every_failure_in_a_fold_keeps_its_line_and_tail() {
    let mut app = app(80);
    for event in [
        ok("a"),
        bad("first-fail", 2),
        ok("b"),
        ok("c"),
        bad("second-fail", 7),
        ok("d"),
    ] {
        app.push(event);
    }
    prose(&mut app, "done");
    let scroll = rows(&mut app);
    assert!(
        !scroll.iter().any(|r| r.contains("calls")),
        "no fold: {scroll:?}"
    );
    assert_eq!(
        scroll.iter().filter(|r| r.contains("4 B")).count(),
        6,
        "one line per call: {scroll:?}"
    );
    for (cmd, code) in [("first-fail", "exit 2"), ("second-fail", "exit 7")] {
        let at = scroll
            .iter()
            .position(|r| r.contains(cmd))
            .unwrap_or_else(|| panic!("{cmd} hidden: {scroll:?}"));
        assert!(scroll[at].contains(code), "{scroll:?}");
        assert!(
            scroll[at + 1].contains("boom"),
            "the tail follows the failed line: {scroll:?}"
        );
    }
}

/// A run where every call failed has no checked line at all.
#[test]
fn an_all_failed_run_has_no_checkmark_line() {
    let mut app = app(80);
    for c in ["x", "y", "z"] {
        app.push(bad(c, 1));
    }
    prose(&mut app, "all failed");
    let scroll = rows(&mut app);
    assert!(
        !scroll
            .iter()
            .any(|r| r.contains("calls") || r.starts_with('✓') || r.starts_with('√')),
        "{scroll:?}"
    );
    for c in ["$ x", "$ y", "$ z"] {
        assert!(scroll.iter().any(|r| r.contains(c)), "{scroll:?}");
    }
}

/// An `edit` keeps its own line, so the transcript names the file that was
/// changed. No fold groups the bash calls away.
#[test]
fn an_edit_inside_a_fold_keeps_its_own_line() {
    let mut app = app(80);
    app.push(ok("cat a"));
    app.push(call(
        "edit",
        json!({"path": "src/lib.rs", "old": "a", "new": "b"}),
        "ok",
        None,
    ));
    app.push(ok("cargo test"));
    prose(&mut app, "patched");
    let scroll = rows(&mut app);
    assert!(
        !scroll.iter().any(|r| r.contains("calls")),
        "no fold: {scroll:?}"
    );
    assert!(
        scroll.iter().any(|r| r.contains("edit src/lib.rs")),
        "the file change is named in the transcript (F-edit): {scroll:?}"
    );
    assert_eq!(
        scroll.iter().filter(|r| r.contains("4 B")).count(),
        3,
        "one line per call: {scroll:?}"
    );
}

/// A call cancelled by Esc keeps its own `cancelled` line. There is no fold
/// to count it as a checked (successful) call.
#[test]
fn a_cancelled_call_is_not_counted_as_a_checked_call() {
    let mut app = app(80);
    app.push(ok("a"));
    app.push(ok("b"));
    app.push(call(
        "bash",
        json!({"command": "sleep 20"}),
        "stopped",
        None,
    ));
    app.push(Ev::TurnEnded {
        outcome: "stopped".into(),
        final_text: String::new(),
    });
    let scroll = rows(&mut app);
    assert!(
        !scroll.iter().any(|r| r.contains("calls")),
        "no fold: {scroll:?}"
    );
    assert!(
        scroll
            .iter()
            .any(|r| r.contains("sleep 20") && r.contains("cancelled")),
        "the stopped call keeps its own line: {scroll:?}"
    );
    assert_eq!(
        scroll
            .iter()
            .filter(|r| r.starts_with("  $ a") || r.starts_with("  $ b"))
            .count(),
        2,
        "the two clean calls each keep a line: {scroll:?}"
    );
}

/// Ordering: answer, then a run of calls, then an answer, then one call and an
/// answer. The scrollback must follow event order (the old B1 bug).
#[test]
fn prose_and_folds_keep_event_order() {
    let mut app = app(60);
    prose(&mut app, "Before the calls.");
    for c in ["a", "b", "c"] {
        app.push(ok(c));
    }
    prose(&mut app, "After the calls.");
    app.push(ok("solo"));
    prose(&mut app, "After the solo call.");
    let scroll = rows(&mut app);
    let at = |needle: &str| {
        scroll
            .iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("{needle}: {scroll:?}"))
    };
    assert!(at("Before the calls") < at("$ a"), "{scroll:?}");
    assert!(at("$ c") < at("After the calls"), "{scroll:?}");
    assert!(at("After the calls") < at("$ solo"), "{scroll:?}");
    assert!(at("$ solo") < at("After the solo call"), "{scroll:?}");
}

/// Streaming: each settled call commits its line immediately, before the
/// first streamed prose row.
#[test]
fn a_streaming_delta_commits_the_fold_before_its_own_rows() {
    let mut app = app(60);
    for c in ["a", "b", "c"] {
        app.push(ok(c));
    }
    app.push(Ev::Delta(
        "First paragraph.\n\nSecond paragraph being streamed".into(),
    ));
    let mut scroll = rows(&mut app);
    let live: Vec<String> = app.live_lines().iter().map(line_text).collect();
    scroll.extend(live);
    let last_call = scroll
        .iter()
        .rposition(|r| r.contains("4 B"))
        .expect("call lines");
    let first = scroll
        .iter()
        .position(|r| r.contains("First paragraph"))
        .expect("prose");
    assert!(last_call < first, "{scroll:?}");
}

/// The open group previews exactly what it will commit (no shape change).
#[test]
fn the_open_group_preview_equals_what_commits() {
    let mut app = app(80);
    for event in [ok("a"), bad("f", 1), ok("b"), ok("c")] {
        app.push(event);
    }
    let live: Vec<String> = app.live_lines().iter().map(line_text).collect();
    prose(&mut app, "x");
    let committed = rows(&mut app);
    for row in live.iter().filter(|r| !r.trim().is_empty()) {
        assert!(
            committed.contains(row),
            "{row:?} previewed but not committed: {committed:?}"
        );
    }
}

/// A `done` turn writes no outcome line; `paused`, `stopped`, `error` write one
/// each, with no duration.
#[test]
fn outcome_lines_exist_only_for_turns_that_need_a_human() {
    for (outcome, expect_line) in [
        ("done", false),
        ("paused", true),
        ("stopped", true),
        ("error", true),
    ] {
        let mut app = app(80);
        app.push(Ev::TurnStarted {
            turn: 1,
            provider: "p".into(),
            model: "m".into(),
            limit: None,
        });
        app.push(Ev::Step {
            step: 1,
            provider: "p".into(),
            model: "m".into(),
            turn_steps: 1,
            limit: None,
        });
        app.push(Ev::Usage {
            prompt: 900,
            completion: 100,
            cached: 800,
        });
        prose(&mut app, "answer");
        app.push(Ev::TurnEnded {
            outcome: outcome.into(),
            final_text: "answer".into(),
        });
        let scroll = rows(&mut app);
        let has = scroll.iter().any(|r| r.contains("1 step"));
        assert_eq!(has, expect_line, "{outcome}: {scroll:?}");
        assert!(
            !scroll.iter().any(|r| r.contains('$') || r.contains('%')),
            "{outcome}: {scroll:?}"
        );
        if outcome == "done" {
            let last = scroll.iter().rev().find(|r| !r.trim().is_empty()).unwrap();
            assert_eq!(last, "answer", "{scroll:?}");
        }
    }
}

/// The facts row never grows a step/cache part and never exceeds its width.
#[test]
fn the_facts_row_is_phase_model_tokens_at_every_width() {
    let status = Status {
        provider: "openrouter".into(),
        model: "deepseek/deepseek-v4.1-flash-1017".into(),
        step: 9,
        turn_steps: 4,
        limit: Some(5),
        prompt_tokens: 12_000,
        completion_tokens: 500,
        cached_tokens: 9_000,
        phase: Phase::Working,
        ..Status::default()
    };
    let full = status_text(&status);
    for banned in ["step", "cache", "$", "%"] {
        assert!(!full.contains(banned), "{full}");
    }
    for width in [120usize, 80, 60, 40, 30, 20, 12, 8] {
        let text = facts_for_width(&status, width);
        assert!(
            unicode_width::UnicodeWidthStr::width(text.as_str()) <= width,
            "{width}: {text:?}"
        );
    }
}
