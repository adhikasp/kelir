//! Windows live-drive regressions plus the Linux busy-session kill regression.
#![allow(clippy::pedantic, clippy::nursery)]

#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(any(target_os = "linux", windows))]
use crate::support::*;
#[cfg(windows)]
use serde_json::json;

/// A gate script that holds the session's socket loop long enough that an
/// external `kelir kill` cannot get an answer from it.
#[cfg(target_os = "linux")]
fn slow_gate() -> &'static str {
    "[gates]\nprompt_submit = 'python3 -c \"import time; time.sleep(12)\"'\n"
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn kill_a_session_stuck_in_a_slow_gate_from_a_terminal() {
    let mock = Mock::start(vec![]).await;
    let mut live = LiveSession::start(&mock, &["run", "--json", "HELLO"], slow_gate()).await;
    // `prompt_submit` runs synchronously before the model call; by the time
    // `LiveSession::start` has read `session_started` the gate is under way.
    tokio::time::sleep(Duration::from_secs(1)).await;

    // A second process, with no KELIR_SOCK: this is the exact external-kill
    // path that used to fail with "the session did not answer in time".
    let second = Sandbox::from_live(&live);
    let output = second
        .kelir_with_ceiling(
            &mock.base_url(),
            &["kill", &live.id],
            &[],
            Duration::from_secs(25),
        )
        .await;
    assert_eq!(
        output.code, 0,
        "external kill failed: {} / {}",
        output.stdout, output.stderr
    );
    assert!(
        output.stdout.contains("owner pid"),
        "the socket-blocked session did not use the owner fallback: {}",
        output.stdout
    );
    // The CLI's own claim is not proof: the root must actually be gone,
    // and gone before the harness guard does the killing for it. A
    // `killpg(<non-leader>)` returned success here while the session
    // kept running (PR #65 review). Check `/proc` immediately after the
    // CLI returns: a long grace poll lets the gate exit on its own and
    // the root turn zombie, which hides that bug.
    let root = live.pid();
    let stat = std::fs::read_to_string(format!("/proc/{root}/stat"))
        .expect("the session must still be up before the kill");
    let after = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    // After the command name: field 3 = state, 4 = ppid, 5 = pgrp.
    let pgid: u32 = after[2].parse().unwrap();
    assert_ne!(
        pgid, root,
        "this regression needs a session that is not a group leader"
    );
    // Only absence or a zombie is acceptable right now: a still-running
    // `S`/`R` root means the external kill lied.
    let root_gone = std::fs::metadata(format!("/proc/{root}")).is_err()
        || std::fs::read_to_string(format!("/proc/{root}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
        });
    assert!(
        root_gone,
        "session {root} survived the external kill: {:?}",
        std::fs::read_to_string(format!("/proc/{root}/stat"))
    );
    // The gate child sleeps 12 s: if the tree kill did not reach it, it
    // is still listed as a child of the session (or reparented to init)
    // well after the external kill returned. Unlike the root, the child
    // may need a short grace period to settle after SIGKILL.
    let children_path = format!("/proc/{root}/task/{root}/children");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut child_list = String::new();
    while std::time::Instant::now() < deadline {
        child_list = std::fs::read_to_string(&children_path).unwrap_or_default();
        if child_list.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        child_list.is_empty(),
        "gate children survived the external kill: {child_list}"
    );
    live.kill().await;
}

/// A forged owner file (a live but unrelated pid with the wrong start time)
/// must never signal that pid: the owner fallback verifies identity before it
/// kills, and reports failure instead.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn external_kill_refuses_a_pid_that_is_not_the_owner() {
    let mock = Mock::start(vec![]).await;
    let live = LiveSession::start(&mock, &["run", "--json", "HELLO"], slow_gate()).await;
    // Wait until the slow gate actually holds the socket: the fallback must
    // be reached, or the test proves nothing about the owner file.
    tokio::time::sleep(Duration::from_millis(700)).await;
    let victim = live.pid();
    let owner = live.session_dir.join("owner");
    let original = std::fs::read_to_string(&owner).expect("an owner file");
    let forged = format!("{{\"pid\":{victim},\"start\":1}}\n");
    std::fs::write(&owner, &forged).unwrap();

    let second = Sandbox::from_live(&live);
    let output = second
        .kelir_with_ceiling(
            &mock.base_url(),
            &["kill", &live.id],
            &[],
            Duration::from_secs(25),
        )
        .await;
    std::fs::write(&owner, &original).unwrap();
    assert!(
        output.stderr.contains("owner") || output.stderr.contains("not running"),
        "the fallback was not reached: {} / {}",
        output.stdout,
        output.stderr
    );
    assert_ne!(
        output.code, 0,
        "a recycled pid was signalled: {}",
        output.stdout
    );
    assert!(
        std::fs::metadata(format!("/proc/{victim}")).is_ok(),
        "the unrelated process was killed"
    );
    let mut live = live;
    live.kill().await;
}

#[cfg(windows)]
#[tokio::test]
async fn bash_tool_writes_its_output_through_the_shim() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "c1",
            "bash",
            &json!({"command": "echo BASH-PATH-OK"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("");
    let out = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "PROBE"], &[])
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let event = out
        .json_lines()
        .into_iter()
        .find(|event| event["type"] == "call_settled" && event["id"] == "c1")
        .expect("a settled bash call");
    assert_eq!(event["exit_code"], json!(0), "{event}");
    let text = event["result"].as_str().unwrap_or("");
    assert!(text.contains("BASH-PATH-OK"), "{event}");
}
