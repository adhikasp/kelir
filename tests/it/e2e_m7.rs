//! M7 end-to-end: children as a CLI (`kelir run --child`, `kelir brief`,
//! `kelir ls`, `kelir kill`) against a routed mock provider.
//!
//! Several sessions share one mock, so the mock picks each reply from the
//! request itself (`Mock::start_routed`): the first user message says whose
//! turn it is, and the presence of a tool result says how far along it is.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::support::*;
use serde_json::{Value, json};

// ----------------------------------------------------------------- helpers

/// The text of a message's `content`, whatever shape the request used.
fn text_of(message: &Value) -> String {
    match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// The first user message: the task that opened the session.
fn task_of(request: &Recorded) -> String {
    request
        .messages()
        .iter()
        .find(|message| message["role"] == "user")
        .map(text_of)
        .unwrap_or_default()
}

fn tool_results(request: &Recorded) -> Vec<String> {
    request
        .messages()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(text_of)
        .collect()
}

fn bash(id: &str, command: &str) -> Reply {
    Reply::Sse(sse(&tool_turn(id, "bash", &json!({"command": command}))))
}

fn say(text: &str) -> Reply {
    Reply::Sse(sse(&text_turn(text, 0)))
}

/// A reply that never comes until the test opens the gate: a child that is
/// "thinking" for as long as the test needs it to be alive.
fn held(gate: &Arc<tokio::sync::Notify>) -> Reply {
    Reply::Gated(gate.clone(), sse(&text_turn("woke", 0)))
}

/// `"$KELIR_BIN" run --child …` as a bash command.
fn child_cmd(flags: &str, task: &str) -> String {
    format!("\"$KELIR_BIN\" run --child {flags} \"{task}\"")
}

struct Family {
    sandbox: Sandbox,
}

impl Family {
    fn new(extra_config: &str) -> Family {
        Family {
            sandbox: Sandbox::new(extra_config),
        }
    }

    /// Run a root session to the end.
    async fn run(&self, mock: &Mock, task: &str) -> Output {
        self.sandbox
            .kelir(&mock.base_url(), &["run", "--json", task], &[])
            .await
    }

    /// Every session's events, keyed by the task that opened it.
    fn by_task(&self) -> Vec<(String, String, Vec<Value>)> {
        self.sandbox
            .session_ids()
            .into_iter()
            .map(|id| {
                let log = self.sandbox.log(&id);
                let task = log
                    .iter()
                    .find(|event| event["type"] == "user")
                    .and_then(|event| event["text"].as_str())
                    .unwrap_or("")
                    .to_string();
                (id, task, log)
            })
            .collect()
    }

    fn session_for(&self, task_prefix: &str) -> (String, Vec<Value>) {
        // A brief's log starts with a copy of its parent's, task included: the
        // session that is the task's own is the one with no parent.
        let found: Vec<_> = self
            .by_task()
            .into_iter()
            .filter(|(_, task, log)| {
                task.starts_with(task_prefix)
                    && !(log[0]["parent"].is_string() && log[0]["depth"] != 0 && task == "ROOT")
            })
            .collect();
        assert_eq!(
            found.len(),
            1,
            "expected one session for `{task_prefix}`, got {:?}",
            self.by_task()
                .iter()
                .map(|(id, task, _)| (id.clone(), task.clone()))
                .collect::<Vec<_>>()
        );
        let (id, _, log) = found.into_iter().next().unwrap();
        (id, log)
    }
}

fn of_type<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["type"] == kind)
        .collect()
}

fn calls_of<'a>(events: &'a [Value], kind: &str, call_kind: &str) -> Vec<&'a Value> {
    of_type(events, kind)
        .into_iter()
        .filter(|event| event["kind"] == call_kind)
        .collect()
}

fn result_of_call(events: &[Value], call_id: &str) -> String {
    events
        .iter()
        .find(|event| event["type"] == "call_settled" && event["id"] == call_id)
        .map(|event| match &event["result"] {
            Value::String(text) => text.clone(),
            other => other["preview"].as_str().unwrap_or("").to_string(),
        })
        .unwrap_or_default()
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        std::process::Command::new(
            std::path::Path::new(&root)
                .join("System32")
                .join("tasklist.exe"),
        )
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\"")))
        .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .map(|out| {
                let stat = String::from_utf8_lossy(&out.stdout);
                out.status.success() && !stat.trim().is_empty() && !stat.trim().starts_with('Z')
            })
            .unwrap_or(false)
    }
}

async fn wait_gone(pids: &[u32], within: Duration) {
    let started = Instant::now();
    loop {
        let alive: Vec<u32> = pids.iter().copied().filter(|pid| pid_alive(*pid)).collect();
        if alive.is_empty() {
            return;
        }
        assert!(
            started.elapsed() < within,
            "processes still running after {within:?}: {alive:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ------------------------------------------------------------------- tests

#[tokio::test]
async fn a_child_of_a_child_finishes_and_the_result_is_the_bash_output() {
    let mock = Mock::start_routed(vec![], |request| {
        let task = task_of(request);
        let done = !tool_results(request).is_empty();
        Some(match (task.as_str(), done) {
            ("ROOT", false) => bash("call_root", &child_cmd("", "CHILD-A work")),
            ("ROOT", true) => say("root done"),
            (a, false) if a.starts_with("CHILD-A") => {
                bash("call_a", &child_cmd("", "CHILD-B leaf"))
            }
            (a, true) if a.starts_with("CHILD-A") => say("A done"),
            (b, _) if b.starts_with("CHILD-B") => say("B result"),
            _ => return None,
        })
    })
    .await;
    let family = Family::new("");
    let out = family.run(&mock, "ROOT").await;
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert_requests_snapshot("child_wake", &mock.requests());

    let (root_id, root) = family.session_for("ROOT");
    let (a_id, a) = family.session_for("CHILD-A");
    let (b_id, b) = family.session_for("CHILD-B");

    // The foreground child's result is the bash call's stdout.
    assert!(
        result_of_call(&root, "call_root").contains("A done"),
        "{}",
        result_of_call(&root, "call_root")
    );
    assert!(result_of_call(&a, "call_a").contains("B result"));

    // Each child owns its session, with its place in the tree.
    let started = |log: &[Value]| log[0].clone();
    assert_eq!(started(&a)["type"], "session_started");
    assert_eq!(started(&a)["parent"], root_id.as_str());
    assert_eq!(started(&a)["depth"], 1);
    assert_eq!(started(&b)["parent"], a_id.as_str());
    assert_eq!(started(&b)["depth"], 2);
    assert_eq!(started(&root)["depth"], 0);
    assert!(started(&root).get("parent").is_none() || started(&root)["parent"].is_null());

    // The root is the registry: both descendants, started and settled.
    let started_in_root = calls_of(&root, "call_started", "child");
    assert_eq!(
        started_in_root.len(),
        2,
        "{started_in_root:?}
A: {:?}
B: {:?}",
        a.iter().map(|e| e["type"].to_string()).collect::<Vec<_>>(),
        b.iter()
            .map(|e| format!("{} {}", e["type"], e["text"]))
            .collect::<Vec<_>>()
    );
    let depths: Vec<u64> = started_in_root
        .iter()
        .map(|event| event["info"]["depth"].as_u64().unwrap())
        .collect();
    assert_eq!(depths, vec![1, 2]);
    let settled_in_root = calls_of(&root, "call_settled", "child");
    assert_eq!(
        settled_in_root.len(),
        2,
        "{:#?}",
        root.iter()
            .map(|e| format!("{} {} {}", e["seq"], e["type"], e["error"]))
            .collect::<Vec<_>>()
    );
    assert!(
        settled_in_root
            .iter()
            .all(|event| event["outcome"] == "done")
    );
    let child_starts = calls_of(&root, "call_started", "child");
    assert!(settled_in_root.iter().any(|event| {
        child_starts
            .iter()
            .any(|start| start["id"] == event["id"] && start["info"]["session"] == b_id.as_str())
            && event["result"] == "B result"
    }));
    // The middle agent also keeps its own child's pair (it is the parent).
    assert_eq!(calls_of(&a, "call_started", "child").len(), 1);
    assert_eq!(calls_of(&a, "call_settled", "child").len(), 1);
    // The root's own pair is the only one in its log for its own child.
    assert_eq!(of_type(&b, "call_started").len(), 0);
}

#[tokio::test]
async fn a_background_child_returns_a_handle_and_wakes_the_parent() {
    let mock = Mock::start_routed(vec![], |request| {
        let task = task_of(request);
        let results = tool_results(request).len();
        Some(match (task.as_str(), results) {
            ("ROOT", 0) => bash("call_bg", &child_cmd("--background", "CHILD-BG look")),
            ("ROOT", 1) => bash(
                "call_wait",
                "for i in $(seq 1 90); do \"$KELIR_BIN\" ls | grep 'CHILD-BG' | grep -q ' done ' && break; sleep 1; done; echo waited",
            ),
            ("ROOT", _) => say("root done"),
            (c, _) if c.starts_with("CHILD-BG") => say("bg answer"),
            _ => return None,
        })
    })
    .await;
    let family = Family::new("");
    let out = family.run(&mock, "ROOT").await;
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let (_, root) = family.session_for("ROOT");
    let handle = result_of_call(&root, "call_bg");
    assert!(
        handle.contains("started in the background") && handle.contains("ses_"),
        "{handle}"
    );
    // The completion is a message the model reads at the next boundary.
    let last = mock
        .requests()
        .into_iter()
        .rfind(|request| task_of(request) == "ROOT")
        .unwrap();
    let wake = last
        .messages()
        .iter()
        .map(text_of)
        .find(|text| text.starts_with("[child ses_"))
        .unwrap_or_else(|| panic!("no wake message in {:?}", last.roles()));
    assert!(
        wake.contains("done") && wake.contains("bg answer"),
        "{wake}"
    );
    let woke = root
        .iter()
        .find(|event| event["type"] == "user" && event["from"] == "child")
        .expect("a user event from child");
    assert_eq!(woke["delivery"], "next");
}

#[tokio::test]
async fn outside_a_session_the_child_flags_say_so() {
    let sandbox = Sandbox::new("");
    let out = sandbox
        .kelir(
            "http://127.0.0.1:9/v1",
            &["run", "--child", "--background", "x"],
            &[],
        )
        .await;
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("KELIR_SOCK"), "{}", out.stderr);
    let out = sandbox
        .kelir("http://127.0.0.1:9/v1", &["brief", "what?"], &[])
        .await;
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("no session"), "{}", out.stderr);
    let out = sandbox.kelir("http://127.0.0.1:9/v1", &["kill"], &[]).await;
    assert_eq!(out.code, 2);
    let out = sandbox
        .kelir("http://127.0.0.1:9/v1", &["ls", "--json"], &[])
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout.trim(), "[]");
}

// -------------------------------------------------------------------- caps

/// Processes a test's sessions left behind. Windows: the harness's Job Object
/// knows (everything the test started is in it). Elsewhere: any process whose
/// environment names this test's state dir.
fn leftovers(family: &Family) -> Vec<u32> {
    #[cfg(windows)]
    {
        vec![0; family.sandbox.guard.active() as usize]
    }
    #[cfg(not(windows))]
    {
        let needle = format!("KELIR_STATE_DIR={}", family.sandbox.state.display());
        let mut found = Vec::new();
        for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Ok(environ) = std::fs::read(entry.path().join("environ")) else {
                continue;
            };
            let text = String::from_utf8_lossy(&environ);
            if text.split('\0').any(|var| var == needle) && pid_alive(pid) {
                found.push(pid);
            }
        }
        found
    }
}

async fn no_leftovers(family: &Family) {
    let started = Instant::now();
    loop {
        let left = leftovers(family);
        if left.is_empty() {
            return;
        }
        assert!(
            started.elapsed() < CEILING,
            "processes left after the root ended: {left:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn the_eleventh_live_child_of_one_agent_is_refused_and_nothing_outlives_the_root() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let held_gate = gate.clone();
    let mock = Mock::start_routed(vec![], move |request| {
        let task = task_of(request);
        let results = tool_results(request).len();
        Some(match (task.as_str(), results) {
            ("ROOT", 0) => bash(
                "call_loop",
                "for i in 1 2 3 4 5 6 7 8 9 10 11; do out=$(\"$KELIR_BIN\" run --child --background \"SLEEP $i\" 2>&1); echo \"rc=$? $out\"; done",
            ),
            ("ROOT", _) => say("root done"),
            (sleeper, _) if sleeper.starts_with("SLEEP") => held(&held_gate),
            _ => return None,
        })
    })
    .await;
    let family = Family::new("");
    let out = family.run(&mock, "ROOT").await;
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let (_, root) = family.session_for("ROOT");
    let lines = result_of_call(&root, "call_loop");
    let rcs: Vec<&str> = lines
        .lines()
        .filter(|line| line.starts_with("rc="))
        .collect();
    assert_eq!(rcs.len(), 11, "{lines}");
    assert!(
        rcs[..10].iter().all(|line| line.starts_with("rc=0 ")),
        "{lines}"
    );
    assert!(rcs[10].starts_with("rc=3 "), "the 11th: {}", rcs[10]);
    assert!(rcs[10].contains("10 live children"), "{}", rcs[10]);
    assert!(rcs[10].contains("limit 10"), "{}", rcs[10]);
    assert_eq!(calls_of(&root, "call_started", "child").len(), 10);
    gate.notify_waiters();
    // The sleepers were still thinking when the root ended: none may survive it.
    no_leftovers(&family).await;
    // The root's end killed them; each is settled in its log, not left running.
    let settled = calls_of(&root, "call_settled", "child");
    assert_eq!(
        settled.len(),
        0,
        "the root exited before they settled: {settled:?}"
    );
}

#[tokio::test]
async fn the_seventeenth_live_descendant_is_refused() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let held_gate = gate.clone();
    let mock = Mock::start_routed(vec![], move |request| {
        let task = task_of(request);
        let results = tool_results(request).len();
        Some(match (task.as_str(), results) {
            ("ROOT", 0) => bash(
                "call_loop",
                "for i in 1 2 3 4 5 6 7 8 9; do \"$KELIR_BIN\" run --child --background \"SLEEP $i\" >/dev/null 2>&1; done; \"$KELIR_BIN\" run --child --background \"NEST all\"",
            ),
            ("ROOT", 1) => bash(
                "call_wait",
                "for i in $(seq 1 90); do \"$KELIR_BIN\" ls | grep 'NEST all' | grep -q ' done ' && break; sleep 1; done; echo waited",
            ),
            ("ROOT", _) => say("root done"),
            (nest, 0) if nest.starts_with("NEST") => bash(
                "call_nest",
                "for i in 1 2 3 4 5 6 7; do out=$(\"$KELIR_BIN\" run --child --background \"DEEP $i\" 2>&1); echo \"rc=$? $out\"; done",
            ),
            (nest, _) if nest.starts_with("NEST") => say("nest done"),
            (other, _) if other.starts_with("SLEEP") || other.starts_with("DEEP") => {
                held(&held_gate)
            }
            _ => return None,
        })
    })
    .await;
    let family = Family::new("");
    let out = family.run(&mock, "ROOT").await;
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let (_, nest) = family.session_for("NEST");
    let lines = result_of_call(&nest, "call_nest");
    let rcs: Vec<&str> = lines
        .lines()
        .filter(|line| line.starts_with("rc="))
        .collect();
    assert_eq!(rcs.len(), 7, "{lines}");
    assert!(
        rcs[..6].iter().all(|line| line.starts_with("rc=0 ")),
        "{lines}"
    );
    assert!(rcs[6].starts_with("rc=3 "), "the 17th: {}", rcs[6]);
    assert!(rcs[6].contains("16 live descendants"), "{}", rcs[6]);
    gate.notify_waiters();
    no_leftovers(&family).await;
}

#[tokio::test]
async fn depth_four_is_refused() {
    let mock = Mock::start_routed(vec![], |request| {
        let task = task_of(request);
        let done = !tool_results(request).is_empty();
        Some(match (task.as_str(), done) {
            ("L1", false) => bash("call_1", &child_cmd("", "L2 x")),
            ("L2 x", false) => bash("call_2", &child_cmd("", "L3 x")),
            ("L3 x", false) => bash("call_3", &child_cmd("", "L4 x")),
            ("L4 x", false) => bash(
                "call_4",
                "out=$(\"$KELIR_BIN\" run --child \"L5 x\" 2>&1); echo \"rc=$? $out\"",
            ),
            (_, true) => say("done"),
            (leaf, _) if leaf.starts_with("L5") => say("never runs"),
            _ => return None,
        })
    })
    .await;
    let family = Family::new("");
    let out = family.run(&mock, "L1").await;
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let (_, l4) = family.session_for("L4 x");
    let result = result_of_call(&l4, "call_4");
    assert!(result.contains("rc=3"), "{result}");
    assert!(result.contains("depth 4"), "{result}");
    assert!(result.contains("[children] max_depth"), "{result}");
    assert!(
        family
            .by_task()
            .iter()
            .all(|(_, task, _)| !task.starts_with("L5")),
        "no depth-4 session may exist"
    );
    let (_, l1) = family.session_for("L1");
    assert_eq!(
        calls_of(&l1, "call_started", "child").len(),
        3,
        "L2, L3, L4 and nothing for L5"
    );
    let started = family.session_for("L4 x").1;
    assert_eq!(started[0]["depth"], 3);
}

// ------------------------------------------------------------------- brief

fn last_text(request: &Recorded) -> String {
    request.messages().last().map(text_of).unwrap_or_default()
}

fn is_brief(request: &Recorded) -> bool {
    request
        .messages()
        .iter()
        .any(|message| message["role"] == "user" && text_of(message).starts_with("BRIEFQ"))
}

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

// ----------------------------------------------------------- ls and kill

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

// ------------------------------------------------- live sessions: orphans

/// The live children of a root, from `ctl status --json`.
async fn child_list(live: &LiveSession) -> Vec<Value> {
    let answer = live.ctl(&["status", "--json"]).await;
    assert_eq!(answer.code, 0, "{}", answer.stderr);
    answer.response()["state"]["child_list"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

async fn wait_for_children(live: &LiveSession, count: usize) -> Vec<Value> {
    let started = Instant::now();
    loop {
        let list = child_list(live).await;
        if list.len() >= count && list.iter().all(|kid| kid["pid"].is_number()) {
            return list;
        }
        assert!(
            started.elapsed() < CEILING,
            "wanted {count} children with pids, have {list:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn pids_of(list: &[Value]) -> Vec<u32> {
    list.iter()
        .filter_map(|kid| kid["pid"].as_u64())
        .map(|pid| pid as u32)
        .collect()
}

fn leftovers_in(state: &std::path::Path, guard: &TreeGuard) -> Vec<u32> {
    #[cfg(windows)]
    {
        let _ = state;
        vec![0; guard.active() as usize]
    }
    #[cfg(not(windows))]
    {
        let _ = guard;
        let needle = format!("KELIR_STATE_DIR={}", state.display());
        let mut found = Vec::new();
        for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Ok(environ) = std::fs::read(entry.path().join("environ")) else {
                continue;
            };
            let text = String::from_utf8_lossy(&environ);
            if text.split('\0').any(|var| var == needle) && pid_alive(pid) {
                found.push(pid);
            }
        }
        found
    }
}

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

// -------------------------------------------------------------- security

/// A raw request to a session's socket with its token, as any holder of the
/// token file could send.
async fn raw(live: &LiveSession, verb: &str, args: Value, from: &str) -> kelir::ipc::Response {
    let request = kelir::ipc::Request::new(verb, args, from);
    kelir::ipc::request_with_token_file(&live.session_dir, &live.id, &request)
        .await
        .expect("the session answers")
}

fn all_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            all_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[cfg(windows)]
fn command_line_of(pid: u32) -> String {
    use std::os::windows::process::CommandExt as _;
    let script = format!("(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CommandLine");
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x0800_0000)
        .output()
        .expect("powershell");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[cfg(target_os = "macos")]
fn command_line_of(pid: u32) -> String {
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn command_line_of(pid: u32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " "))
        .unwrap_or_default()
}

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

// ------------------------------------------------------- more behaviours

#[tokio::test]
async fn an_idle_keep_session_wakes_on_a_background_child() {
    let mock = Mock::start_routed(vec![], |request| {
        if last_text(request).starts_with("[child ses_") {
            return Some(say("woke up"));
        }
        let task = task_of(request);
        let results = tool_results(request).len();
        Some(match (task.as_str(), results) {
            ("ROOT", 0) => bash("call_bg", &child_cmd("--background", "QUICK look")),
            ("ROOT", _) => say("started"),
            (quick, _) if quick.starts_with("QUICK") => {
                Reply::SseAfter(Duration::from_secs(2), sse(&text_turn("quick answer", 0)))
            }
            _ => return None,
        })
    })
    .await;
    let mut root = LiveSession::start(&mock, &["run", "--keep", "--json", "ROOT"], "").await;
    let started = Instant::now();
    loop {
        let log = root.log();
        let assistants: Vec<String> = log
            .iter()
            .filter(|event| event["type"] == "assistant")
            .map(|event| event["text"].as_str().unwrap_or("").to_string())
            .collect();
        if assistants.iter().any(|text| text == "woke up") {
            // The turn ended on "started" first: the session was idle when the
            // child settled, and the settlement started a new turn.
            assert!(
                assistants.iter().any(|text| text == "started"),
                "{assistants:?}"
            );
            let wake = log
                .iter()
                .find(|event| event["type"] == "user" && event["from"] == "child")
                .expect("the wake message");
            assert!(wake["text"].as_str().unwrap().contains("quick answer"));
            break;
        }
        assert!(
            started.elapsed() < CEILING,
            "the idle session never woke: {assistants:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    root.kill().await;
}

#[tokio::test]
async fn ctl_brief_prints_the_answer_and_leaves_the_history_alone() {
    let mock = Mock::start_routed(vec![], |request| {
        Some(if is_brief(request) {
            say("ctl brief answer")
        } else {
            say("hello")
        })
    })
    .await;
    let mut root = LiveSession::start(&mock, &["run", "--keep", "--json", "ROOT"], "").await;
    // Wait for the first turn to end so the snapshot has an answer in it.
    let started = Instant::now();
    while !root.log().iter().any(|event| event["type"] == "turn_ended") {
        assert!(started.elapsed() < CEILING);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let answer = root.ctl(&["brief", "BRIEFQ what did you say?"]).await;
    assert_eq!(answer.code, 0, "{} {}", answer.stdout, answer.stderr);
    assert!(
        answer.stdout.contains("ctl brief answer"),
        "{}",
        answer.stdout
    );
    let log = root.log();
    assert_eq!(
        of_type(&log, "user").len(),
        1,
        "the question is not in the history"
    );
    assert_eq!(
        calls_of(&log, "call_started", "child")[0]["info"]["name"],
        "brief"
    );
    let verbs = of_type(&log, "verb");
    assert!(verbs.iter().any(|verb| verb["name"] == "brief"));
    // The brief asked about a finished conversation: its last message is the
    // question, after the parent's own answer.
    let brief: Vec<Recorded> = mock.requests().into_iter().filter(is_brief).collect();
    let messages = brief[0].messages();
    assert_eq!(messages[messages.len() - 2]["role"], "assistant");
    root.kill().await;
}

#[tokio::test]
async fn only_the_root_writes_the_herdr_status_and_a_long_answer_settles() {
    let long = "x".repeat(100_000);
    let long_for_mock = long.clone();
    let mock = Mock::start_routed(vec![], move |request| {
        let task = task_of(request);
        let done = !tool_results(request).is_empty();
        Some(match (task.as_str(), done) {
            ("ROOT", false) => bash("call_a", &child_cmd("", "CHILD-A work")),
            ("ROOT", true) => say("root done"),
            (a, false) if a.starts_with("CHILD-A") => {
                bash("call_b", &child_cmd("", "CHILD-B leaf"))
            }
            (a, true) if a.starts_with("CHILD-A") => say("A done"),
            (b, _) if b.starts_with("CHILD-B") => say(&long_for_mock),
            _ => return None,
        })
    })
    .await;
    let family = Family::new("");
    // Not `--json`: this is the run that may write OSC status records.
    let out = family
        .sandbox
        .kelir(&mock.base_url(), &["run", "ROOT"], &[("KELIR_HERDR", "1")])
        .await;
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("\u{1b}]7501;"),
        "the root writes its status: {:?}",
        out.stdout
    );
    for (id, task, log) in family.by_task() {
        if task.starts_with("CHILD") {
            for event in &log {
                let text = event.to_string();
                // The record is the OSC sequence `ESC ] 7501 ;`. Matching the bare
                // digits "7501" was a flake: a pid, a byte count or a temp-dir
                // name in a child's log contains them now and then (STEER-9,
                // seen once in five loaded runs).
                assert!(
                    !text.contains("\u{1b}]7501") && !text.contains("\\u001b]7501"),
                    "a child wrote a status record: {id}"
                );
            }
        }
    }
    let (_, root) = family.session_for("ROOT");
    let (b_id, _) = family.session_for("CHILD-B");
    // The grandchild's 100 KB answer reached the root's registry (cut to what a
    // request line carries) and the middle agent's whole tool result.
    let settled = calls_of(&root, "call_settled", "child");
    assert_eq!(settled.len(), 2, "{settled:?}");
    let child_starts = calls_of(&root, "call_started", "child");
    let deep = settled
        .iter()
        .find(|event| {
            child_starts.iter().any(|start| {
                start["id"] == event["id"] && start["info"]["session"] == b_id.as_str()
            })
        })
        .unwrap();
    assert_eq!(deep["outcome"], "done");
    let (_, a) = family.session_for("CHILD-A");
    let call_b = a
        .iter()
        .find(|event| event["type"] == "call_settled" && event["id"] == "call_b")
        .unwrap();
    assert_eq!(call_b["bytes"].as_u64().unwrap() as usize, long.len() + 1);
}
