//! Child process cleanup and security tests.
#![allow(clippy::pedantic, clippy::nursery)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::children_tree::*;
use crate::support::{CEILING, LiveSession, Mock};

#[tokio::test]
async fn killing_the_root_hard_leaves_no_orphans() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let held_gate = gate.clone();
    let mock = Mock::start_routed(vec![], move |request| {
        let task = task_of(request);
        let results = tool_results(request).len();
        Some(match (task.as_str(), results) {
            ("ROOT", 0) => bash(
                "call_start",
                "\"$KELIR_BIN\" run --child --background \"SLEEP one\" >/dev/null; \
                 \"$KELIR_BIN\" run --child --background \"NESTORPHAN x\" >/dev/null; echo started",
            ),
            ("ROOT", _) => bash("call_long", "sleep 300"),
            (nest, 0) if nest.starts_with("NESTORPHAN") => bash(
                "call_nest",
                "\"$KELIR_BIN\" run --child --background \"SLEEP nested\" >/dev/null; sleep 300",
            ),
            (other, _) if other.starts_with("SLEEP") => held(&held_gate),
            _ => return None,
        })
    })
    .await;
    let mut root = LiveSession::start(&mock, &["run", "--keep", "--json", "ROOT"], "").await;
    // Three live descendants: one sleeper, a nest, and the nest's sleeper.
    let list = wait_for_children(&root, 3).await;
    let pids = pids_of(&list);
    assert_eq!(pids.len(), 3, "{list:?}");
    assert!(pids.iter().all(|pid| pid_alive(*pid)));
    assert!(list.iter().any(|kid| kid["depth"] == 2), "{list:?}");

    // The session dies the way a crash or `taskkill /F` ends it: no cleanup.
    let state = root.state.clone();
    root.kill().await;

    wait_gone(&pids, CEILING).await;
    let started = Instant::now();
    loop {
        let left = leftovers_in(&state, &root.guard);
        if left.is_empty() {
            break;
        }
        assert!(
            started.elapsed() < CEILING,
            "processes left after the root was killed: {left:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    gate.notify_waiters();
}

#[tokio::test]
async fn stopping_the_call_stops_its_foreground_child() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let held_gate = gate.clone();
    let mock = Mock::start_routed(vec![], move |request| {
        let task = task_of(request);
        let results = tool_results(request).len();
        Some(match (task.as_str(), results) {
            ("ROOT", 0) => bash("call_fg", &child_cmd("", "SLEEP foreground")),
            ("ROOT", _) => say("root done"),
            (other, _) if other.starts_with("SLEEP") => held(&held_gate),
            _ => return None,
        })
    })
    .await;
    let mut root = LiveSession::start(&mock, &["run", "--keep", "--json", "ROOT"], "").await;
    let list = wait_for_children(&root, 1).await;
    let pids = pids_of(&list);
    let stopped = root.ctl(&["stop", "--call", "call_fg"]).await;
    assert_eq!(stopped.code, 0, "{} {}", stopped.stdout, stopped.stderr);
    wait_gone(&pids, CEILING).await;
    // The registry records how it ended, and the slot is free again.
    let settled = loop {
        let log = root.log();
        if let Some(event) = log.iter().find(|event| event["type"] == "call_settled") {
            break event.clone();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(
        settled["outcome"] == "stopped" || settled["outcome"] == "killed",
        "{settled}"
    );
    let list = child_list(&root).await;
    assert!(list.iter().all(|kid| kid["state"] != "running"), "{list:?}");
    gate.notify_waiters();
    root.kill().await;
}
