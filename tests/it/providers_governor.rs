//! STEER-5 review: adversarial tests against the retry / governor change.
//!
//! These are the reviewer's, not the implementer's. A test that fails here is a
//! finding (see `docs/reviews/steer5-review.md`).

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::time::{Duration, Instant};

use crate::support::*;
use serde_json::Value;

fn provider_errors(log: &[Value]) -> Vec<&Value> {
    log.iter()
        .filter(|event| event["type"] == "error" && event["where"] == "provider")
        .collect()
}

fn too_many(retry_after: &str) -> Reply {
    Reply::HttpWith(
        429,
        vec![("Retry-After".into(), retry_after.into())],
        "slow down".into(),
    )
}

async fn wait_until(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

// ------------------------------------------------------------ interruptibility

/// Brief: "`model` (the retry uses the new model) ... take effect immediately".
#[tokio::test]
async fn model_during_a_long_wait_retries_at_once() {
    let mock = Mock::start(vec![
        too_many("30"),
        Reply::Sse(sse(&text_turn("on the new model", 0))),
    ])
    .await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "retry_max_wait = 600\n",
    )
    .await;
    session.wait_for("error").await;
    let answer = session.ctl(&["model", "go/model"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let arrived = wait_until(Duration::from_secs(3), || mock.request_count() >= 2).await;
    assert!(
        arrived,
        "`model` during a 30 s wait did not retry within 3 s"
    );
    assert_eq!(mock.requests()[1].body["model"], "go/model");
    let _ = session.ctl(&["end"]).await;
    let _ = session.finish().await;
}

// ----------------------------------------------------------------- hammering

/// A provider that answers `429 Retry-After: 0` forever is not a licence to
/// send 200 requests back to back. A floor (or any jitter) between attempts is
/// the least a "backoff" does.
#[tokio::test]
async fn retry_after_zero_is_not_a_hot_loop() {
    let script: Vec<Reply> = (0..250).map(|_| too_many("0")).collect();
    let mock = Mock::start(script).await;
    let started = Instant::now();
    let run = run_task(
        &mock,
        &["run", "--json", "go"],
        &[],
        None,
        "retry_max_wait = 600\nretry_max_attempts = 0\n",
    )
    .await;
    let elapsed = started.elapsed();
    let requests = mock.request_count();
    eprintln!(
        "retry-after 0: {requests} requests in {elapsed:?}, code {}",
        run.code
    );
    let rate = requests as f64 / elapsed.as_secs_f64().max(0.001);
    assert!(
        rate < 50.0,
        "{requests} requests in {elapsed:?} ({rate:.0}/s) against a provider that said 429"
    );
}

// ------------------------------------------------------------ absurd headers

/// Negative, garbage, overflowing and fractional `Retry-After` values are not
/// hints: the exponential schedule takes over, nothing panics, nothing waits
/// for ever.
#[tokio::test]
async fn absurd_retry_after_values_fall_back_to_the_schedule() {
    let mock = Mock::start(vec![
        too_many("-5"),
        too_many("tomorrow"),
        too_many("99999999999999999999999"),
        too_many("1.5"),
        too_many("1e3"),
        Reply::Sse(sse(&text_turn("fine", 0))),
    ])
    .await;
    let started = Instant::now();
    let run = run_task(
        &mock,
        &["run", "--json", "go"],
        &[],
        None,
        "retry_max_wait = 600\nretry_max_attempts = 10\n",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.one_event("turn_ended")["outcome"], "done");
    let errors = provider_errors(&run.events);
    assert_eq!(errors.len(), 5, "{errors:#?}");
    for error in errors {
        let wait = error["retry_in_ms"].as_u64().unwrap();
        assert!(wait <= 400, "a garbage header produced a {wait} ms wait");
    }
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// A huge but valid `Retry-After` is clamped, does not fit the budget, and so
/// pauses at once rather than hanging or panicking.
#[tokio::test]
async fn a_huge_retry_after_pauses_instead_of_hanging() {
    let mock = Mock::start(vec![too_many("999999999")]).await;
    let started = Instant::now();
    let run = run_task(&mock, &["run", "--json", "go"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.one_event("turn_ended")["outcome"], "paused");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(mock.request_count(), 1);
}

/// 408 and 529 are retried like 429 and 5xx.
#[tokio::test]
async fn retryable_statuses_are_retried() {
    for status in [408u16, 500, 502, 503, 504, 529] {
        let mock = Mock::start(vec![
            Reply::Http(status, "later".into()),
            Reply::Sse(sse(&text_turn("ok", 0))),
        ])
        .await;
        let run = run_task(&mock, &["run", "--json", "go"], &[], None, "").await;
        assert_eq!(run.code, 0, "{status}: {}", run.stderr);
        assert_eq!(mock.request_count(), 2, "{status}");
    }
}

// ------------------------------------------------------------------ secrets

// ----------------------------------------------------------------- governor

/// The governor belongs to the provider a request goes to. After a `model`
/// swap to a provider with `max_concurrent_requests`, requests take its slot.
#[tokio::test]
async fn the_governor_follows_a_model_swap() {
    let mock = Mock::start(vec![
        too_many("1"),
        Reply::Sse(sse(&text_turn("swapped", 0))),
    ])
    .await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "retry_max_wait = 600\n[providers.opencode-go]\nmax_concurrent_requests = 1\n",
    )
    .await;
    session.wait_for("error").await;
    let answer = session.ctl(&["model", "opencode-go/some-model"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done", "{ended}");
    assert_eq!(
        mock.requests()[1].body["model"],
        "some-model",
        "the swap did not happen"
    );
    let locks = session.state.join("locks");
    let used = std::fs::read_dir(&locks)
        .map(|dir| dir.count())
        .unwrap_or(0);
    assert!(
        used > 0,
        "the request after swapping to a governed provider took no slot ({locks:?})"
    );
    let _ = session.ctl(&["end"]).await;
    let _ = session.finish().await;
}

/// A held slot blocks a request only while held: when the holder's request
/// ends before ours is built, ours is not delayed by a poll longer than
/// SLOT_POLL. (Latency sanity, not a hard bound.)
#[tokio::test]
async fn a_free_slot_costs_no_noticeable_latency() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("fast", 0)))]).await;
    let started = Instant::now();
    let run = run_task(
        &mock,
        &["run", "--json", "go"],
        &[],
        None,
        "[providers.openrouter]\nmax_concurrent_requests = 1\n",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(started.elapsed() < Duration::from_secs(5));
}

// ---------------------------------------------------------- pause and resume
