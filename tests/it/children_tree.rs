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
pub(super) fn text_of(message: &Value) -> String {
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
pub(super) fn task_of(request: &Recorded) -> String {
    request
        .messages()
        .iter()
        .find(|message| message["role"] == "user")
        .map(text_of)
        .unwrap_or_default()
}

pub(super) fn tool_results(request: &Recorded) -> Vec<String> {
    request
        .messages()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(text_of)
        .collect()
}

pub(super) fn bash(id: &str, command: &str) -> Reply {
    Reply::Sse(sse(&tool_turn(id, "bash", &json!({"command": command}))))
}

pub(super) fn say(text: &str) -> Reply {
    Reply::Sse(sse(&text_turn(text, 0)))
}

/// A reply that never comes until the test opens the gate: a child that is
/// "thinking" for as long as the test needs it to be alive.
pub(super) fn held(gate: &Arc<tokio::sync::Notify>) -> Reply {
    Reply::Gated(gate.clone(), sse(&text_turn("woke", 0)))
}

/// `"$KELIR_BIN" run --child …` as a bash command.
pub(super) fn child_cmd(flags: &str, task: &str) -> String {
    format!("\"$KELIR_BIN\" run --child {flags} \"{task}\"")
}

pub(super) struct Family {
    pub(super) sandbox: Sandbox,
}

impl Family {
    pub(super) fn new(extra_config: &str) -> Family {
        Family {
            sandbox: Sandbox::new(extra_config),
        }
    }

    /// Run a root session to the end.
    pub(super) async fn run(&self, mock: &Mock, task: &str) -> Output {
        self.sandbox
            .kelir(&mock.base_url(), &["run", "--json", task], &[])
            .await
    }

    /// Every session's events, keyed by the task that opened it.
    pub(super) fn by_task(&self) -> Vec<(String, String, Vec<Value>)> {
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

    pub(super) fn session_for(&self, task_prefix: &str) -> (String, Vec<Value>) {
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

pub(super) fn of_type<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["type"] == kind)
        .collect()
}

pub(super) fn calls_of<'a>(events: &'a [Value], kind: &str, call_kind: &str) -> Vec<&'a Value> {
    of_type(events, kind)
        .into_iter()
        .filter(|event| event["kind"] == call_kind)
        .collect()
}

pub(super) fn result_of_call(events: &[Value], call_id: &str) -> String {
    events
        .iter()
        .find(|event| event["type"] == "call_settled" && event["id"] == call_id)
        .map(|event| match &event["result"] {
            Value::String(text) => text.clone(),
            other => other["preview"].as_str().unwrap_or("").to_string(),
        })
        .unwrap_or_default()
}

pub(super) fn pid_alive(pid: u32) -> bool {
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
pub(super) fn leftovers(family: &Family) -> Vec<u32> {
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

pub(super) async fn no_leftovers(family: &Family) {
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
// ----------------------------------------------------------- ls and kill
// ------------------------------------------------- live sessions: orphans
// -------------------------------------------------------------- security

// ------------------------------------------------------- more behaviours

pub(super) async fn wait_gone(pids: &[u32], within: Duration) {
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

pub(super) fn last_text(request: &Recorded) -> String {
    request.messages().last().map(text_of).unwrap_or_default()
}

pub(super) fn is_brief(request: &Recorded) -> bool {
    request
        .messages()
        .iter()
        .any(|message| message["role"] == "user" && text_of(message).starts_with("BRIEFQ"))
}

/// The live children of a root, from `ctl status --json`.
pub(super) async fn child_list(live: &LiveSession) -> Vec<Value> {
    let answer = live.ctl(&["status", "--json"]).await;
    assert_eq!(answer.code, 0, "{}", answer.stderr);
    answer.response()["state"]["child_list"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

pub(super) async fn wait_for_children(live: &LiveSession, count: usize) -> Vec<Value> {
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

pub(super) fn pids_of(list: &[Value]) -> Vec<u32> {
    list.iter()
        .filter_map(|kid| kid["pid"].as_u64())
        .map(|pid| pid as u32)
        .collect()
}

pub(super) fn leftovers_in(state: &std::path::Path, guard: &TreeGuard) -> Vec<u32> {
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

/// A raw request to a session's socket with its token, as any holder of the
/// token file could send.
pub(super) async fn raw(
    live: &LiveSession,
    verb: &str,
    args: Value,
    from: &str,
) -> kelir::ipc::Response {
    let request = kelir::ipc::Request::new(verb, args, from);
    kelir::ipc::request_with_token_file(&live.session_dir, &live.id, &request)
        .await
        .expect("the session answers")
}

pub(super) fn all_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
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
pub(super) fn command_line_of(pid: u32) -> String {
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
pub(super) fn command_line_of(pid: u32) -> String {
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(super) fn command_line_of(pid: u32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " "))
        .unwrap_or_default()
}
