//! tui window tests: a_model_swap_announces_the_new, a_slow_list_is_applied_to_an_i, the_tui_banner_is_silent_about, the_tui_banner_names_a_window_.
#![allow(clippy::pedantic, clippy::nursery)]

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::support::{ApplyEv as _, Ev, Mock, Reply, run_task, sse, text_turn, tool_turn};
use kelir::commands::{self, Action, Parsed};
use kelir::tui::{self, App, Phase};
use kelir::verbs;
use serde_json::json;

use super::tui_output::{herdr_records, plain};
use super::tui_pty::{
    PtyInput, PtyRun, drive_idle_in_a_pty, drive_in_a_pty, drive_pty_in_idle_or_turn,
    panic_in_a_pty, tiny_png,
};

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
