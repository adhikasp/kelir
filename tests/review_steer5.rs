//! STEER-5 review: adversarial tests against the retry / governor change.
//!
//! These are the reviewer's, not the implementer's. A test that fails here is a
//! finding (see `docs/reviews/steer5-review.md`).

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
mod support;

use std::time::{Duration, Instant};

use serde_json::Value;
use support::*;

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

/// Brief: "`say --now` ... take effect immediately" during a wait. A 30 s
/// `Retry-After` must not make a person who typed a correction wait it out.
#[tokio::test]
async fn say_now_during_a_wait_does_not_wait_it_out() {
    let mock = Mock::start(vec![
        too_many("30"),
        Reply::Sse(sse(&text_turn("heard you", 0))),
    ])
    .await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "retry_max_wait = 600\n",
    )
    .await;
    session.wait_for("error").await;
    let answer = session.ctl(&["say", "--now", "new plan: say hi"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let arrived = wait_until(Duration::from_secs(3), || mock.request_count() >= 2).await;
    assert!(
        arrived,
        "`say --now` during a 30 s Retry-After wait did not send a request within 3 s"
    );
    let requests = mock.requests();
    assert!(
        requests[1].body.to_string().contains("new plan: say hi"),
        "the retried request does not carry the --now message"
    );
    let _ = session.ctl(&["end"]).await;
    let _ = session.finish().await;
}

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

/// `end` during a wait ends the session at once (not after the wait).
#[tokio::test]
async fn end_during_a_wait_ends_the_session_at_once() {
    let mock = Mock::start(vec![too_many("30")]).await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "retry_max_wait = 600\n",
    )
    .await;
    session.wait_for("error").await;
    let started = Instant::now();
    let _ = session.ctl(&["end"]).await;
    let (code, stderr) = session.finish().await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "end took {:?}",
        started.elapsed()
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(mock.request_count(), 1);
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

/// Reset headers of every flavour produce a hint that is used.
#[tokio::test]
async fn a_reset_header_is_honoured_when_there_is_no_retry_after() {
    let mock = Mock::start(vec![
        Reply::HttpWith(
            429,
            vec![("x-ratelimit-reset-requests".into(), "1s".into())],
            "slow".into(),
        ),
        Reply::Sse(sse(&text_turn("fine", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "go"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let errors = provider_errors(&run.events);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["retry_in_ms"], 1000, "{}", errors[0]);
}

/// 4xx that are answers are never retried: 400, 401, 403, 404 and 422 end the
/// turn in `error` after exactly one request.
#[tokio::test]
async fn answers_are_never_retried() {
    for status in [400u16, 401, 403, 404, 422] {
        let mock = Mock::start(vec![
            Reply::Http(status, "no".into()),
            Reply::Sse(sse(&text_turn("must not be asked", 0))),
        ])
        .await;
        let run = run_task(&mock, &["run", "--json", "go"], &[], None, "").await;
        assert_ne!(run.code, 0, "{status}: {}", run.stderr);
        assert_eq!(run.one_event("turn_ended")["outcome"], "error", "{status}");
        let expected = if status == 401 || status == 403 { 2 } else { 1 };
        let seen = mock.request_count();
        // 401/403: the one credential re-read (STEER-2) may send a second.
        assert!(
            seen == 1 || (seen == expected && (status == 401 || status == 403)),
            "{status}: {seen} requests"
        );
    }
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

/// The response body can echo a credential (a gateway that quotes the request,
/// a 401 that prints the key). The wait's visible line never carries it
/// (implementer test); neither should the `error` event the brief calls "short
/// reason, never the body of secrets".
#[tokio::test]
async fn a_credential_echoed_in_a_body_never_reaches_events_or_output() {
    let mock = Mock::start(vec![
        Reply::Http(
            503,
            "upstream said: Authorization: Bearer test-openrouter-key was rejected".into(),
        ),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "go"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let everything = format!("{}\n{}", run.json_lines().len(), run.stderr);
    assert!(!run.stderr.contains("test-openrouter-key"), "{everything}");
    let events: Vec<String> = run.events.iter().map(|e| e.to_string()).collect();
    for event in events {
        assert!(
            !event.contains("test-openrouter-key"),
            "a credential reached an event: {event}"
        );
    }
}

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

/// A turn that paused on an outage is picked up by `kelir resume`: it retries
/// from the same fold with a fresh budget.
#[tokio::test]
async fn resume_after_a_provider_pause_retries_from_the_same_fold() {
    let mock = Mock::start(vec![
        Reply::Http(503, "down".into()),
        Reply::Http(503, "down".into()),
        Reply::Sse(sse(&text_turn("back", 0))),
    ])
    .await;
    let sandbox = Sandbox::new(
        "retry_base_ms = 20
retry_cap_ms = 40
retry_max_attempts = 2
retry_max_wait = 5
",
    );
    let first = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "go"], &[])
        .await;
    assert_eq!(first.code, 0, "{}{}", first.stdout, first.stderr);
    let id = sandbox.only_session();
    let paused = sandbox
        .log(&id)
        .into_iter()
        .rev()
        .find(|event| event["type"] == "turn_ended")
        .unwrap();
    assert_eq!(paused["outcome"], "paused", "{paused}");
    let before = sandbox.log(&id).len();
    let out = sandbox
        .kelir(&mock.base_url(), &["resume", "--json", id.as_str()], &[])
        .await;
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    let log = sandbox.log(&id);
    assert!(log.len() > before);
    let last_turn = log
        .iter()
        .rev()
        .find(|event| event["type"] == "turn_ended")
        .expect("a turn_ended");
    assert_eq!(last_turn["outcome"], "done", "{last_turn}");
    // The model was asked the same question: the resume added no user message.
    let requests = mock.requests();
    assert_eq!(
        requests[0].body["messages"], requests[2].body["messages"],
        "a resume after a pause re-sent a different fold"
    );
}
