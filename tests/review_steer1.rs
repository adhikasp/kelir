//! Reviewer probes for STEER-1 (provider model metadata, `docs/reviews/STEER-1-review.md`).
//!
//! What the change promises: the compaction window comes from the provider's
//! `GET /models`, the list is cached for a day and reused by the next start, a
//! failing or odd answer never fails the session and falls back to the default
//! window, a person's `window`/`[model_windows]` still wins, and the window
//! never leaks into the request bytes. Two probes are `#[ignore]`d because they
//! record findings that are real today (S1, S2 in the review): run them with
//! `cargo test --test review_steer1 -- --ignored` to watch them fail.

mod support;

use std::path::PathBuf;

use serde_json::Value;
use support::*;

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
    let cache_file = state_dir.join("models-openrouter.json");
    let cached = std::fs::read_to_string(&cache_file).expect("the list is cached");
    // The session's own temp holds its state dir: keep a copy of just the cache
    // so a second state dir can be seeded with it.
    let kept = tempfile::tempdir().unwrap();
    let warmed: PathBuf = kept.path().join("state");
    std::fs::create_dir_all(&warmed).unwrap();
    std::fs::copy(&cache_file, warmed.join("models-openrouter.json")).unwrap();
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
        std::fs::read_to_string(warmed.join("models-openrouter.json")).unwrap(),
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
        let cache =
            std::fs::read_to_string(state_dir.join("models-openrouter.json")).unwrap_or_default();
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

    let strip = |body: &Value| {
        let mut body = body.clone();
        body.as_object_mut().unwrap().remove("session_id");
        body
    };
    let provider_request = strip(&provider_mock.requests()[0].body);
    let config_request = strip(&config_mock.requests()[0].body);
    assert_eq!(
        provider_request, config_request,
        "a different window must not change one byte of the request"
    );
}

/// FINDING S1: the cache is keyed by the provider *name*, not its base URL, so
/// a second endpoint that shares a state dir reuses the first one's numbers.
#[ignore = "FINDING S1: models-<provider>.json ignores base_url"]
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

/// FINDING S2: a cache written while the clock was ahead keeps the fetch locked
/// out (`in_backoff`) until the clock catches up with the file.
#[ignore = "FINDING S2: a future failed_at_ms locks the fetch out"]
#[test]
fn a_cache_dated_in_the_future_does_not_lock_the_fetch_out() {
    use kelir::models::Cache;
    let now = 1_800_000_000_000u64;
    let mut cache = Cache::new("openrouter", Vec::new(), 0);
    cache.failed_at_ms = Some(now + 365 * 24 * 60 * 60 * 1000);
    assert!(
        !cache.in_backoff(now),
        "a failure stamped in the future is a clock skew, not an hour of backoff"
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
