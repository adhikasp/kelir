//! STEER-5 end-to-end: retrying with backoff on provider errors.
//!
//! The real binary against the scripted mock, with the retry schedule scaled
//! down (`retry_base_ms`, `retry_max_attempts`) so a test about *behaviour*
//! finishes in a second. What is asserted: the `error` events (one per wait, with
//! `where: "provider"`, `retryable`, the status, the attempt and the wait), the
//! turn's outcome (`done`, `stopped`, `paused` — never `error` for an outage), the
//! requests the mock received, and that no response body ever leaks into a
//! visible line.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
mod support;

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::*;

fn of_type<'a>(log: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|event| event["type"] == event_type)
        .collect()
}

fn provider_errors(log: &[Value]) -> Vec<&Value> {
    of_type(log, "error")
        .into_iter()
        .filter(|event| event["where"] == "provider")
        .collect()
}

/// A 429 with `Retry-After: 1`, twice, then a good answer: the step succeeds,
/// the log shows two waits and no turn error.
#[tokio::test]
async fn two_retry_after_waits_then_the_step_succeeds() {
    let mock = Mock::start(vec![
        Reply::HttpWith(
            429,
            vec![("Retry-After".into(), "1".into())],
            "{\"error\":{\"type\":\"rate_limit_error\"}}".into(),
        ),
        Reply::HttpWith(
            429,
            vec![("Retry-After".into(), "1".into())],
            "{\"error\":{\"type\":\"rate_limit_error\"}}".into(),
        ),
        Reply::Sse(sse(&text_turn("made it", 0))),
    ])
    .await;
    let started = Instant::now();
    let run = run_task(&mock, &["run", "--json", "try"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let errors = provider_errors(&run.events);
    assert_eq!(errors.len(), 2, "{:#?}", run.events);
    for error in &errors {
        assert_eq!(error["retryable"], true);
        assert!(error["message"].as_str().unwrap().contains("429"));
    }
    assert_eq!(mock.request_count(), 3, "one request per attempt");
    assert_eq!(run.one_event("turn_ended")["outcome"], "done");
    assert_eq!(run.events_of("assistant").len(), 1);
    assert_eq!(run.result()["outcome"], "done");
    // Two `Retry-After: 1` waits really were waited: this is not a busy loop.
    assert!(
        started.elapsed() >= Duration::from_millis(1_800),
        "two 1 s waits took {:?}",
        started.elapsed()
    );
}

/// Without a header the schedule decides: the configured base is honoured and
/// the wait still ends in a good step.
#[tokio::test]
async fn a_429_without_a_header_follows_the_configured_schedule() {
    let mock = Mock::start(vec![
        Reply::Http(429, "slow down".into()),
        Reply::Sse(sse(&text_turn("at last", 0))),
    ])
    .await;
    // A fixed 400/400 ms schedule: the first wait is exactly 400 ms, and a test
    // can assert on the clock without depending on the jitter draw.
    let started = Instant::now();
    let run = run_task(
        &mock,
        &["run", "--json", "try"],
        &[],
        None,
        "retry_base_ms = 400\nretry_cap_ms = 400\nretry_max_attempts = 3\n",
    )
    .await;
    let waited = started.elapsed();
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let errors = provider_errors(&run.events);
    assert_eq!(errors.len(), 1);
    assert_eq!(mock.request_count(), 2);
    // Full jitter draws from the ceiling, so the wait is a bound rather than a
    // value. Assert on the wait the log *recorded*, not on the wall clock: a
    // busy runner makes the latter a flake (STEER-9), and the former is the
    // thing under test — the configured 400 ms schedule, never the 2 s default.
    let logged = errors[0]["retry_in_ms"]
        .as_u64()
        .expect("a wait was recorded");
    assert!(
        logged <= 400,
        "the configured 400 ms schedule was not used: {logged} ms"
    );
    assert!(
        waited < Duration::from_secs(30),
        "the run took {waited:?}; the wait itself was {logged} ms"
    );
}

/// A `Retry-After` in HTTP-date form is honoured too: the date is two seconds
/// out, so the wait is clearly longer than the 400 ms ceiling a hintless
/// schedule gets in the test config.
#[tokio::test]
async fn a_retry_after_date_is_understood() {
    let when = std::time::SystemTime::now() + Duration::from_secs(2);
    let date = http_date(when);
    let mock = Mock::start(vec![
        Reply::HttpWith(429, vec![("Retry-After".into(), date)], "busy".into()),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    let started = Instant::now();
    let run = run_task(&mock, &["run", "--json", "try"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(provider_errors(&run.events).len(), 1);
    assert_eq!(run.result()["outcome"], "done");
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "the date was not honoured: {:?}",
        started.elapsed()
    );
}

/// "Sun, 06 Nov 1994 08:49:37 GMT": the HTTP-date form `Retry-After` allows.
fn http_date(at: std::time::SystemTime) -> String {
    let secs = at
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let at = time::OffsetDateTime::from_unix_timestamp(secs as i64).expect("a valid instant");
    let format = time::format_description::parse_borrowed::<2>(
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT",
    )
    .expect("a valid format");
    at.format(&format).expect("a formattable instant")
}

/// A 5xx is retried exactly like a 429.
#[tokio::test]
async fn a_5xx_is_retried() {
    let mock = Mock::start(vec![
        Reply::Http(503, "upstream is busy".into()),
        Reply::Sse(sse(&text_turn("fine now", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "try"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let errors = provider_errors(&run.events);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["retryable"], true);
    assert!(errors[0]["message"].as_str().unwrap().contains("503"));
    assert_eq!(run.result()["final_text"], "fine now");
}

/// A 400/401/403/404 is an answer, not an outage: no retry, and the turn fails
/// as it always has (the model was wrong, the key is bad — another request
/// buys the same answer).
#[tokio::test]
async fn a_4xx_refusal_is_not_retried() {
    for status in [400u16, 401, 403, 404] {
        let mock = Mock::start(vec![
            Reply::Http(status, "refused".into()),
            Reply::Sse(sse(&text_turn("never asked", 0))),
        ])
        .await;
        let run = run_task(&mock, &["run", "--json", "try"], &[], None, "").await;
        assert_eq!(run.code, 1, "{status}: stderr {}", run.stderr);
        assert_eq!(mock.request_count(), 1, "{status} was retried");
        let errors = provider_errors(&run.events);
        assert_eq!(errors.len(), 1, "{status}: {:#?}", run.events);
        assert_eq!(errors[0]["retryable"], false, "{status}");
        assert_eq!(run.one_event("turn_ended")["outcome"], "error", "{status}");
    }
}

/// A stream that dies *before any tool call of the step began* is re-sent from
/// the same fold: no side effect happened, so the request simply goes again.
#[tokio::test]
async fn a_mid_stream_disconnect_before_tools_is_retried() {
    let partial = format!("data: {}\n\n", delta("half an answer"));
    let mock = Mock::start(vec![
        Reply::Disconnect(partial),
        Reply::Sse(sse(&text_turn("the whole answer", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "try"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(provider_errors(&run.events).len(), 1);
    assert_eq!(mock.request_count(), 2);
    assert_eq!(run.events_of("assistant").len(), 1);
    assert_eq!(run.result()["final_text"], "the whole answer");
}

/// A stream that dies *after* a tool call had begun is not re-sent: that step
/// may already have had an effect. The turn fails and the tool call is settled
/// as an unknown outcome (M3's rule), never re-run.
#[tokio::test]
async fn a_mid_stream_disconnect_after_a_tool_call_is_not_retried() {
    // A complete tool-call delta, then the socket dies before the finish.
    let mut chunks = tool_call_chunks("call_1", "bash", &json!({"command": "printf 'x\n'"}));
    chunks.pop();
    let partial = format!("data: {}\n\n", chunks[0]);
    let mock = Mock::start(vec![
        Reply::Disconnect(partial),
        Reply::Sse(sse(&text_turn("never used", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "try"], &[], None, "").await;
    assert_eq!(run.code, 1, "stderr: {}", run.stderr);
    assert_eq!(
        mock.request_count(),
        1,
        "a step whose tool call began was re-sent"
    );
    assert_eq!(provider_errors(&run.events).len(), 1);
    assert_eq!(run.one_event("turn_ended")["outcome"], "error");
    // No tool ever ran: a half-read call is not a request the next step could
    // make, and nothing was started.
    assert!(run.events_of("tool_started").is_empty());
}

/// `stop` during a wait ends the turn **stopped**, at once, not after the wait.
/// The hint is an hour, so "at once" and "waited it out" cannot be confused on
/// a slow runner the way a 30 s hint can.
#[tokio::test]
async fn stop_during_a_wait_ends_the_turn_stopped() {
    let mock = Mock::start(vec![
        Reply::HttpWith(
            429,
            vec![("Retry-After".into(), "3600".into())],
            "slow down".into(),
        ),
        Reply::Sse(sse(&text_turn("never asked", 0))),
    ])
    .await;
    // A budget that could wait the hour out: only the `stop` ends it, so a
    // paused turn here would mean the budget answered instead of the verb.
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "retry_max_wait = 7200\n",
    )
    .await;
    let error = session.wait_for("error").await;
    assert_eq!(error["retryable"], true, "{error}");

    let started = Instant::now();
    let answer = session.ctl(&["stop", "no time for this"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let ended = session.wait_for("turn_ended").await;
    let waited = started.elapsed();
    assert_eq!(
        ended["outcome"], "stopped",
        "a stopped wait is not an error: {ended}"
    );
    // The bound separates "at once" from "waited the hour out"; it is not a
    // latency assertion, because starting a `kelir ctl` process is part of it.
    assert!(
        waited < Duration::from_secs(20),
        "the stop waited {waited:?} for the Retry-After"
    );
    assert_eq!(mock.request_count(), 1, "the retry was sent after the stop");
    let (code, stderr) = session.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// `model` during a wait: the retry uses the new model.
#[tokio::test]
async fn model_during_a_wait_retries_on_the_new_model() {
    let mock = Mock::start(vec![
        Reply::HttpWith(
            429,
            vec![("Retry-After".into(), "5".into())],
            "slow down".into(),
        ),
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
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done", "{ended}");

    let requests = mock.requests();
    let last = requests.last().expect("the retried request");
    assert_eq!(
        last.body["model"], "go/model",
        "the retry did not use the swapped model"
    );
    assert_eq!(requests[0].body["model"], "test/model");
    session.end().await;
}

/// The waiting budget spent, the turn **pauses**: outcome `paused`, a message
/// that says how to go on, and a later `say` retries from the same fold.
#[tokio::test]
async fn budget_exhaustion_pauses_and_a_say_retries() {
    let mock = Mock::start(vec![
        Reply::Http(429, "slow down".into()),
        Reply::Http(429, "slow down".into()),
        Reply::Http(429, "slow down".into()),
        Reply::Sse(sse(&text_turn("back at last", 0))),
    ])
    .await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "retry_base_ms = 50\nretry_cap_ms = 100\nretry_max_attempts = 3\nretry_max_wait = 5\n",
    )
    .await;
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "paused", "{ended}");
    let message = ended["final_text"].as_str().unwrap();
    assert!(
        message.contains("provider unavailable after 3 attempts"),
        "{message}"
    );
    assert!(message.contains("say continue to retry"), "{message}");
    assert_eq!(mock.request_count(), 3);
    // The last error of the episode says "gave up": no `retry_in_ms`, because
    // there is no next attempt.
    let log = session.log();
    let errors = provider_errors(&log);
    assert_eq!(errors.len(), 3, "{errors:#?}");
    assert_eq!(errors[2]["attempt"], 3);
    assert!(
        errors[2].get("retry_in_ms").is_none(),
        "the last error promised a retry: {}",
        errors[2]
    );

    // A later `say` starts a new turn and retries from the same fold.
    let answer = session.ctl(&["say", "continue"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done", "{ended}");
    assert_eq!(mock.request_count(), 4);
    let (code, stderr) = session.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// A response body never reaches the wait's visible line: only the status and a
/// short reason do, so a credential a provider echoes in a body cannot land on
/// a person's screen.
#[tokio::test]
async fn a_response_body_never_reaches_the_wait_line() {
    let mock = Mock::start(vec![
        Reply::Http(
            429,
            "{\"error\":\"bad key sk-secret-should-not-leak\"}".into(),
        ),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    // No `--json`: the human output is the one a person reads. Every event
    // still has to be checked in the log afterwards.
    let run = run_task(&mock, &["run", "try"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let errors = provider_errors(&run.events);
    assert_eq!(errors.len(), 1);
    let message = errors[0]["message"].as_str().unwrap();
    assert!(message.contains("429"), "{message}");
    assert!(
        !run.stderr.contains("sk-secret"),
        "the wait line leaked the body: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("rate limited (429)"),
        "the wait line is missing: {}",
        run.stderr
    );
    assert!(run.stderr.contains("say stop to cancel"), "{}", run.stderr);
}

/// The countdown reaches the screen: the wait's line names the attempt and the
/// wait, and a person is told how to cancel it.
#[tokio::test]
async fn the_wait_line_says_what_and_how_long() {
    let mock = Mock::start(vec![
        Reply::HttpWith(
            429,
            vec![("Retry-After".into(), "1".into())],
            "slow down".into(),
        ),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "try"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        run.stderr
            .contains("rate limited (429): retrying in 1 s, attempt 1, say stop to cancel"),
        "the wait line: {}",
        run.stderr
    );
}

/// Two kelir processes on one account, `max_concurrent_requests = 1`: their
/// provider requests never overlap. The slot is an OS file lock, so a holder
/// that crashes or is killed leaks nothing: the kernel releases it and the next
/// process takes it. (That is the property a pid file does not have; the
/// governor's own unit test proves the lock itself, and this one proves the
/// session honours it across processes.)
///
/// The first request is held open by the test's gate, so this is about the
/// governor and not about how long a process takes to start.
#[tokio::test]
async fn two_processes_never_overlap_a_provider_request() {
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Gated(gate.clone(), sse(&text_turn("first", 0))),
        Reply::Sse(sse(&text_turn("second", 0))),
    ])
    .await;
    // One state dir shared by both: that is where the slots are, and that is
    // what "across local kelir processes" means. The knob is per provider —
    // one account has one limit.
    let config = "[providers.openrouter]
max_concurrent_requests = 1
";
    let mut first = LiveSession::start(&mock, &["run", "--keep", "--json", "one"], config).await;
    first.wait_for("step_started").await;
    // The first request is in the mock's hands now (the gate holds it), so the
    // first session certainly holds the slot.
    wait_until(CEILING, || mock.request_count() >= 1).await;

    let mut second = LiveSession::start_reusing(
        first.state.clone(),
        &mock,
        &["run", "--keep", "--json", "two"],
        config,
    )
    .await;
    second.wait_for("step_started").await;
    // The second session has built its request and is waiting for the slot, so
    // nothing reaches the mock while the first held it. Give it far longer than
    // the slot poll needs: a request it *could* send would arrive in ~100 ms.
    let deadline = Instant::now() + Duration::from_millis(1_200);
    while Instant::now() < deadline {
        assert_eq!(
            mock.request_count(),
            1,
            "the second request was sent before the slot came free"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Release the first; the second may go in now.
    gate.notify_one();
    let ended = first.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done", "{ended}");
    let ended = second.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done", "{ended}");
    assert_eq!(mock.max_concurrent(), 1, "the requests overlapped");
    assert_eq!(mock.request_count(), 2);
    // `end` for both, then wait: a `--keep` session otherwise sits in idle for
    // ever, and the harness's guard would have to kill it.
    assert_eq!(first.ctl(&["end"]).await.code, 0, "first end");
    assert_eq!(second.ctl(&["end"]).await.code, 0, "second end");
    let _ = first.finish().await;
    let _ = second.finish().await;
}

/// Poll `check` until it holds or `limit` passes. The calls are condition-based
/// rather than sleep-based on purpose: a busy dev box can make any fixed sleep
/// a coin flip.
async fn wait_until(limit: Duration, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the condition did not hold within {limit:?}");
}

/// Without the knob, nothing is serialised: two sessions on one account are
/// free to be in flight together (the default must not surprise anyone).
#[tokio::test]
async fn without_the_knob_requests_are_not_serialised() {
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Gated(gate.clone(), sse(&text_turn("first", 0))),
        Reply::Sse(sse(&text_turn("second", 0))),
    ])
    .await;
    let mut first = LiveSession::start(&mock, &["run", "--keep", "--json", "one"], "").await;
    first.wait_for("step_started").await;
    let mut second = LiveSession::start_reusing(
        first.state.clone(),
        &mock,
        &["run", "--keep", "--json", "two"],
        "",
    )
    .await;
    second.wait_for("step_started").await;
    // With no governor the second request goes out while the first is still
    // held: two in flight, which is the default.
    wait_until(CEILING, || mock.request_count() >= 2).await;
    assert_eq!(
        mock.max_concurrent(),
        2,
        "the governor was on without being asked for"
    );
    gate.notify_one();
    let _ = first.wait_for("turn_ended").await;
    let _ = second.wait_for("turn_ended").await;
    assert_eq!(first.ctl(&["end"]).await.code, 0, "first end");
    assert_eq!(second.ctl(&["end"]).await.code, 0, "second end");
    let _ = first.finish().await;
    let _ = second.finish().await;
}
/// A slot holder that dies without cleaning up leaks nothing: the OS releases
/// the file lock when the process ends, so the next kelir takes the slot
/// without waiting for anything (no stale pid file to notice).
#[tokio::test]
async fn a_killed_slot_holder_frees_the_slot() {
    let mock = Mock::start(vec![
        Reply::SseAfter(Duration::from_millis(5_000), sse(&text_turn("never", 0))),
        Reply::Sse(sse(&text_turn("after the kill", 0))),
    ])
    .await;
    let config = "[providers.openrouter]\nmax_concurrent_requests = 1\n";
    let mut holder = LiveSession::start(&mock, &["run", "--keep", "--json", "hold"], config).await;
    holder.wait_for("step_started").await;
    // The holder is inside its 5 s request: it holds the only slot.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(mock.request_count(), 1);

    // Kill it the way a crash would: the process dies, its handle closes, and
    // the kernel drops the lock. No `end`, no `stop`, no cleanup.
    holder.kill().await;

    let mut next = LiveSession::start_reusing(
        holder.state.clone(),
        &mock,
        &["run", "--keep", "--json", "next"],
        config,
    )
    .await;
    let ended = next.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done", "{ended}");
    let done = next
        .events
        .iter()
        .find(|event| event["type"] == "assistant")
        .expect("the answer");
    assert_eq!(done["text"], "after the kill");
    assert_eq!(next.ctl(&["end"]).await.code, 0, "end");
    let _ = next.finish().await;
}

// ------------------------------------------------------------ interruptibility

/// `say --now` during a wait does not wait it out: the message is in the fold
/// the moment it arrives, and the request is re-sent at once (STEER-5 review
/// B2). The wait's budget was worth ten minutes here, so only the verb can end
/// it early.
#[tokio::test]
async fn say_now_during_a_wait_retries_at_once_with_the_message() {
    let mock = Mock::start(vec![
        Reply::HttpWith(
            429,
            vec![("Retry-After".into(), "3600".into())],
            "slow down".into(),
        ),
        Reply::Sse(sse(&text_turn("on the new plan", 0))),
    ])
    .await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "retry_max_wait = 7200\n",
    )
    .await;
    session.wait_for("error").await;
    let started = Instant::now();
    let answer = session.ctl(&["say", "--now", "forget that, do this"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    assert_eq!(
        answer.response()["applies"],
        "now",
        "`say --now` cannot answer `now` and then wait 30 s"
    );
    let ended = session.wait_for("turn_ended").await;
    assert!(started.elapsed() < Duration::from_secs(20), "waited it out");
    assert_eq!(ended["outcome"], "done", "{ended}");
    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "the retry is one request");
    assert!(
        requests[1]
            .body
            .to_string()
            .contains("forget that, do this"),
        "the retried request does not carry the message"
    );
    let _ = session.ctl(&["end"]).await;
    let _ = session.finish().await;
}

/// `model` during a wait: the retry goes out at once, on the new model, with a
/// fresh budget and the new provider's governor (STEER-5 review B2 + S1).
#[tokio::test]
async fn model_during_a_wait_retries_at_once_on_the_new_model() {
    let mock = Mock::start(vec![
        Reply::HttpWith(
            429,
            vec![("Retry-After".into(), "3600".into())],
            "slow down".into(),
        ),
        Reply::Sse(sse(&text_turn("swapped", 0))),
    ])
    .await;
    let config = "retry_max_wait = 7200\n[providers.opencode-go]\nmax_concurrent_requests = 1\n";
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], config).await;
    session.wait_for("error").await;
    let started = Instant::now();
    let answer = session.ctl(&["model", "opencode-go/go/model"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    // The ack is answered after the swap landed, so it says what really
    // happened: the verb applied now and `state.model` is the new model
    // (STEER-5 review N1).
    assert_eq!(answer.response()["applies"], "now", "{}", answer.stdout);
    assert_eq!(
        answer.response()["state"]["model"],
        "go/model",
        "{}",
        answer.stdout
    );
    let ended = session.wait_for("turn_ended").await;
    assert!(started.elapsed() < Duration::from_secs(20), "waited it out");
    assert_eq!(ended["outcome"], "done", "{ended}");
    let requests = mock.requests();
    assert_eq!(requests[1].body["model"], "go/model");
    // The provider the session swapped to is governed: its request took a slot.
    let locks = session.state.join("locks");
    let used = std::fs::read_dir(&locks)
        .map(|dir| dir.count())
        .unwrap_or(0);
    assert!(used > 0, "the swapped-to provider took no slot ({locks:?})");
    let _ = session.ctl(&["end"]).await;
    let _ = session.finish().await;
}

/// A Ctrl-C during a wait ends the turn `stopped` — the same answer the `stop`
/// verb gets — instead of killing a session whose log says nothing (STEER-5
/// review B2). A `--keep` session stays up for the next message.
///
/// Unix only, and by `SIGINT` to the session's own pid: a console control event
/// on Windows addresses a whole console, so a test that sends one kills the
/// test runner too (AGENTS.md; it did, 2026-11-19). The Windows side of the path
/// is `signals::interrupted`'s registration, which the module's unit test
/// polls, and the wait branch it feeds, which `stop` covers on every platform.
#[cfg(unix)]
#[tokio::test]
async fn an_interrupt_during_a_wait_ends_the_turn_stopped() {
    let mock = Mock::start(vec![
        Reply::HttpWith(
            429,
            vec![("Retry-After".into(), "3600".into())],
            "slow down".into(),
        ),
        Reply::Sse(sse(&text_turn("still here", 0))),
    ])
    .await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "retry_max_wait = 7200\n",
    )
    .await;
    session.wait_for("error").await;
    let started = Instant::now();
    session.interrupt().await;
    let ended = session.wait_for("turn_ended").await;
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the interrupt waited out the Retry-After"
    );
    assert_eq!(
        ended["outcome"], "stopped",
        "an interrupted wait is a stop, not an error: {ended}"
    );
    // A `--keep` session is still up and takes the next message.
    let answer = session.ctl(&["say", "carry on"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done", "{ended}");
    let _ = session.ctl(&["end"]).await;
    let _ = session.finish().await;
}

/// The lost-SIGINT stress test (STEER-5 fixes round 2).
///
/// The bug was a lost wakeup, not load: `signals::interrupted` was edge
/// triggered, so an interrupt that arrived *before* the wait that would act on
/// it was gone for ever (`watch::Sender::send` drops its value when nothing is
/// subscribed, and a later subscriber took the bumped value as its baseline).
/// The e2e above passed alone and failed about one run in three under load,
/// because the `error` event and the wait's first poll are far apart enough for
/// a `kill -INT` to land in between.
///
/// This drives that window deliberately instead of hoping for it: the mock
/// holds the 429 at a gate, so the session cannot have entered a wait yet, the
/// test signals, then opens the gate. `signals::interrupted` must answer from
/// the level alone, because no wait was parked when the signal arrived. The
/// loop is there for the *other* half — a wait entered after an interrupt was
/// already consumed by an earlier wait — and to catch a fix that wins by luck.
///
/// Unix only: a console control event on Windows addresses a whole console
/// (AGENTS.md hard rule), so the Windows half is `signals`' own unit tests.
#[cfg(unix)]
#[tokio::test]
async fn an_interrupt_before_the_wait_begins_is_not_lost() {
    const ROUNDS: usize = 20;
    let mut late = Vec::new();
    for round in 0..ROUNDS {
        let gate = std::sync::Arc::new(tokio::sync::Notify::new());
        let mock = Mock::start(vec![Reply::GatedHttpWith(
            429,
            vec![("Retry-After".into(), "3600".into())],
            "slow down".into(),
            gate.clone(),
        )])
        .await;
        let mut session = LiveSession::start(
            &mock,
            &["run", "--keep", "--json", "go"],
            "retry_max_wait = 7200\n",
        )
        .await;
        // The session is waiting on the mock, so it has not entered a retry
        // wait and no waiter is parked: an interrupt now can only be seen
        // through the level.
        let started = Instant::now();
        session.interrupt().await;
        // Let the handler run before the 429 creates the wait that should see
        // it. Without this the two orders race and the test would pass on the
        // old code half the time; with it, the old code never sees the signal.
        tokio::time::sleep(Duration::from_millis(150)).await;
        gate.notify_one();
        let deadline = tokio::time::Instant::now() + CEILING;
        let ended = tokio::time::timeout_at(deadline, session.wait_for("turn_ended")).await;
        let elapsed = started.elapsed();
        match ended {
            Ok(event) => {
                assert_eq!(
                    event["outcome"], "stopped",
                    "round {round}: an interrupt is a stop, not {:?}",
                    event["outcome"]
                );
                if elapsed > Duration::from_secs(2) {
                    late.push(elapsed);
                }
            }
            _ => panic!(
                "round {round}: the interrupt was lost; the turn never ended (seen: {:?}, \
                 stderr: {})",
                session
                    .events
                    .iter()
                    .map(|event| event["type"].clone())
                    .collect::<Vec<_>>(),
                session.stderr()
            ),
        }
        let _ = session.ctl(&["end"]).await;
        let _ = session.finish().await;
        // No `signals::DOUBLE` wait between rounds: the double-interrupt window
        // is per *process* and every round starts a new session.
    }
    // "Immediately" (STEER-5 / review B2) is two seconds: a round that needed
    // longer is a lost wakeup that a poll interval papered over.
    assert!(
        late.is_empty(),
        "{} of {ROUNDS} rounds took over 2 s to stop: {late:?}",
        late.len()
    );
}
