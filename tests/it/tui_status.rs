//! TUI status-line reporting tests.
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::{Mock, Reply, sse, text_turn, tool_turn};

use super::tui_output::{herdr_records, plain};
use super::tui_pty::drive_in_a_pty;

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
