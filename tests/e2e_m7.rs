//! M7 end-to-end: children as a CLI (`kelir run --child`, `kelir brief`,
//! `kelir ls`, `kelir kill`) against a routed mock provider.
//!
//! Several sessions share one mock, so the mock picks each reply from the
//! request itself (`Mock::start_routed`): the first user message says whose
//! turn it is, and the presence of a tool result says how far along it is.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::*;

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

fn result_of_call(events: &[Value], call_id: &str) -> String {
    events
        .iter()
        .find(|event| event["type"] == "tool_settled" && event["call_id"] == call_id)
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
    let started_in_root = of_type(&root, "child_started");
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
        .map(|event| event["depth"].as_u64().unwrap())
        .collect();
    assert_eq!(depths, vec![1, 2]);
    let settled_in_root = of_type(&root, "child_settled");
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
    assert!(
        settled_in_root
            .iter()
            .any(|event| event["session"] == b_id.as_str() && event["result"] == "B result")
    );
    // The middle agent also keeps its own child's pair (it is the parent).
    assert_eq!(of_type(&a, "child_started").len(), 1);
    assert_eq!(of_type(&a, "child_settled").len(), 1);
    // The root's own pair is the only one in its log for its own child.
    assert_eq!(of_type(&b, "child_started").len(), 0);
}

#[tokio::test]
async fn a_background_child_returns_a_handle_and_wakes_the_parent() {
    let mock = Mock::start_routed(vec![], |request| {
        let task = task_of(request);
        let results = tool_results(request).len();
        Some(match (task.as_str(), results) {
            ("ROOT", 0) => bash("call_bg", &child_cmd("--background", "CHILD-BG look")),
            ("ROOT", 1) => bash("call_wait", "sleep 4; echo waited"),
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
        .filter(|request| task_of(request) == "ROOT")
        .next_back()
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
            started.elapsed() < Duration::from_secs(15),
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
    let rcs: Vec<&str> = lines.lines().filter(|line| line.starts_with("rc=")).collect();
    assert_eq!(rcs.len(), 11, "{lines}");
    assert!(rcs[..10].iter().all(|line| line.starts_with("rc=0 ")), "{lines}");
    assert!(rcs[10].starts_with("rc=3 "), "the 11th: {}", rcs[10]);
    assert!(rcs[10].contains("10 live children"), "{}", rcs[10]);
    assert!(rcs[10].contains("limit 10"), "{}", rcs[10]);
    assert_eq!(of_type(&root, "child_started").len(), 10);
    gate.notify_waiters();
    // The sleepers were still thinking when the root ended: none may survive it.
    no_leftovers(&family).await;
    // The root's end killed them; each is settled in its log, not left running.
    let settled = of_type(&root, "child_settled");
    assert_eq!(settled.len(), 0, "the root exited before they settled");
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
            ("ROOT", 1) => bash("call_wait", "sleep 8; echo waited"),
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
    let rcs: Vec<&str> = lines.lines().filter(|line| line.starts_with("rc=")).collect();
    assert_eq!(rcs.len(), 7, "{lines}");
    assert!(rcs[..6].iter().all(|line| line.starts_with("rc=0 ")), "{lines}");
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
    assert!(result.contains("max_child_depth"), "{result}");
    assert!(
        family.by_task().iter().all(|(_, task, _)| !task.starts_with("L5")),
        "no depth-4 session may exist"
    );
    let (_, l1) = family.session_for("L1");
    assert_eq!(of_type(&l1, "child_started").len(), 3, "L2, L3, L4 and nothing for L5");
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
    assert_eq!(brief.len(), 2, "the brief asked, was refused an edit, answered");

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
    let started = of_type(&root, "child_started");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["kind"], "brief");
    assert_eq!(started[0]["parent"], root_id.as_str());
    assert_eq!(of_type(&root, "child_settled")[0]["outcome"], "done");
    assert!(of_type(&root, "source").is_empty());
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
                "for t in NESTK SIB ATTACK; do \"$KELIR_BIN\" run --child --background \"$t x\" >/dev/null; done; sleep 6; \
                 \"$KELIR_BIN\" ls --json; \
                 nk=$(\"$KELIR_BIN\" ls | grep NESTK | grep -o 'ses_[a-z0-9]*' | head -1); \
                 \"$KELIR_BIN\" kill $nk; echo killrc=$?; sleep 2; \"$KELIR_BIN\" ls",
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
    let json_line = text.lines().find(|line| line.starts_with('[')).expect("ls --json");
    let tree: Value = serde_json::from_str(json_line).unwrap();
    assert_eq!(tree[0]["id"], root_id.as_str());
    let kids = tree[0]["children"].as_array().unwrap();
    assert_eq!(kids.len(), 3, "{tree}");
    assert!(kids.iter().all(|kid| kid["depth"] == 1 && kid["kind"] == "child"));
    let nest = kids
        .iter()
        .find(|kid| kid["task"].as_str().unwrap().starts_with("NESTK"))
        .unwrap();
    assert_eq!(nest["children"][0]["depth"], 2);
    for key in ["state", "model", "cost_usd", "cache_hit_ratio", "task", "tree_cost_usd"] {
        assert!(tree[0].get(key).is_some(), "missing {key}");
    }

    // Killing the nest stops it and the sleeper below it.
    assert!(text.contains("killrc=0"), "{text}");
    let settled = of_type(&root, "child_settled");
    let killed: Vec<_> = settled.iter().filter(|e| e["outcome"] == "killed").collect();
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
        lines.iter().all(|line| line.contains("rc=1") && line.contains("refused")),
        "{tries}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with(&attack_id)),
        "it tried itself too: {tries}"
    );
    gate.notify_waiters();
    no_leftovers(&family).await;
}
