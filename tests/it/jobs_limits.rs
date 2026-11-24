//! jobs limits tests.
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::*;
use serde_json::{Value, json};

use super::jobs::{clean, completions, kelir, of_type, status, text};

/// A step cap must settle the cap-th tool call before pausing. Ported from
/// review_m9fix.rs (jobs.toml cap replaced by --max-steps).
#[tokio::test]
async fn a_cap_th_answer_after_a_tool_pauses_after_the_tool_result() {
    let answer = "late answer on the cap";
    let mut capped = tool_call_chunks("c1", "edit", &json!({"path":"f.txt","old":"","new":"x\n"}));
    capped.push(crate::support::finish("tool_calls"));
    capped.extend(text_turn(answer, 0));
    let mock = Mock::start(vec![
        Reply::Sse(sse(&crate::support::tool_turn(
            "c0",
            "edit",
            &json!({"path":"f0.txt","old":"","new":"x\n"}),
        ))),
        Reply::Sse(sse(&capped)),
        Reply::Sse(sse(&text_turn("never asked", 0))),
    ])
    .await;
    let sb = Sandbox::new("");
    let out = kelir(
        &sb,
        &mock,
        &[
            "run",
            "--name",
            "latecap",
            "--single-flight",
            "--max-steps",
            "2",
            "go",
        ],
        &[],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let now = status(&sb, "latecap");
    assert_eq!(now["outcome"], "paused", "{now}");
    assert_eq!(now["steps"], 2, "{now}");
    assert_eq!(completions(&mock).len(), 2, "no third request past the cap");
    let id = sb.only_session();
    let log = sb.log(&id);
    let ended = of_type(&log, "turn_ended").pop().expect("turn_ended");
    assert_eq!(ended["outcome"], "paused");
    assert_eq!(ended["reason"], "steps");
    assert_eq!(ended["final_text"], answer, "the cap-th answer is kept");
    assert_eq!(of_type(&log, "call_settled").len(), 2, "both calls settled");
    let types: Vec<&str> = log
        .iter()
        .map(|event| event["type"].as_str().unwrap_or(""))
        .collect();
    let last_tool = types
        .iter()
        .rposition(|kind| *kind == "call_settled")
        .unwrap();
    let turn_end = types
        .iter()
        .rposition(|kind| *kind == "turn_ended")
        .unwrap();
    assert!(last_tool < turn_end, "never between a call and its result");
    clean(&sb).await;
}

/// The cap is a scheduling rule, not model text: the request prefix is
/// byte-identical to an uncapped run. Ported from review_m9fix.rs.
#[tokio::test]
async fn the_job_cap_is_not_in_the_model_prompt_and_is_not_misreported() {
    let sb = Sandbox::new("");
    let mock = Mock::start(vec![text("finished")]).await;
    let out = kelir(
        &sb,
        &mock,
        &[
            "run",
            "--name",
            "secretcap",
            "--single-flight",
            "--max-steps",
            "1",
            "go",
        ],
        &[],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let paused = status(&sb, "secretcap");
    assert_eq!(paused["outcome"], "paused", "{paused}");
    assert!(
        !serde_json::to_string(&paused)
            .unwrap()
            .contains("job_step_cap")
    );
    assert!(
        paused["error"]
            .as_str()
            .is_some_and(|error| error.to_lowercase().contains("paused")),
        "STATUS says why it paused: {paused}"
    );
    let paused_events = serde_json::to_string(&sb.log(&sb.only_session())).unwrap();
    assert!(
        !paused_events.contains("job_step_cap"),
        "internal name leaked"
    );
    let first = completions(&mock)[0].messages().to_vec();

    // Lift the cap in the SAME sandbox: same session, prompt and paths.
    let mock2 = Mock::start(vec![text("finished again")]).await;
    let out2 = kelir(
        &sb,
        &mock2,
        &["run", "--name", "secretcap", "--single-flight", "go"],
        &[],
    )
    .await;
    assert_eq!(out2.code, 0, "{}", out2.stderr);
    assert_eq!(status(&sb, "secretcap")["outcome"], "done");
    let second = completions(&mock2)[0].messages().to_vec();
    assert_eq!(first[0], second[0], "the system prompt must be identical");
    assert_eq!(
        &second[..first.len()],
        &first[..],
        "cached prefix untouched"
    );
    assert_eq!(first.len(), second.len() - 2, "only tail messages are new");
    let joined = serde_json::to_string(&first).unwrap();
    assert!(!joined.contains("max_steps"), "{joined}");
    assert!(!joined.contains("step cap"), "{joined}");
    assert!(!joined.contains("job_step_cap"), "{joined}");
    clean(&sb).await;
}

/// A `--fresh` run puts carry before the job source, both at the tail.
/// Ported from review_m9fix.rs's rotation test.
#[tokio::test]
async fn a_fresh_run_adds_carry_then_job_after_the_frozen_prefix() {
    let mock = Mock::start(vec![text("the old session learned X"), text("fresh start")]).await;
    let sb = Sandbox::new("");
    assert_eq!(
        kelir(
            &sb,
            &mock,
            &["run", "--name", "rot", "--single-flight", "go"],
            &[]
        )
        .await
        .code,
        0
    );
    let old = sb.only_session();
    assert_eq!(
        kelir(
            &sb,
            &mock,
            &["run", "--name", "rot", "--single-flight", "--fresh", "go"],
            &[]
        )
        .await
        .code,
        0
    );
    let reqs = completions(&mock);
    assert_eq!(reqs.len(), 2);
    let second = reqs[1].messages();
    let tail: Vec<&str> = second[second.len() - 2..]
        .iter()
        .map(|message| message["content"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(tail.len(), 2, "carry, task: {tail:?}");
    assert!(tail[0].starts_with("[source: carry "), "{tail:?}");
    assert!(tail[0].contains("the old session learned X"), "{tail:?}");
    assert_eq!(tail[1], "go");
    let new_id: String = serde_json::from_str::<Value>(
        &std::fs::read_to_string(sb.state.join("names").join("rot")).unwrap(),
    )
    .unwrap()["session"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(new_id, old);
    let new_log = sb.log(&new_id);
    let kinds: Vec<&str> = of_type(&new_log, "source")
        .iter()
        .map(|source| source["kind"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(kinds, vec!["carry"], "the tail source: {kinds:?}");
    clean(&sb).await;
}
