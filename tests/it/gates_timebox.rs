//! Integration tests filed by feature: `gates` (the built-in `timebox`).

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::time::Duration;

use crate::support::*;

/// A config with the built-in timebox as the only gate.
fn timebox(options: &str) -> String {
    format!("[gates.timebox]\nbuiltin = 'timebox'\n{options}")
}

#[tokio::test]
async fn a_closed_window_lets_the_turn_end() {
    // A one-second window and a model answer that takes two of them: by the time
    // it stops the clock is out, so the gate has nothing to send back.
    let mock = Mock::start(vec![Reply::SseAfter(
        Duration::from_secs(2),
        sse(&text_turn("done", 0)),
    )])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "say hi"],
        &[],
        None,
        &timebox("duration = '1s'\n"),
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let gates = run.events_of("gate");
    assert_eq!(gates.len(), 1, "{}", run.stderr);
    assert_eq!(gates[0]["event"], "turn_end");
    assert_eq!(gates[0]["decision"], "allow");
    assert!(
        gates[0]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("window closed"),
        "{}",
        gates[0]
    );
    assert_eq!(mock.request_count(), 1, "the turn was not sent back");
}

#[tokio::test]
async fn the_gate_stops_the_model_finishing_early_and_speaks_for_itself() {
    // The prompt asks for ten minutes and the model stops at once: the gate
    // denies, the steering reaches the model, and a second stop is refused too
    // while the budget lasts.
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first", 0))),
        Reply::Sse(sse(&text_turn("second", 0))),
    ])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "research it, you have 10m"],
        &[],
        None,
        &timebox(
            "max_denies = 1\nsteering = 'only {left} left of {target}, you stopped at {elapsed}'\n",
        ),
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        2,
        "the first stop was refused, the second was let through"
    );
    let second = requests[1].body.to_string();
    assert!(
        second.contains("only 10m left of 10m, you stopped at 0s"),
        "the steering reached the model: {second}"
    );
    let gates = run.events_of("gate");
    assert_eq!(gates[0]["decision"], "deny", "{}", gates[0]);
    assert_eq!(gates[1]["decision"], "deny", "{}", gates[1]);
    // The second deny is logged but not enforced: the budget is one.
    let nudges = run
        .events_of("user")
        .into_iter()
        .filter(|event| event["from"] == "gate")
        .count();
    assert_eq!(nudges, 1, "one nudge, not two: {}", run.stderr);
    assert!(
        run.events_of("error")
            .iter()
            .any(|event| event["message"].as_str().unwrap_or("").contains("ignored")),
        "{:?}",
        run.events_of("error")
    );
}

#[tokio::test]
async fn max_denies_lets_a_timebox_keep_the_turn_open() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("one", 0))),
        Reply::Sse(sse(&text_turn("two", 0))),
        Reply::Sse(sse(&text_turn("three", 0))),
    ])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "dig, you have 10m"],
        &[],
        None,
        &timebox("max_denies = 2\n"),
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(mock.request_count(), 3, "{}", run.stderr);
    let reasons: Vec<&str> = run
        .events_of("user")
        .into_iter()
        .filter(|event| event["from"] == "gate")
        .map(|event| event["text"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(reasons.len(), 2, "{reasons:?}");
    assert!(reasons[0].contains("not accepted"), "{reasons:?}");
    assert!(
        reasons[1].contains("Still not accepted (blocked 2 times this turn)"),
        "{reasons:?}"
    );
    // The steering the default prompt gives says what is left, so the model can
    // judge how much more work is owed.
    assert!(reasons[0].contains("You have 10m left."), "{reasons:?}");
}

#[tokio::test]
async fn a_prompt_with_no_clock_is_left_alone() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("done", 0)))]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "explain the 5 minute rule"],
        &[],
        None,
        &timebox("only_when_asked = true\n"),
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(mock.request_count(), 1, "{}", run.stderr);
    let gate = run.one_event("gate");
    assert_eq!(gate["decision"], "allow");
    assert_eq!(gate["reason"], "", "no window, no comment");
}

#[tokio::test]
async fn the_configured_duration_timeboxes_every_turn() {
    // No clock in the prompt, so the gate's own `duration` is the window.
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("one", 0))),
        Reply::Sse(sse(&text_turn("two", 0))),
    ])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "look into it"],
        &[],
        None,
        &timebox("duration = '15m'\nmax_denies = 1\nsteering = 'keep going'\n"),
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(mock.request_count(), 2, "{}", run.stderr);
    let requests = mock.requests();
    assert!(
        requests[1].body.to_string().contains("keep going"),
        "the steering reached the model"
    );
}

#[tokio::test]
async fn a_bad_duration_is_a_config_error_before_the_model_is_called() {
    for duration in ["'soon'", "0", "[]"] {
        let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("x", 0)))]).await;
        let run = run_task(
            &mock,
            &["run", "--json", "go"],
            &[],
            None,
            &timebox(&format!("duration = {duration}\n")),
        )
        .await;
        assert_ne!(run.code, 0, "duration {duration} was accepted");
        assert!(
            run.stderr.contains("duration"),
            "duration {duration}: {}",
            run.stderr
        );
        assert_eq!(mock.request_count(), 0, "a bad config reached the model");
    }
}

#[tokio::test]
async fn the_timebox_only_runs_at_turn_end() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("x", 0)))]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "go"],
        &[],
        None,
        "[gates.timebox]\nbuiltin = 'timebox'\nevent = 'before_tool'\n",
    )
    .await;
    assert_ne!(run.code, 0);
    assert!(
        run.stderr.contains("turn_end"),
        "the error names the event it supports: {}",
        run.stderr
    );
    assert_eq!(mock.request_count(), 0);
}

#[tokio::test]
async fn the_gate_document_names_the_budget_it_measured() {
    // A closed window still reports what it measured, so a session's log says
    // whether a turn was ever timeboxed and for how long.
    let mock = Mock::start(vec![Reply::SseAfter(
        Duration::from_secs(2),
        sse(&text_turn("done", 0)),
    )])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "check"],
        &[],
        None,
        &timebox("duration = '1s'\n"),
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let gate = run.one_event("gate");
    assert_eq!(gate["decision"], "allow");
    assert_eq!(gate["gate"], "timebox");
    let reason = gate["reason"].as_str().unwrap_or("");
    assert!(reason.contains("of 1s"), "{reason:?}");
}
