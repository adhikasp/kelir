//! Background-job child-process cleanup tests.
#![allow(clippy::pedantic, clippy::nursery)]

use std::time::Duration;

use crate::support::*;
use serde_json::json;

use super::tools_bg::*;

/// R11 (F5): after a session is killed, a child that a *finished* call left
/// running (`cmd &`) must not survive. Through M3 the Windows Job Object was
/// kill-on-close, so killing the session killed it; M4 made the job not
/// kill-on-close and the shim is already gone.
#[tokio::test]
async fn r11_a_leftover_background_child_dies_with_a_killed_session() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_left",
            "bash",
            &json!({"command": "( for i in $(seq 1 60); do echo tick >> left.txt; sleep 0.2; done ) > /dev/null 2>&1 &"}),
        ))),
        Reply::Sse(sse(&text_turn("left it running", 0))),
    ])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    live.wait_for("turn_ended").await;
    let ticks = live.cwd.join("left.txt");
    wait_until("the leftover to write", CEILING, || bytes_len(&ticks) > 0).await;
    live.kill().await;
    // The leftover may be mid-write when the kill lands, and on a loaded box
    // the tree takes a while to go (STEER-9): `first` is taken once the file has
    // stopped moving, and the window after it is what proves nothing writes.
    wait_until("the file to stop growing", CEILING, || {
        let before = bytes_len(&ticks);
        std::thread::sleep(Duration::from_millis(500));
        before == bytes_len(&ticks)
    })
    .await;
    let first = bytes_len(&ticks);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let second = bytes_len(&ticks);
    // (the loop is bounded to 60 iterations, so a survivor cleans itself up)
    assert!(
        second == first,
        "a leftover `&` child kept writing after its session was killed ({first} -> {second} bytes)"
    );
}

/// R18 (F7): the tree of an unpromoted call must die with a killed session even
/// when it holds *exec'd* grandchildren (`sleep &`, a nested `bash -c`), not just
/// fork-only subshells. On Windows with Git's msys runtime these children ask to
/// break away from the call's Job Object, and M4 gave that job
/// `JOB_OBJECT_LIMIT_BREAKAWAY_OK`.
#[tokio::test]
async fn r18_a_killed_session_takes_exec_grandchildren_with_it() {
    let mock = Mock::start(vec![Reply::Sse(sse(&tool_turn(
        "call_gc",
        "bash",
        &json!({"command": "bash -c 'sleep 7311 & sleep 7312 & wait' & sleep 7313 & wait"}),
    )))])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    live.wait_for("call_started").await;
    wait_until("the grandchildren to exist", CEILING, || {
        pids_with("7311").len() + pids_with("7312").len() + pids_with("7313").len() >= 3
    })
    .await;
    live.kill().await;
    settle_to_zero(|| {
        ["7311", "7312", "7313"]
            .iter()
            .map(|marker| pids_with(marker).len())
            .sum()
    })
    .await;
    let left: Vec<u32> = ["7311", "7312", "7313"]
        .iter()
        .flat_map(|marker| pids_with(marker))
        .collect();
    for marker in ["7311", "7312", "7313"] {
        reap(marker);
    }
    assert!(
        left.is_empty(),
        "processes of the killed session's call survived: {left:?}"
    );
}

/// R19 (F7): `bg cancel` must take exec'd grandchildren down too.
#[tokio::test]
async fn r19_cancel_takes_exec_grandchildren_with_it() {
    let sandbox = Sandbox::new("");
    let started = sandbox
        .kelir(
            NO_PROVIDER,
            &[
                "bg",
                "run",
                "bash -c 'sleep 7321 & sleep 7322 & wait' & sleep 7323 & wait",
            ],
            &[],
        )
        .await;
    assert_eq!(started.code, 0, "{}", started.stderr);
    let id = id_of(&started);
    wait_until("the grandchildren to exist", CEILING, || {
        pids_with("7321").len() + pids_with("7322").len() + pids_with("7323").len() >= 3
    })
    .await;
    let cancelled = sandbox
        .kelir(NO_PROVIDER, &["bg", "cancel", &id], &[])
        .await;
    assert_eq!(cancelled.code, 0, "{}", cancelled.stderr);
    settle_to_zero(|| {
        ["7321", "7322", "7323"]
            .iter()
            .map(|marker| pids_with(marker).len())
            .sum()
    })
    .await;
    let left: Vec<u32> = ["7321", "7322", "7323"]
        .iter()
        .flat_map(|marker| pids_with(marker))
        .collect();
    for marker in ["7321", "7322", "7323"] {
        reap(marker);
    }
    assert!(
        left.is_empty(),
        "bg cancel left processes of the job running: {left:?}"
    );
}
