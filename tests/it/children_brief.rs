//! children brief tests: a_brief_cannot_delegate_and_ls, a_brief_shares_the_parents_pre, ls_shows_the_tree_and_kill_tak.
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::children_tree::*;
use crate::support::*;
use crate::support::{CEILING, LiveSession, Mock, Reply, TreeGuard};
use serde_json::{Value, json};

#[tokio::test]
async fn a_brief_shares_the_parents_prefix_and_leaves_its_history_alone() {
    let mock = Mock::start_routed(vec![], |request| {
        // The brief: the parent's history, then the question.
        if is_brief(request) {
            let last_is_tool = request
                .messages()
                .last()
                .is_some_and(|message| message["role"] == "tool");
            return Some(if last_is_tool {
                say("the brief answer")
            } else {
                Reply::Sse(sse(&tool_turn(
                    "call_edit",
                    "edit",
                    &json!({"path": "x.txt", "old": "", "new": "nope"}),
                )))
            });
        }
        Some(match tool_results(request).len() {
            0 => bash("call_a", "echo prior work"),
            1 => bash(
                "call_brief",
                "\"$KELIR_BIN\" brief \"BRIEFQ what is the answer\"",
            ),
            _ => say("root done"),
        })
    })
    .await;
    let family = Family::new("");
    let out = family.run(&mock, "ROOT").await;
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let requests = mock.requests();
    let parent: Vec<&Recorded> = requests.iter().filter(|r| !is_brief(r)).collect();
    let brief: Vec<&Recorded> = requests.iter().filter(|r| is_brief(r)).collect();
    assert_eq!(parent.len(), 3, "root steps");
    assert_eq!(
        brief.len(),
        2,
        "the brief asked, was refused an edit, answered"
    );

    // The first brief request is the parent's last settled request plus the
    // question: same system, same tools, same messages, one more at the tail.
    let prefix = parent[1];
    let first = brief[0];
    assert_eq!(first.body["tools"], prefix.body["tools"]);
    assert_eq!(first.body["model"], prefix.body["model"]);
    let shared = prefix.messages().len();
    assert_eq!(&first.messages()[..shared], prefix.messages());
    assert_eq!(first.messages().len(), shared + 1);
    assert!(last_text(first).starts_with("BRIEFQ"));
    // ... and it keys the provider's cache by the parent's session.
    let parent_key = prefix.body["session_id"].as_str().expect("a session_id");
    assert_eq!(first.body["session_id"].as_str(), Some(parent_key));

    // `edit` is refused by result, not by a different tool list.
    let refusal = tool_results(brief[1]).pop().unwrap_or_default();
    assert!(refusal.starts_with("refused:"), "{refusal}");
    assert!(refusal.contains("edit"), "{refusal}");

    // The answer is the bash call's stdout.
    let (root_id, root) = family.session_for("ROOT");
    assert!(
        result_of_call(&root, "call_brief").contains("the brief answer"),
        "{}",
        result_of_call(&root, "call_brief")
    );
    // The parent's history is untouched: the child_* pair, nothing else new.
    assert_eq!(of_type(&root, "user").len(), 1);
    assert_eq!(of_type(&root, "assistant").len(), 3);
    let started = calls_of(&root, "call_started", "child");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["info"]["name"], "brief");
    assert_eq!(started[0]["info"]["parent"], root_id.as_str());
    assert_eq!(
        calls_of(&root, "call_settled", "child")[0]["outcome"],
        "done"
    );
    assert!(of_type(&root, "source").is_empty());
}

/// m7fix S3 (and S1 end to end): a brief cannot start children or briefs, by
/// result with exit 3, and `ls` lists it as a brief with its own question.
#[tokio::test]
async fn a_brief_cannot_delegate_and_ls_lists_it_as_a_brief() {
    let mock = Mock::start_routed(vec![], |request| {
        if is_brief(request) {
            let last_is_tool = request
                .messages()
                .last()
                .is_some_and(|message| message["role"] == "tool");
            return Some(if last_is_tool {
                say("brief done")
            } else {
                // The snapshot may carry an old instruction to delegate.
                bash(
                    "call_try",
                    "\"$KELIR_BIN\" run --child \"CHILDT x\"; echo rc1=$?; \"$KELIR_BIN\" brief \"BRIEFQ nested\"; echo rc2=$?",
                )
            });
        }
        Some(match tool_results(request).len() {
            0 => bash("call_a", "echo prior work"),
            1 => bash("call_brief", "\"$KELIR_BIN\" brief \"BRIEFQ outer\""),
            2 => bash("call_ls", "\"$KELIR_BIN\" ls --json"),
            _ => say("root done"),
        })
    })
    .await;
    let family = Family::new("");
    let out = family.run(&mock, "ROOT").await;
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let requests = mock.requests();
    let brief: Vec<&Recorded> = requests.iter().filter(|r| is_brief(r)).collect();
    assert_eq!(brief.len(), 2, "one tool call, one answer; no nested brief");
    let tried = tool_results(brief[1]).pop().unwrap_or_default();
    assert_eq!(
        tried.matches("refused: this is a brief").count(),
        2,
        "{tried}"
    );
    assert!(
        tried.contains("rc1=3") && tried.contains("rc2=3"),
        "{tried}"
    );
    assert!(
        !requests.iter().any(|r| task_of(r) == "CHILDT x"),
        "no child was started"
    );
    let (_, root) = family.session_for("ROOT");
    assert_eq!(
        calls_of(&root, "call_started", "child").len(),
        1,
        "only the outer brief"
    );
    let listing = result_of_call(&root, "call_ls");
    assert!(listing.contains("\"kind\":\"brief\""), "{listing}");
    assert!(listing.contains("BRIEFQ outer"), "{listing}");
}

#[tokio::test]
async fn ls_shows_the_tree_and_kill_takes_a_subtree_but_only_from_above() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let held_gate = gate.clone();
    let mock = Mock::start_routed(vec![], move |request| {
        let task = task_of(request);
        let results = tool_results(request).len();
        Some(match (task.as_str(), results) {
            ("ROOT", 0) => bash(
                "call_main",
                "for t in NESTK SIB ATTACK; do \"$KELIR_BIN\" run --child --background \"$t x\" >/dev/null; done; for i in $(seq 1 90); do \"$KELIR_BIN\" ls | grep -q 'SLEEP deep' && break; sleep 1; done; \
                 \"$KELIR_BIN\" ls --json; \
                 nk=$(\"$KELIR_BIN\" ls | grep NESTK | grep -o 'ses_[a-z0-9]*' | head -1); \
                 \"$KELIR_BIN\" kill $nk; echo killrc=$?; sleep 2; \"$KELIR_BIN\" ls; for i in $(seq 1 90); do \"$KELIR_BIN\" ls | grep 'ATTACK x' | grep -q ' done ' && break; sleep 1; done",
            ),
            ("ROOT", _) => say("root done"),
            (nest, 0) if nest.starts_with("NESTK") => bash(
                "call_nest",
                "\"$KELIR_BIN\" run --child --background \"SLEEP deep\" >/dev/null; sleep 40",
            ),
            (attack, 0) if attack.starts_with("ATTACK") => bash(
                "call_attack",
                "sleep 3; for s in $(\"$KELIR_BIN\" ls | grep -o 'ses_[a-z0-9]*' | sort -u); do out=$(\"$KELIR_BIN\" kill $s 2>&1); echo \"$s rc=$? $out\"; done",
            ),
            (attack, _) if attack.starts_with("ATTACK") => say("attack done"),
            (other, _) if other.starts_with("SIB") || other.starts_with("SLEEP") => {
                held(&held_gate)
            }
            _ => return None,
        })
    })
    .await;
    let family = Family::new("");
    let out = family.run(&mock, "ROOT").await;
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let (root_id, root) = family.session_for("ROOT");
    let text = result_of_call(&root, "call_main");

    // `ls --json` is one line holding the tree: the root, with its children.
    let json_line = text
        .lines()
        .find(|line| line.starts_with('['))
        .expect("ls --json");
    let tree: Value = serde_json::from_str(json_line).unwrap();
    assert_eq!(tree[0]["id"], root_id.as_str());
    let kids = tree[0]["children"].as_array().unwrap();
    assert_eq!(kids.len(), 3, "{tree}");
    assert!(
        kids.iter()
            .all(|kid| kid["depth"] == 1 && kid["kind"] == "child")
    );
    let nest = kids
        .iter()
        .find(|kid| kid["task"].as_str().unwrap().starts_with("NESTK"))
        .unwrap();
    assert_eq!(nest["children"][0]["depth"], 2);
    for key in ["state", "model", "cache_hit_ratio", "task"] {
        assert!(tree[0].get(key).is_some(), "missing {key}");
    }

    // Killing the nest stops it and the sleeper below it.
    assert!(text.contains("killrc=0"), "{text}");
    let settled = of_type(&root, "call_settled");
    let killed: Vec<_> = settled
        .iter()
        .filter(|e| e["outcome"] == "killed")
        .collect();
    assert_eq!(killed.len(), 2, "{settled:?}");
    let after = text.split("killrc=").nth(1).unwrap();
    assert!(after.contains("killed"), "{after}");
    assert!(after.contains("SIB x"), "{after}");

    // A child can stop only what is below it: every try from ATTACK is refused.
    let (attack_id, attack) = family.session_for("ATTACK");
    let tries = result_of_call(&attack, "call_attack");
    let lines: Vec<&str> = tries.lines().filter(|l| l.contains(" rc=")).collect();
    assert!(lines.len() >= 4, "{tries}");
    assert!(
        lines
            .iter()
            .all(|line| line.contains("rc=1") && line.contains("refused")),
        "{tries}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with(&attack_id)),
        "it tried itself too: {tries}"
    );
    gate.notify_waiters();
    no_leftovers(&family).await;
}
