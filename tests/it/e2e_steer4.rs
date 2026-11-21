//! STEER-4 end-to-end: no step limit by default, and an explicit cap pauses the
//! turn instead of failing it.
//!
//! The real binary runs against the scripted mock. The no-limit test drives a
//! long turn (150 tool steps) that the old default of 100 would have failed; the
//! cap tests assert on the `paused` outcome in the log, on the NDJSON `result`,
//! on the exit code and on the requests the mock received.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::*;
use serde_json::{Value, json};

fn of_type<'a>(log: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|event| event["type"] == event_type)
        .collect()
}

/// One tool call the mock scripts, cheap for the loop: `edit` creates a
/// numbered file in the session's cwd (no process, no shim).
fn edit_step(index: usize) -> Vec<Value> {
    tool_turn(
        &format!("call_{index}"),
        "edit",
        &json!({"path": format!("f{index}.txt"), "old": "", "new": format!("{index}\n")}),
    )
}

/// 150 steps complete with no cap at all. The old default was 100, so this is
/// the dogfood failure (M5-A: `kelir: step limit 100 reached`) that made the
/// default fatal.
#[tokio::test]
async fn a_150_step_turn_completes_with_no_limit() {
    let mut script = Vec::new();
    for index in 1..=150 {
        script.push(Reply::Sse(sse(&edit_step(index))));
    }
    script.push(Reply::Sse(sse(&text_turn("all 150 done", 0))));
    let mock = Mock::start(script).await;

    let run = run_task(&mock, &["run", "--json", "long task"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        !run.stderr.contains("step limit"),
        "no cap must not stop the turn: {}",
        run.stderr
    );
    assert_eq!(run.one_event("turn_ended")["outcome"], "done");
    assert_eq!(run.events_of("step_started").len(), 151);
    assert_eq!(mock.request_count(), 151);
    let result = run.result();
    assert_eq!(result["outcome"], "done");
    assert_eq!(result["steps"], 151);
    assert_eq!(result["exit_code"], 0);

    // The 150 files the edit calls created are all there: the turn really ran
    // every step, none was cut off.
    assert!(run.cwd.join("f150.txt").is_file());
}

/// An explicit cap of 5 pauses the turn: outcome `paused` in the log and in the
/// NDJSON result, exit code 0 (a pause is not a failure), and the message the
/// maintainer asked for.
#[tokio::test]
async fn an_explicit_cap_pauses_the_turn_and_exits_zero() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&edit_step(1))),
        Reply::Sse(sse(&edit_step(2))),
        Reply::Sse(sse(&edit_step(3))),
        Reply::Sse(sse(&edit_step(4))),
        Reply::Sse(sse(&edit_step(5))),
        Reply::Sse(sse(&text_turn("never asked", 0))),
    ])
    .await;

    let run = run_task(
        &mock,
        &["run", "--json", "long task"],
        &[],
        None,
        "max_steps = 5\n",
    )
    .await;
    assert_eq!(
        run.code, 0,
        "a paused turn is not a failure: {}",
        run.stderr
    );
    assert!(
        run.stderr
            .contains("turn paused after 5 steps; say continue to resume"),
        "stderr: {}",
        run.stderr
    );

    // Exactly the cap's steps ran; the sixth was never asked for.
    assert_eq!(run.events_of("step_started").len(), 5);
    assert_eq!(mock.request_count(), 5);

    let ended = run.one_event("turn_ended");
    assert_eq!(ended["outcome"], "paused");
    // Nothing was said in these steps, so `final_text` is empty, as it is for a
    // stopped turn between calls.
    assert_eq!(ended["final_text"], "");
    assert_eq!(run.one_event("session_ended")["reason"], "paused");

    let result = run.result();
    assert_eq!(result["outcome"], "paused");
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["steps"], 5);
}

/// A `--keep` session whose turn paused stays alive, and the next `say` opens a
/// new turn with a fresh count: the session-wide step numbers keep counting,
/// the turn's own count starts again at 1.
#[tokio::test]
async fn after_a_pause_the_next_say_starts_a_fresh_count() {
    let mut script = Vec::new();
    for index in 1..=5 {
        script.push(Reply::Sse(sse(&edit_step(index))));
    }
    // The second turn: three steps — under the cap of 5, but steps 7, 8 and 9
    // session-wide, so a count that did not reset would have paused at once.
    for index in 6..=8 {
        script.push(Reply::Sse(sse(&edit_step(index))));
    }
    script.push(Reply::Sse(sse(&text_turn("continued", 0))));
    let mock = Mock::start(script).await;

    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "long task"],
        "max_steps = 5\n",
    )
    .await;
    let ended = session.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "paused");
    assert_eq!(mock.request_count(), 5);

    // The session is still up and answering verbs.
    let answer = session.ctl(&["say", "continue"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);

    let ended = session.wait_for("turn_ended").await;
    assert_eq!(
        ended["outcome"],
        "done",
        "the second turn runs to its answer: {}",
        session.stderr()
    );

    let log = session.log();
    let steps: Vec<u64> = of_type(&log, "step_started")
        .iter()
        .map(|event| event["step"].as_u64().expect("step"))
        .collect();
    assert_eq!(
        steps,
        vec![1, 2, 3, 4, 5, 6, 7, 8, 9],
        "session-wide step numbers stay unique across the pause"
    );
    // Two turns, one paused and one done.
    let outcomes: Vec<&str> = of_type(&log, "turn_ended")
        .iter()
        .map(|event| event["outcome"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(outcomes, vec!["paused", "done"]);
    session.end().await;
}

/// A paused turn is continued by `kelir resume` the same way a stopped one is:
/// the log is picked up, a new turn runs with a fresh count, and the work the
/// paused turn did is still in it.
#[tokio::test]
async fn resume_continues_a_paused_turn() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&edit_step(1))),
        Reply::Sse(sse(&edit_step(2))),
        Reply::Sse(sse(&text_turn("picked it up", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("max_steps = 2\n");
    let first = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "long task"], &[])
        .await;
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert_eq!(first.result().expect("result")["outcome"], "paused");
    let id = sandbox.only_session();

    let second = sandbox
        .kelir(&mock.base_url(), &["resume", "--json", id.as_str()], &[])
        .await;
    assert_eq!(second.code, 0, "{}", second.stderr);
    let result = second.result().expect("result");
    assert_eq!(result["outcome"], "done", "stderr: {}", second.stderr);
    assert_eq!(result["steps"], 3, "the log's step numbers keep counting");

    let log = sandbox.log(&id);
    let outcomes: Vec<&str> = of_type(&log, "turn_ended")
        .iter()
        .map(|event| event["outcome"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(outcomes, vec!["paused", "done"]);
    // The resume marked the log and never re-ran a call.
    assert_eq!(of_type(&log, "resumed").len(), 1);
    assert_eq!(of_type(&log, "tool_started").len(), 2);
}

/// `--max-steps` overrides the file, and `0` there means "no cap" as it does in
/// `config.toml`: the flag can lift a configured cap.
#[tokio::test]
async fn the_flag_overrides_the_config_and_zero_lifts_a_cap() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&edit_step(1))),
        Reply::Sse(sse(&edit_step(2))),
        Reply::Sse(sse(&edit_step(3))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "--max-steps", "0", "task"],
        &[],
        None,
        "max_steps = 2\n",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.one_event("turn_ended")["outcome"], "done");
    assert_eq!(run.events_of("step_started").len(), 4);

    // And the flag lowers a cap the config does not have.
    let mock = Mock::start(vec![
        Reply::Sse(sse(&edit_step(1))),
        Reply::Sse(sse(&text_turn("never asked", 0))),
    ])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "--max-steps", "1", "task"],
        &[],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.one_event("turn_ended")["outcome"], "paused");
    assert_eq!(run.events_of("step_started").len(), 1);
}

/// A verb's `state` reports the turn's own step count and the cap (null when
/// there is none), so a steering human sees a pause coming before it lands.
#[tokio::test]
async fn the_verb_state_reports_the_turn_count_and_the_cap() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &json!({"command": "sleep 1"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let mut session = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "task"],
        "max_steps = 4\n",
    )
    .await;
    session.wait_for("step_started").await;
    let answer = session.ctl(&["say", "keep going"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let state = &answer.response()["state"];
    assert_eq!(state["max_steps"], 4, "the cap is reported");
    assert_eq!(state["turn_steps"], 1, "one step of this turn has started");
    session.wait_for("turn_ended").await;

    // With no cap, the same fields say so rather than inventing a number.
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("done", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "task"], "").await;
    session.wait_for("turn_ended").await;
    let answer = session.ctl(&["say", "again"]).await;
    let state = &answer.response()["state"];
    assert_eq!(state["max_steps"], Value::Null, "no cap is null");
    session.end().await;
}
