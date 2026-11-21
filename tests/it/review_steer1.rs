//! Reviewer probes for STEER-1 (provider model metadata, `docs/reviews/STEER-1-review.md`).
//!
//! What the change promises: the compaction window comes from the provider's
//! `GET /models`, the list is cached for a day and reused by the next start, a
//! failing or odd answer never fails the session and falls back to the default
//! window, a person's `window`/`[model_windows]` still wins, and the window
//! never leaks into the request bytes. The two probes that recorded findings
//! (S1 endpoint keying, N2 clock skew) now run: they are the regression tests
//! for the fixes.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::PathBuf;

use crate::support::*;
use serde_json::Value;

/// The cache file for a provider on a base URL, as the session names it: the
/// endpoint id, not the provider name (STEER-1 review S1).
fn cache_file(state: &std::path::Path, provider: &str, base_url: &str) -> PathBuf {
    state.join(format!(
        "models-{}.json",
        kelir::models::endpoint_id(provider, base_url)
    ))
}

/// The state a verb's answer reports.
fn state_of(answer: &Ctl) -> Value {
    answer.response()["state"].clone()
}

async fn window_of(session: &mut LiveSession) -> (u64, String) {
    session.wait_for("turn_ended").await;
    let state = state_of(&session.ctl(&["stop"]).await);
    (
        state["window"].as_u64().expect("window"),
        state["window_source"]
            .as_str()
            .expect("window_source")
            .to_string(),
    )
}

/// One `GET /models`, then the cache: the second start of the same box reads
/// the list from disk and asks the provider nothing.
#[tokio::test]
async fn the_provider_list_is_fetched_once_and_the_next_start_uses_the_cache() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("one", 0))),
        Reply::Sse(sse(&text_turn("two", 0))),
    ])
    .await;

    let mut first = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    assert_eq!(
        window_of(&mut first).await,
        (262_144, "api".to_string()),
        "the first start takes the window from the provider's list"
    );
    let state_dir: PathBuf = first.state.clone();
    let file = cache_file(&state_dir, "openrouter", &mock.base_url());
    let cached = std::fs::read_to_string(&file).expect("the list is cached");
    // The session's own temp holds its state dir: keep a copy of just the cache
    // so a second state dir can be seeded with it.
    let kept = tempfile::tempdir().unwrap();
    let warmed: PathBuf = kept.path().join("state");
    std::fs::create_dir_all(&warmed).unwrap();
    let warmed_file = cache_file(&warmed, "openrouter", &mock.base_url());
    std::fs::copy(&file, &warmed_file).unwrap();
    first.end().await;
    assert_eq!(mock.models_requests(), 1);

    let mut second = LiveSession::start_reusing(
        warmed.clone(),
        &mock,
        &["run", "--keep", "--json", "go"],
        "",
    )
    .await;
    assert_eq!(
        window_of(&mut second).await,
        (262_144, "cache".to_string()),
        "the second start answers from the cache, and says so"
    );
    second.end().await;
    assert_eq!(
        mock.models_requests(),
        1,
        "the second start must not ask the provider again"
    );
    assert_eq!(
        std::fs::read_to_string(&warmed_file).unwrap(),
        cached,
        "a fresh cache is not rewritten"
    );
}

/// A 401, a 500 and a body that is not JSON: the session still runs, on the
/// default window, and the failure is what the cache records for the next hour.
#[tokio::test]
async fn a_failing_model_list_does_not_fail_the_session() {
    let cases = [
        (401u16, "no key for you".to_string(), "unauthorised"),
        (500, "the list is down".to_string(), "server error"),
        (200, r#"{"data": [ {"id": "#.to_string(), "malformed"),
    ];
    for (status, body, label) in cases {
        let mock = Mock::start_with_models(
            vec![Reply::Sse(sse(&text_turn("still here", 0)))],
            ModelsAnswer::http(status, body),
        )
        .await;
        let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
        assert_eq!(
            window_of(&mut session).await,
            (128_000, "default".to_string()),
            "{label}: the default window answers when the list does not"
        );
        let state_dir = session.state.clone();
        assert_eq!(mock.models_requests(), 1, "{label}: one try, no loop");
        let path = cache_file(&state_dir, "openrouter", &mock.base_url());
        let cache = std::fs::read_to_string(path).unwrap_or_default();
        let cache: Value = serde_json::from_str(&cache).unwrap_or(Value::Null);
        assert!(
            cache["failed_at_ms"].as_u64().unwrap_or(0) > 0,
            "{label}: the failure is recorded so the next hour is quiet: {cache}"
        );
        session.end().await;
    }
}

/// The real OpenRouter list is ~1 MB of models; a long list must be read whole,
/// including the entry near its end.
#[tokio::test]
async fn a_long_model_list_is_read_to_the_end() {
    let mut models = String::from(r#"{"data":["#);
    for index in 0..1_500 {
        models.push_str(&format!(
            r#"{{"id":"vendor{index}/model-{index}","context_length":4096,"supported_parameters":["tools"]}},"#
        ));
    }
    models.push_str(r#"{"id":"test/model","context_length":131072}]}"#);

    let mock = Mock::start_with_models(
        vec![Reply::Sse(sse(&text_turn("ok", 0)))],
        ModelsAnswer::ok(models),
    )
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    assert_eq!(
        window_of(&mut session).await,
        (131_072, "api".to_string()),
        "the last entry of a 1 500-model list is not lost"
    );
    session.end().await;
}

/// A configured window still wins over the provider, and `state` says which
/// source answered.
#[tokio::test]
async fn a_configured_window_wins_over_the_provider() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session =
        LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "window = 4096\n").await;
    assert_eq!(window_of(&mut session).await, (4_096, "config".to_string()));
    session.end().await;
}

/// The window is bookkeeping: two sessions of the same task and cwd, one with a
/// configured window and one on the provider's, send byte-identical requests
/// apart from the session id.
#[tokio::test]
async fn the_window_does_not_change_the_request_bytes() {
    let shared = tempfile::tempdir().unwrap();
    let cwd = shared.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.to_string_lossy().to_string();

    let provider_mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let config_mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;

    let mut on_provider = LiveSession::start(
        &provider_mock,
        &["run", "--keep", "--json", "--cwd", &cwd, "say hi"],
        "",
    )
    .await;
    assert_eq!(
        window_of(&mut on_provider).await,
        (262_144, "api".to_string())
    );
    let state = on_provider.state.clone();
    on_provider.end().await;

    // The second session is the *same machine*: it reuses the state dir, so
    // the home and config dirs its system prompt names (as the skills
    // directories) are the same strings. Only the window differs.
    let mut on_config = LiveSession::start_reusing(
        state,
        &config_mock,
        &["run", "--keep", "--json", "--cwd", &cwd, "say hi"],
        "window = 4096\n",
    )
    .await;
    assert_eq!(
        window_of(&mut on_config).await,
        (4_096, "config".to_string())
    );
    on_config.end().await;

    // The two live sessions get different ids; strip both the body field and
    // the session block embedded in the system prompt before comparing.
    let strip = |body: &Value| {
        let mut body = body.clone();
        let obj = body.as_object_mut().unwrap();
        obj.remove("session_id");
        if let Some(messages) = obj.get_mut("messages").and_then(|m| m.as_array_mut())
            && let Some(content) = messages.first_mut().and_then(|m| m.get_mut("content"))
            && let Some(text) = content.as_str()
        {
            let clean = text
                .split("\n# Session\n")
                .next()
                .unwrap_or(text)
                .to_string();
            *content = Value::String(clean);
        }
        body
    };
    let provider_request = strip(&provider_mock.requests()[0].body);
    let config_request = strip(&config_mock.requests()[0].body);
    assert_eq!(
        provider_request, config_request,
        "a different window must not change one byte of the request"
    );
}

/// S1 (fixed): the cache is keyed by the *endpoint*, not the provider name, so
/// a second endpoint that shares a state dir gets its own numbers — and an
/// unreachable one falls back to `default` instead of to the other's cache.
#[tokio::test]
async fn a_cache_from_another_endpoint_is_not_reused() {
    let first_mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    // A gateway that serves the same model with a much smaller window.
    let second_mock = Mock::start_with_models(
        vec![Reply::Sse(sse(&text_turn("ok", 0)))],
        ModelsAnswer::ok(r#"{"data":[{"id":"test/model","context_length":8192}]}"#),
    )
    .await;

    let kept = tempfile::tempdir().unwrap();
    let state_dir = kept.path().join("state");

    let mut first = LiveSession::start_reusing(
        state_dir.clone(),
        &first_mock,
        &["run", "--keep", "--json", "go"],
        "",
    )
    .await;
    assert_eq!(window_of(&mut first).await, (262_144, "api".to_string()));
    first.end().await;

    let mut second = LiveSession::start_reusing(
        state_dir.clone(),
        &second_mock,
        &["run", "--keep", "--json", "go"],
        "",
    )
    .await;
    assert_eq!(
        window_of(&mut second).await,
        (8_192, "api".to_string()),
        "the new endpoint's own list must answer, not the other endpoint's cache"
    );
    second.end().await;
    assert_eq!(second_mock.models_requests(), 1);
}

/// S1, the other half: an *unreachable* endpoint falls back to `default` even
/// on a box whose state dir holds another endpoint's cache. The session still
/// runs (the step has a provider), and `state` says why the window is not the
/// provider's.
#[tokio::test]
async fn an_unreachable_endpoint_does_not_use_another_endpoints_cache() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let kept = tempfile::tempdir().unwrap();
    let state_dir = kept.path().join("state");

    let mut first = LiveSession::start_reusing(
        state_dir.clone(),
        &mock,
        &["run", "--keep", "--json", "go"],
        "",
    )
    .await;
    assert_eq!(window_of(&mut first).await, (262_144, "api".to_string()));
    first.end().await;

    // Same state dir, a base URL nothing listens on: the *cache* written for
    // the mock must not answer, or auto-compaction would never fire against a
    // provider whose real window is smaller.
    let second = LiveSession::start_with_env(
        state_dir.clone(),
        "http://127.0.0.1:9/v1",
        &["run", "--keep", "--json", "go"],
        "",
    )
    .await;
    // The fetch fails on its own (nothing listens there); give the session a
    // moment to record it, then read the state.
    let deadline = std::time::Instant::now() + CEILING;
    let state = loop {
        let state = state_of(&second.ctl(&["stop"]).await);
        let note = state["window_note"].as_str().unwrap_or("").to_string();
        if !state["models_fetching"].as_bool().unwrap_or(false)
            || std::time::Instant::now() > deadline
        {
            break state;
        }
        let _ = note;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(
        state["window"], 128_000,
        "an unreachable endpoint must fall back to the default, not reuse the other cache"
    );
    assert_eq!(state["window_source"], "default");
    assert_eq!(
        state["window_note"].as_str(),
        Some("the model list: offline"),
        "the fallback records why (N4): {state}"
    );
    second.end().await;
}

/// S2: the fetch no longer holds the first request. A list the endpoint holds
/// for longer than the start's grace delays nothing — the step runs on the
/// default or a stale cache and the answer is applied when it lands — and a
/// list that never arrives at all is a session that still works and says why.
#[tokio::test]
async fn a_slow_list_does_not_hold_the_first_request() {
    // The held lists are held by a gate the test owns, not by a number of
    // seconds: "slower than the turn" must not depend on how fast the turn ran
    // (STEER-9). Only the first is ever released, after the assertions.
    let late = std::sync::Arc::new(tokio::sync::Notify::new());
    let never = std::sync::Arc::new(tokio::sync::Notify::new());
    for (label, models, expected) in [
        (
            "a list slower than the start grace",
            ModelsAnswer::ok(MODELS_JSON).gated(late.clone()),
            "the list arrives after the turn",
        ),
        (
            "a list endpoint that is down",
            ModelsAnswer::http(503, "later"),
            "a failure is not a wait",
        ),
        (
            // The review's worst case: the body never comes at all.
            "a list endpoint that never answers",
            ModelsAnswer::ok(MODELS_JSON).gated(never.clone()),
            "a hang is bounded by the background fetch, not by the turn",
        ),
    ] {
        let mock = Mock::start_with_models(
            vec![
                Reply::Sse(sse(&tool_turn(
                    "call_1",
                    "bash",
                    &serde_json::json!({"command": "printf 'ran\\n'"}),
                ))),
                Reply::Sse(sse(&text_turn("done", 0))),
            ],
            models,
        )
        .await;
        let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
        // The first step ran and its call settled while the list was still
        // held (or failed): the step did not wait for the metadata. That is an
        // ordering the gate guarantees, so no stopwatch is involved.
        session.wait_for("tool_settled").await;
        assert_eq!(
            mock.models_requests(),
            1,
            "{label}: one ask, and the session did not wait for it to start"
        );
        session.wait_for("turn_ended").await;
        let state = state_of(&session.ctl(&["stop"]).await);
        assert_eq!(
            state["window_source"], "default",
            "{label}: nothing answered within the turn: {state}"
        );
        assert!(
            state["window_note"].is_string(),
            "{label}: {expected}; the fallback records why: {state}"
        );
        late.notify_one();
        session.end().await;
    }
}

/// S2: a list that lands after the start grace is applied at the next boundary,
/// so a slow endpoint still ends the turn on the provider's window — just not
/// the first request's.
#[tokio::test]
async fn a_list_that_lands_after_the_grace_is_applied_at_the_next_step() {
    let mock = Mock::start_with_models(
        vec![
            Reply::Sse(sse(&tool_turn(
                "call_1",
                "bash",
                &serde_json::json!({"command": "printf 'ran\\n'"}),
            ))),
            Reply::Sse(sse(&text_turn("done", 0))),
        ],
        ModelsAnswer::ok(MODELS_JSON).slow(std::time::Duration::from_millis(600)),
    )
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    // The tool result settles, the loop reaches `before_step`, and the list is
    // there by then.
    session.wait_for("tool_settled").await;
    let deadline = std::time::Instant::now() + CEILING;
    loop {
        let state = state_of(&session.ctl(&["stop"]).await);
        if state["window"] == 262_144 {
            assert_eq!(state["window_source"], "api", "{state}");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the list never landed: {state}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    session.wait_for("turn_ended").await;
    session.end().await;
}

/// N2 (fixed): a cache written while the clock was ahead keeps the fetch locked
/// out (`in_backoff`) until the clock catches up with the file.
#[test]
fn a_cache_dated_in_the_future_does_not_lock_the_fetch_out() {
    use kelir::models::Cache;
    let now = 1_800_000_000_000u64;
    let mut cache = Cache::new("openrouter", "http://h/v1", Vec::new(), 0);
    cache.failed_at_ms = Some(now + 365 * 24 * 60 * 60 * 1000);
    assert!(
        !cache.in_backoff(now),
        "a failure stamped in the future is a clock skew, not an hour of backoff"
    );
    cache.failed_at_ms = None;
    cache.fetched_at_ms = now + 365 * 24 * 60 * 60 * 1000;
    assert!(
        !cache.is_fresh(now),
        "a list fetched in the future is stale, not fresh for a year"
    );
}

/// The 80 % trigger at the 1M window the default model publishes, at a small
/// configured window, and at the default when no metadata answered. The e2e
/// arithmetic (a `window = 100` session that really compacts) is
/// `e2e_m3::the_auto_trigger_compacts_at_eighty_percent_of_the_window`.
///
/// STEER-6 added an absolute soft target (`compact_at_tokens`, default 150K) on
/// top of the fraction; these assertions pass `0` for it, which is the "no cap"
/// case, so they still describe the window arithmetic alone. What the two
/// combine to is `compact::tests::the_soft_target_caps_a_large_window` and
/// `e2e_steer6::the_soft_target_compacts_a_big_window_early`.
#[test]
fn the_auto_trigger_scales_with_the_window() {
    use kelir::compact::{DEFAULT_WINDOW, should_auto};
    const NO_TARGET: u64 = 0;
    // 80 % of 1 048 576 is 838 860.8: the first count that triggers is 838 861.
    assert!(!should_auto(1_048_576, 0.8, NO_TARGET, 838_860));
    assert!(should_auto(1_048_576, 0.8, NO_TARGET, 838_861));
    // A small configured window triggers proportionally earlier.
    assert!(!should_auto(8_192, 0.8, NO_TARGET, 6_553));
    assert!(should_auto(8_192, 0.8, NO_TARGET, 6_554));
    // With no metadata the default (128 000) still compacts at 102 400.
    assert_eq!(DEFAULT_WINDOW, 128_000);
    assert!(!should_auto(DEFAULT_WINDOW, 0.8, NO_TARGET, 102_399));
    assert!(should_auto(DEFAULT_WINDOW, 0.8, NO_TARGET, 102_400));
}
