//! children registry tests: a_resumed_root_settles_the_chi, registry_operations_need_a_chi.
#![allow(clippy::pedantic, clippy::nursery)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::children_tree::*;
use crate::support::*;
use crate::support::{CEILING, LiveSession, Mock};
use serde_json::{Value, json};

#[tokio::test]
async fn registry_operations_need_a_child_session_in_the_tree_and_no_secret_travels() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let held_gate = gate.clone();
    let mock = Mock::start_routed(vec![], move |request| {
        let task = task_of(request);
        let results = tool_results(request).len();
        Some(match (task.as_str(), results) {
            ("ROOT", 0) => bash(
                "call_start",
                "\"$KELIR_BIN\" run --child --background \"SLEEP sec\" >/dev/null; \
                 \"$KELIR_BIN\" run --child --background \"ENVDUMP x\" >/dev/null; echo started",
            ),
            ("ROOT", _) => bash("call_long", "sleep 300"),
            (dump, 0) if dump.starts_with("ENVDUMP") => bash("call_env", "env | sort"),
            (dump, _) if dump.starts_with("ENVDUMP") => say("env dumped"),
            (other, _) if other.starts_with("SLEEP") => held(&held_gate),
            _ => return None,
        })
    })
    .await;
    let mut root = LiveSession::start(&mock, &["run", "--keep", "--json", "ROOT"], "").await;
    let list = wait_for_children(&root, 2).await;
    let before = root.log().len();
    let sleeper = list
        .iter()
        .find(|kid| kid["state"] == "running")
        .cloned()
        .unwrap();

    // 1. The token reaches every same-user caller, so the root checks the caller's session id:
    //    a guessed id is not in the tree, and a registry operation naming one is refused.
    let child_id = sleeper["child_id"].as_str().unwrap().to_string();
    for (op, extra) in [
        ("admit", json!({"task": "forged", "kind": "child"})),
        ("up", json!({"child_id": child_id, "pid": 4242})),
        (
            "settle",
            json!({"child_id": child_id, "outcome": "done", "result": "forged"}),
        ),
        ("kill", json!({"target": child_id})),
    ] {
        for session in ["", "guess", "0000"] {
            let mut args = json!({"op": op, "session": session});
            for (key, value) in extra.as_object().unwrap() {
                args[key] = value.clone();
            }
            let answer = raw(&root, "child", args, "child").await;
            assert!(
                !answer.ok,
                "{op} with session {session:?} was accepted: {answer:?}"
            );
            let error = answer.error.unwrap_or_default();
            assert!(error.contains("refused"), "{op}: {error}");
        }
    }
    // 2. The session's own operations need the session's private key.
    for op in [
        "admit-local",
        "up-local",
        "settle-local",
        "note-started",
        "note-settled",
        "reap",
    ] {
        let answer = raw(
            &root,
            "child",
            json!({"op": op, "key": "nope"}),
            "child-internal",
        )
        .await;
        assert!(!answer.ok, "{op}: {answer:?}");
        let answer = raw(&root, "child", json!({"op": op}), "child").await;
        assert!(!answer.ok, "{op}: {answer:?}");
    }
    // 3. Nothing was written and the sleeper is untouched.
    let after: Vec<Value> = root.log();
    assert!(
        after[before..]
            .iter()
            .all(|event| !event["type"].as_str().unwrap_or("").starts_with("child_")),
        "a forged operation reached the log: {:?}",
        &after[before..]
    );
    let list = child_list(&root).await;
    let still = list
        .iter()
        .find(|kid| kid["child_id"] == child_id.as_str())
        .unwrap();
    assert_eq!(still["state"], "running");
    assert_eq!(still["pid"], sleeper["pid"]);

    // 4. No secret in a child's argv or in any file kelir writes.
    let root_token = std::fs::read_to_string(root.session_dir.join("token")).unwrap();
    let root_token = root_token.trim().to_string();
    assert!(root_token.len() >= 32);
    let mut child_tokens = Vec::new();
    for kid in &list {
        let session = kid["session"].as_str().unwrap();
        let dir = root.state.join("sessions").join(session);
        if let Ok(token) = std::fs::read_to_string(dir.join("token")) {
            child_tokens.push((session.to_string(), token.trim().to_string()));
        }
        if kid["state"] != "running" {
            continue;
        }
        let pid = kid["pid"].as_u64().unwrap() as u32;
        let line = command_line_of(pid);
        // On Windows the host's own DACL denies even this read (the PEB needs
        // PROCESS_VM_READ), which is the hardening working; Linux shows it.
        if cfg!(not(windows)) || !line.trim().is_empty() {
            assert!(
                line.trim_end().ends_with("child-host"),
                "the child host's command line is only `child-host`: {line}"
            );
        }
        assert!(!line.contains(&root_token), "{line}");
    }
    // sleep or a "done" guess in `ls`. The full `env | sort` output is in the child's
    // `calls/<id>/out` file; the event's `result` is only the model's preview and is
    // deliberately spilled (first/last 1 KB) when the inherited environment is large.
    // Under nextest each test process inherits a different set of `KELIR_*`/`CARGO_*`
    // variables, so the sorted dump crosses the 10 KB spill limit on some runs: the
    // `KELIR_*` lines then fall in the omitted middle. Assert on the real file, bounded
    // by the same CEILING (STEER-9: event-driven, no weaker proposition).
    let waiting = Instant::now();
    let envdump = loop {
        assert!(
            waiting.elapsed() < CEILING,
            "the env dump child never finished"
        );
        let list = child_list(&root).await;
        let kid = list.iter().find(|kid| kid["state"] == "done").cloned();
        if let Some(kid) = kid {
            break kid;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let dump_dir = root
        .state
        .join("sessions")
        .join(envdump["session"].as_str().unwrap());
    let dump_log = std::fs::read_to_string(dump_dir.join("events.jsonl")).unwrap();
    let mut spill_found = false;
    let dump: String = dump_log
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["type"] == "call_settled")
        .map(|event| {
            spill_found |= event["result"]
                .as_str()
                .unwrap_or_default()
                .contains("[output spilled:");
            event["result"].to_string()
        })
        .collect();
    // call directory so the assertion holds whatever the inherited env size is.
    let mut full_dump = dump.clone();
    for path in dump_dir
        .join("calls")
        .read_dir()
        .into_iter()
        .flatten()
        .flatten()
    {
        if let Ok(text) = std::fs::read_to_string(path.path().join("out"))
            && text.contains("KELIR_ROOT=")
        {
            full_dump = text;
            break;
        }
    }
    assert!(full_dump.contains("KELIR_ROOT="), "{full_dump}");
    assert!(
        full_dump.contains(&format!(
            "KELIR_PARENT={}",
            envdump["session"].as_str().unwrap()
        )),
        "{full_dump}"
    );
    assert!(full_dump.contains("KELIR_DEPTH=1"), "{full_dump}");
    assert!(
        full_dump.contains(&format!("KELIR_ROOT={}", root.id)),
        "{full_dump}"
    );
    assert!(
        !full_dump.contains(&root_token),
        "the root's token reached a tool child"
    );
    assert!(
        !full_dump.contains("test-openrouter-key"),
        "the provider key reached a tool child"
    );
    // The preview is the model's view: it must be bounded and must not carry a
    // secret even when the full output spilled.
    assert!(
        !dump.contains(&root_token),
        "the root's token reached a tool result preview"
    );
    assert!(
        !dump.contains("test-openrouter-key"),
        "a provider key reached a tool result preview"
    );
    let _ = spill_found;
    let mut files = Vec::new();
    all_files(&root.state.join("sessions"), &mut files);
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if name != "token" {
            assert!(
                !text.contains(&root_token),
                "root token in {}",
                path.display()
            );
            // A child's own token is its tool calls' by design (`env` shows
            // it in its own directory, and any file under that session is
            // inside the same trust boundary); it must not appear under a
            // DIFFERENT session. Check the path's session component, not just
            // any path component: a `calls/<seq>/out` file belongs to its
            // session even though its own name is not a session id.
            for (owner, token) in &child_tokens {
                let session_name = path
                    .components()
                    .rev()
                    .find(|part| {
                        let name = part.as_os_str().to_string_lossy();
                        name.starts_with("ses_")
                    })
                    .map(|part| part.as_os_str().to_string_lossy().into_owned());
                let inside = session_name.as_deref() == Some(owner.as_str());
                assert!(
                    inside || !text.contains(token),
                    "child {owner}'s token in {} (path session: {session_name:?})",
                    path.display()
                );
            }
        }
        assert!(
            !text.contains("test-openrouter-key") && !text.contains("test-go-key"),
            "a provider key in {}",
            path.display()
        );
    }
    gate.notify_waiters();
    root.kill().await;
}

#[tokio::test]
async fn a_resumed_root_settles_the_children_it_cannot_reach_as_lost() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let held_gate = gate.clone();
    let mock = Mock::start_routed(vec![], move |request| {
        let task = task_of(request);
        let results = tool_results(request).len();
        if last_text(request).starts_with("go on") {
            return Some(say("resumed"));
        }
        Some(match (task.as_str(), results) {
            ("ROOT", 0) => bash(
                "call_start",
                "\"$KELIR_BIN\" run --child --background \"SLEEP lost\" >/dev/null; echo started",
            ),
            ("ROOT", _) => bash("call_long", "sleep 300"),
            (other, _) if other.starts_with("SLEEP") => held(&held_gate),
            _ => return None,
        })
    })
    .await;
    let mut root = LiveSession::start(&mock, &["run", "--keep", "--json", "ROOT"], "").await;
    let list = wait_for_children(&root, 1).await;
    let pids = pids_of(&list);
    root.kill().await;
    wait_gone(&pids, CEILING).await;

    let sandbox = Sandbox::from_live(&root);
    let out = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--resume", &root.id, "go on"],
            &[],
        )
        .await;
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let log = sandbox.log(&root.id);
    let settled = calls_of(&log, "call_settled", "child");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["outcome"], "lost");
    let started = calls_of(&log, "call_started", "child");
    let start = started
        .iter()
        .find(|event| event["id"] == settled[0]["id"])
        .expect("the child this resume settled");
    assert_eq!(start["info"]["session"], list[0]["session"]);
    gate.notify_waiters();
}
