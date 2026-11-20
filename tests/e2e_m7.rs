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
        let found: Vec<_> = self
            .by_task()
            .into_iter()
            .filter(|(_, task, _)| task.starts_with(task_prefix))
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
