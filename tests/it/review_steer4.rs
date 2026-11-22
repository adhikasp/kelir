//! Reviewer probes for STEER-4 (no step limit by default; an explicit cap
//! pauses the turn), `docs/reviews/steer4-review.md`.
//!
//! These go past the implementer's e2e: the boundary of the cap (a turn that
//! *finishes* on the cap-th step is `done`, not `paused`), `final_text` after a
//! step that talked, the stopgap `4294967295` and `0` in the file, bad values
//! on the flag, a repeated pause/resume chain, `--max-steps` on `resume`/`ask`,
//! the cache prefix across a pause, and what a verb's `state` says after one.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::*;
use serde_json::{Value, json};

fn edit_step(index: usize) -> Vec<Value> {
    tool_turn(
        &format!("call_{index}"),
        "edit",
        &json!({"path": format!("f{index}.txt"), "old": "", "new": format!("{index}\n")}),
    )
}

/// A step that says something *and* calls a tool.
fn talking_edit_step(index: usize, text: &str) -> Vec<Value> {
    let mut chunks = vec![delta(text)];
    chunks.extend(tool_call_chunks(
        &format!("call_{index}"),
        "edit",
        &json!({"path": format!("f{index}.txt"), "old": "", "new": "x\n"}),
    ));
    chunks.push(finish("tool_calls"));
    chunks.push(usage_chunk(100, 12, 0, 100));
    chunks
}

fn outcomes(log: &[Value]) -> Vec<String> {
    log.iter()
        .filter(|event| event["type"] == "turn_ended")
        .map(|event| event["outcome"].as_str().unwrap_or("").to_string())
        .collect()
}

/// A turn whose cap-th step is the model's final answer is `done`: the cap
/// counts steps *started*, so reaching exactly the cap with an answer must not
/// turn a finished turn into a pause.
#[tokio::test]
async fn a_turn_that_answers_on_the_capth_step_is_done_not_paused() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&edit_step(1))),
        Reply::Sse(sse(&edit_step(2))),
        Reply::Sse(sse(&text_turn("finished on the cap", 0))),
    ])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "task"],
        &[],
        None,
        "max_steps = 3\n",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.one_event("turn_ended")["outcome"], "done");
    assert_eq!(run.result()["outcome"], "done");
    assert!(
        !run.stderr.contains("paused"),
        "a finished turn is not a pause: {}",
        run.stderr
    );

    // A cap of 1 and an immediate answer: also done.
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("instant", 0)))]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "--max-steps", "1", "task"],
        &[],
        None,
        "",
    )
    .await;
    assert_eq!(run.one_event("turn_ended")["outcome"], "done");
}

/// The last thing the model said survives into `turn_ended.final_text` on a
/// pause, and the pause lands *after* the capped step's tool result, never
/// between a call and its result.
#[tokio::test]
async fn a_pause_keeps_the_last_words_and_never_splits_a_call_from_its_result() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&edit_step(1))),
        Reply::Sse(sse(&talking_edit_step(2, "halfway there"))),
        Reply::Sse(sse(&text_turn("never asked", 0))),
    ])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "task"],
        &[],
        None,
        "max_steps = 2\n",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let ended = run.one_event("turn_ended");
    assert_eq!(ended["outcome"], "paused");
    assert_eq!(ended["final_text"], "halfway there");
    // Every started call settled before the pause: 2 started, 2 ended.
    assert_eq!(run.events_of("call_started").len(), 2);
    assert_eq!(run.events_of("call_settled").len(), 2);
    let types = run.event_types();
    let last_tool_end = types.iter().rposition(|t| t == "call_settled").unwrap();
    let turn_end = types.iter().rposition(|t| t == "turn_ended").unwrap();
    assert!(last_tool_end < turn_end, "{types:?}");
}

/// The stopgap value the maintainer's config has, and `0`, both run a normal
/// turn with no cap; the stopgap is `Some(u32::MAX)`, not an arithmetic
/// overflow at `turn_steps > cap`.
#[tokio::test]
async fn the_stopgap_and_zero_in_the_file_run_a_turn_to_its_end() {
    for config in ["max_steps = 4294967295\n", "max_steps = 0\n"] {
        let mock = Mock::start(vec![
            Reply::Sse(sse(&edit_step(1))),
            Reply::Sse(sse(&edit_step(2))),
            Reply::Sse(sse(&text_turn("ok", 0))),
        ])
        .await;
        let run = run_task(&mock, &["run", "--json", "task"], &[], None, config).await;
        assert_eq!(run.code, 0, "{config}: {}", run.stderr);
        assert_eq!(run.one_event("turn_ended")["outcome"], "done", "{config}");
        assert_eq!(run.events_of("step_started").len(), 3, "{config}");
    }
}

/// A negative or non-numeric config value is refused with a usage error that
/// names the key, and nothing reaches the provider.
#[tokio::test]
async fn a_bad_max_steps_in_the_file_is_a_usage_error() {
    for config in [
        "max_steps = -1\n",
        "max_steps = \"five\"\n",
        "max_steps = 1.5\n",
    ] {
        let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("never", 0)))]).await;
        let run = run_task(&mock, &["run", "--json", "task"], &[], None, config).await;
        assert_ne!(run.code, 0, "{config} must be refused");
        assert!(run.stderr.contains("max_steps"), "{config}: {}", run.stderr);
        assert_eq!(mock.request_count(), 0, "{config}");
    }
}

/// Bad values on the flag are clap usage errors (exit 2), not a silent cap.
#[tokio::test]
async fn bad_values_on_the_flag_are_refused() {
    for bad in ["-1", "abc", "4294967296", "1.5", ""] {
        let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("never", 0)))]).await;
        let run = run_task(
            &mock,
            &["run", "--json", "--max-steps", bad, "task"],
            &[],
            None,
            "",
        )
        .await;
        assert_ne!(run.code, 0, "--max-steps {bad:?} must be refused");
        assert_eq!(mock.request_count(), 0, "--max-steps {bad:?}");
    }
}

/// The flag also reaches the other session commands. `resume --max-steps` caps
/// the resumed turn with a fresh count, so a long job can be walked forward N
/// steps at a time: pause, resume, pause, resume, done.
#[tokio::test]
async fn pause_resume_pause_resume_walks_a_long_job_forward() {
    let mut script = Vec::new();
    for index in 1..=5 {
        script.push(Reply::Sse(sse(&edit_step(index))));
    }
    script.push(Reply::Sse(sse(&text_turn("all done", 0))));
    let mock = Mock::start(script).await;
    let sandbox = Sandbox::new("max_steps = 2\n");

    let first = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "task"], &[])
        .await;
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert_eq!(first.result().unwrap()["outcome"], "paused");
    let id = sandbox.only_session();

    // The config cap (2) applies to the resumed turn; the flag overrides it.
    let second = sandbox
        .kelir(&mock.base_url(), &["resume", "--json", &id], &[])
        .await;
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(second.result().unwrap()["outcome"], "paused");
    assert!(
        second.stderr.contains("paused after 2 steps"),
        "{}",
        second.stderr
    );

    let third = sandbox
        .kelir(
            &mock.base_url(),
            &["resume", "--json", "--max-steps", "0", &id],
            &[],
        )
        .await;
    assert_eq!(third.code, 0, "{}", third.stderr);
    assert_eq!(
        third.result().unwrap()["outcome"],
        "done",
        "{}",
        third.stderr
    );

    let log = sandbox.log(&id);
    assert_eq!(outcomes(&log), vec!["paused", "paused", "done"]);
    let steps: Vec<u64> = log
        .iter()
        .filter(|event| event["type"] == "step_started")
        .map(|event| event["step"].as_u64().unwrap())
        .collect();
    assert_eq!(steps, vec![1, 2, 3, 4, 5, 6], "no step repeated or skipped");
    // Each file edit ran exactly once: a resume never re-ran a settled call.
    assert_eq!(
        log.iter()
            .filter(|event| event["type"] == "call_started")
            .count(),
        5
    );

    // The resumed request continues the transcript: it ends on a tool result,
    // and is an append-only extension of the paused turn's last request.
    let requests = mock.requests();
    assert_eq!(requests.len(), 6);
    for pair in requests.windows(2) {
        let before = pair[0].messages();
        let after = pair[1].messages();
        assert!(after.len() > before.len());
        assert_eq!(
            &after[..before.len()],
            before,
            "the cache prefix must survive a pause and a resume"
        );
    }
    assert_eq!(requests[2].roles().last(), Some(&"tool"));
}

/// `ask` takes the flag too; a one-shot question with a cap of 1 still answers.
#[tokio::test]
async fn ask_accepts_the_flag() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("an answer", 0)))]).await;
    let run = run_task(
        &mock,
        &["ask", "--max-steps", "1", "a question"],
        &[],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("an answer"), "{}", run.stdout);
}

/// After a pause a `--keep` session reports the finished turn's count against
/// the cap, a bare `stop` while paused is dropped (not carried into the next
/// turn, which would stop it at its first boundary), and the next `say` runs.
#[tokio::test]
async fn a_paused_keep_session_reports_its_state_and_ignores_a_stray_stop() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&edit_step(1))),
        Reply::Sse(sse(&edit_step(2))),
        Reply::Sse(sse(&edit_step(3))),
        Reply::Sse(sse(&text_turn("resumed fine", 0))),
    ])
    .await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "task"],
        "max_steps = 2\n",
    )
    .await;
    assert_eq!(session.wait_for("turn_ended").await["outcome"], "paused");

    let stop = session.ctl(&["stop"]).await;
    let state = &stop.response()["state"];
    assert_eq!(state["max_steps"], 2);

    let said = session.ctl(&["say", "continue"]).await;
    assert_eq!(said.code, 0, "{}{}", said.stdout, said.stderr);
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(
        ended["outcome"], "done",
        "step 3 (an edit) and step 4 (the answer) fit a fresh cap of 2; got {ended}"
    );
    let log = session.log();
    assert_eq!(outcomes(&log), vec!["paused", "done"]);
    assert_eq!(
        log.iter()
            .filter(|event| event["type"] == "step_started")
            .count(),
        4,
        "the stray stop stopped nothing"
    );
    session.end().await;
}

/// FINDING S1 (fixed): after a pause `state.turn_steps` is the number of steps
/// that ran, not cap + 1 (3 of 2). The cap test is now before the increment.
#[tokio::test]
async fn state_after_a_pause_counts_the_steps_that_ran() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&edit_step(1))),
        Reply::Sse(sse(&edit_step(2))),
        Reply::Sse(sse(&text_turn("never", 0))),
    ])
    .await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "task"],
        "max_steps = 2
",
    )
    .await;
    assert_eq!(session.wait_for("turn_ended").await["outcome"], "paused");
    let state = session.ctl(&["stop"]).await.response()["state"].clone();
    assert_eq!(state["turn_steps"], 2, "two steps ran, not 3 of 2: {state}");
    session.end().await;
}
