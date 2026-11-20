//! M8 end-to-end: gates against the real binary and a scripted mock provider.
//!
//! The gate is `tests/support/gate_probe.py` (records what it was given, then
//! does what `plan.json` says) or one of the shipped examples. Every run goes
//! through the `Sandbox` Drop-guard, so a gate that leaves a process behind
//! fails the test that started it.

mod support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::*;

const PROBE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/gate_probe.py");
const EXAMPLES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/gates");

fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

fn fwd(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().replace('\\', "/")
}

/// A scratch dir the probe records into and reads its plan from.
struct Probe {
    dir: tempfile::TempDir,
}

impl Probe {
    fn new(plan: Value) -> Probe {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plan.json"), plan.to_string()).unwrap();
        Probe { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn command(&self) -> String {
        format!("{} \"{}\" \"{}\"", python(), fwd(PROBE), fwd(self.dir.path()))
    }

    /// `[gates]` binding the probe to these events (optionally with `tools`).
    fn config(&self, events: &[(&str, Option<&str>)]) -> String {
        let mut text = String::from("[gates]\n");
        for (event, tools) in events {
            let tools = match tools {
                Some(tool) => format!(", tools = ['{tool}']"),
                None => String::new(),
            };
            text.push_str(&format!(
                "{event} = {{ name = 'gate_probe', command = '{}'{tools} }}\n",
                self.command()
            ));
        }
        text
    }

    /// The documents the probe read for `event`, oldest first.
    fn seen(&self, event: &str) -> Vec<Value> {
        std::fs::read_to_string(self.path(&format!("{event}.jsonl")))
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn env_of(&self, event: &str) -> Value {
        let text = std::fs::read_to_string(self.path(&format!("env-{event}.json")))
            .unwrap_or_else(|_| panic!("the probe never ran for {event}"));
        serde_json::from_str(&text).unwrap()
    }
}

struct Done {
    out: Output,
    log: Vec<Value>,
    sandbox: Sandbox,
}

impl Done {
    fn events(&self, kind: &str) -> Vec<&Value> {
        self.log.iter().filter(|e| e["type"] == kind).collect()
    }
    fn gates(&self, event: &str) -> Vec<&Value> {
        self.log
            .iter()
            .filter(|e| e["type"] == "gate" && e["event"] == event)
            .collect()
    }
}

async fn drive(mock: &Mock, config: &str, task: &str, env: &[(&str, &str)]) -> Done {
    let sandbox = Sandbox::new(config);
    let out = sandbox
        .kelir(&mock.base_url(), &["run", "--json", task], env)
        .await;
    let ids = sandbox.session_ids();
    let log = ids.first().map(|id| sandbox.log(id)).unwrap_or_default();
    Done { out, log, sandbox }
}

fn one_call_then(text: &str, command: &str) -> Vec<Reply> {
    vec![
        Reply::Sse(sse(&tool_turn("c1", "bash", &json!({"command": command})))),
        Reply::Sse(sse(&text_turn(text, 0))),
    ]
}

// ------------------------------------------------------------------ six events

#[tokio::test]
async fn each_of_the_six_events_fires_with_its_document() {
    let mock = Mock::start(one_call_then("done", "echo hello-gate")).await;
    let probe = Probe::new(json!({}));
    let config = probe.config(&[
        ("session_start", None),
        ("prompt_submit", None),
        ("before_tool", None),
        ("after_tool", None),
        ("turn_end", None),
        ("session_end", None),
    ]);
    let done = drive(&mock, &config, "check the gates", &[]).await;
    assert_eq!(done.out.code, 0, "{}", done.out.stderr);

    for event in [
        "session_start",
        "prompt_submit",
        "before_tool",
        "after_tool",
        "turn_end",
        "session_end",
    ] {
        let runs = done.gates(event);
        assert_eq!(runs.len(), 1, "{event}: {:?}", done.log);
        assert_eq!(runs[0]["decision"], "allow", "{event}");
        assert!(runs[0]["ms"].is_u64(), "{event}");
        assert!(runs[0].get("error").is_none(), "{event}: {}", runs[0]);
        let docs = probe.seen(event);
        assert_eq!(docs.len(), 1, "{event}");
        let doc = &docs[0];
        assert_eq!(doc["event"], event);
        for key in ["session", "session_dir", "log", "cwd"] {
            assert!(doc[key].as_str().is_some_and(|s| !s.is_empty()), "{event} {key}");
        }
        assert!(doc["log"].as_str().unwrap().ends_with("events.jsonl"));
    }
    assert_eq!(probe.seen("prompt_submit")[0]["text"], "check the gates");
    let before = &probe.seen("before_tool")[0];
    assert_eq!(before["tool"], "bash");
    assert_eq!(before["input"]["command"], "echo hello-gate");
    let after = &probe.seen("after_tool")[0];
    assert!(after["result"].as_str().unwrap().contains("hello-gate"), "{after}");
    assert_eq!(after["outcome"], "ok");
    let end = &probe.seen("turn_end")[0];
    assert_eq!(end["final_text"], "done");
    assert_eq!(end["tool_calls"], 1);
    assert_eq!(probe.seen("session_end")[0]["reason"], "done");
    // The log's order: start gate first, session_end gate before session_ended.
    let types: Vec<&str> = done.log.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(types.last(), Some(&"session_ended"), "{types:?}");
    assert_eq!(types[types.len() - 2], "gate", "{types:?}");
}

// ------------------------------------------------------------------- fail open

#[tokio::test]
async fn crashing_garbage_and_silent_gates_fail_open() {
    let mock = Mock::start(one_call_then("done", "echo still-ran")).await;
    let probe = Probe::new(json!({
        "before_tool": {"mode": "crash"},
        "after_tool": {"mode": "garbage"},
        "session_start": {"mode": "empty"},
    }));
    let config = probe.config(&[
        ("session_start", None),
        ("before_tool", None),
        ("after_tool", None),
    ]);
    let done = drive(&mock, &config, "go", &[]).await;
    assert_eq!(done.out.code, 0, "{}", done.out.stderr);
    let settled = done.events("tool_settled");
    assert_eq!(settled[0]["outcome"], "ok");
    assert!(settled[0]["result"].as_str().unwrap().contains("still-ran"));

    let before = done.gates("before_tool");
    assert_eq!(before[0]["decision"], "allow");
    let error = before[0]["error"].as_str().unwrap();
    assert!(error.contains("exited with 3") && error.contains("crashed on purpose"), "{error}");
    let after = done.gates("after_tool");
    assert!(after[0]["error"].as_str().unwrap().contains("not JSON"), "{:?}", after);
    assert!(done.gates("session_start")[0].get("error").is_none());
}

#[tokio::test]
async fn a_gate_that_hangs_times_out_within_the_limit_and_its_tree_dies() {
    let mock = Mock::start(one_call_then("done", "echo go-on")).await;
    let flag = tempfile::tempdir().unwrap();
    let flag_path = flag.path().join("alive.txt");
    let probe = Probe::new(json!({
        "before_tool": {"mode": "hang", "flag": fwd(&flag_path)},
    }));
    let config = probe.config(&[("before_tool", None)]);
    let started = Instant::now();
    let done = drive(&mock, &config, "go", &[("KELIR_GATE_TIMEOUT_MS", "2000")]).await;
    assert_eq!(done.out.code, 0, "{}", done.out.stderr);
    assert!(started.elapsed() < Duration::from_secs(40), "{:?}", started.elapsed());
    let runs = done.gates("before_tool");
    assert_eq!(runs[0]["decision"], "allow");
    assert!(runs[0]["error"].as_str().unwrap().contains("timed out"), "{}", runs[0]);
    assert!(runs[0]["ms"].as_u64().unwrap() < 15_000, "{}", runs[0]);
    assert_eq!(done.events("tool_settled")[0]["outcome"], "ok");
    // The gate's child would write the flag 3 s after it started.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!flag_path.exists(), "the timed-out gate's child outlived it");
}

// ------------------------------------------------------------------ turn_end

#[tokio::test]
async fn a_hedged_final_answer_is_blocked_once_and_the_turn_continues() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("I think it is probably fine.", 0))),
        Reply::Sse(sse(&text_turn("Still probably fine, I think.", 0))),
        Reply::Sse(sse(&text_turn("never asked for", 0))),
    ])
    .await;
    let script = format!("{EXAMPLES}/verify_before_claim.py");
    let config = format!(
        "[gates]\nturn_end = '{} \"{}\"'\n",
        python(),
        fwd(script)
    );
    let done = drive(&mock, &config, "is it fine?", &[]).await;
    assert_eq!(done.out.code, 0, "{}", done.out.stderr);
    assert_eq!(mock.request_count(), 2, "blocked once, then accepted");

    let runs = done.gates("turn_end");
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0]["decision"], "deny");
    assert!(runs[0]["reason"].as_str().unwrap().contains("hedges"));
    assert_eq!(runs[1]["decision"], "allow", "the example allows once blocked");
    let from_gate: Vec<_> = done
        .events("user")
        .into_iter()
        .filter(|e| e["from"] == "gate")
        .collect();
    assert_eq!(from_gate.len(), 1);
    assert!(from_gate[0]["text"].as_str().unwrap().contains("not accepted"));
    // The second request carried the gate's message.
    let second = &mock.requests()[1];
    assert!(
        second.messages().iter().any(|m| m["content"].as_str().is_some_and(|c| c.contains("not accepted"))),
        "the gate's reason never reached the model"
    );
    assert_eq!(done.out.result().unwrap()["final_text"], "Still probably fine, I think.");
}

#[tokio::test]
async fn a_second_deny_in_one_turn_is_logged_but_not_enforced() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first", 0))),
        Reply::Sse(sse(&text_turn("second", 0))),
        Reply::Sse(sse(&text_turn("third", 0))),
    ])
    .await;
    let probe = Probe::new(json!({"turn_end": {"mode": "deny", "reason": "try again"}}));
    let done = drive(&mock, &probe.config(&[("turn_end", None)]), "go", &[]).await;
    assert_eq!(done.out.code, 0, "{}", done.out.stderr);
    assert_eq!(mock.request_count(), 2);
    assert_eq!(done.gates("turn_end").len(), 2);
    let errors = done.events("error");
    assert!(
        errors.iter().any(|e| e["message"].as_str().unwrap_or("").contains("already blocked")),
        "{errors:?}"
    );
}

#[tokio::test]
async fn a_hedge_after_a_check_is_accepted_and_turn_end_can_rewrite() {
    let mock = Mock::start(one_call_then("probably done", "echo checked")).await;
    let script = format!("{EXAMPLES}/verify_before_claim.py");
    let config = format!("[gates]\nturn_end = '{} \"{}\"'\n", python(), fwd(script));
    let done = drive(&mock, &config, "go", &[]).await;
    assert_eq!(done.gates("turn_end")[0]["decision"], "allow");
    assert_eq!(mock.request_count(), 2);

    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("raw text", 0)))]).await;
    let probe = Probe::new(json!({"turn_end": {"mode": "rewrite", "input": {"text": "polished"}}}));
    let done = drive(&mock, &probe.config(&[("turn_end", None)]), "go", &[]).await;
    assert_eq!(done.gates("turn_end")[0]["decision"], "rewrite");
    assert_eq!(done.gates("turn_end")[0]["rewritten"], true);
    assert_eq!(done.events("turn_ended")[0]["final_text"], "polished");
    assert_eq!(done.out.result().unwrap()["final_text"], "polished");
}

// ----------------------------------------------------------------- before_tool

#[tokio::test]
async fn before_tool_can_rewrite_a_calls_input() {
    let mock = Mock::start(one_call_then("done", "echo original")).await;
    let probe = Probe::new(json!({
        "before_tool": {"mode": "rewrite", "input": {"command": "echo rewritten"}},
    }));
    let done = drive(&mock, &probe.config(&[("before_tool", Some("bash"))]), "go", &[]).await;
    assert_eq!(done.out.code, 0, "{}", done.out.stderr);
    let started = done.events("tool_started");
    assert_eq!(started[0]["input"]["command"], "echo rewritten");
    let settled = done.events("tool_settled");
    assert_eq!(settled[0]["result"], "rewritten\n");
    // What the model asked for is still what its own message says.
    let assistant = done.events("assistant");
    assert_eq!(assistant[0]["tool_calls"][0]["args"]["command"], "echo original");
    assert_eq!(done.gates("before_tool")[0]["rewritten"], true);
}

#[tokio::test]
async fn before_tool_deny_returns_the_gate_and_the_reason_and_runs_nothing() {
    let mock = Mock::start(one_call_then("ok, denied", "echo ran > denied.txt")).await;
    let probe = Probe::new(json!({"before_tool": {"mode": "deny", "reason": "no writes today"}}));
    let done = drive(&mock, &probe.config(&[("before_tool", None)]), "go", &[]).await;
    assert_eq!(done.out.code, 0, "{}", done.out.stderr);
    let settled = done.events("tool_settled");
    assert_eq!(settled[0]["outcome"], "denied");
    let result = settled[0]["result"].as_str().unwrap();
    assert!(
        result.starts_with("denied by gate gate_probe: no writes today"),
        "{result}"
    );
    assert!(!done.sandbox.cwd.join("denied.txt").exists(), "the denied call ran");
    // The model read the denial.
    let second = &mock.requests()[1];
    assert!(second.messages().iter().any(|m| m["content"]
        .as_str()
        .is_some_and(|c| c.contains("denied by gate gate_probe"))));
}

#[tokio::test]
async fn after_tool_can_replace_the_result_or_append_a_note() {
    let mock = Mock::start(one_call_then("done", "echo raw-output")).await;
    let probe = Probe::new(json!({"after_tool": {"mode": "rewrite", "input": {"note": "mind the typo"}}}));
    let done = drive(&mock, &probe.config(&[("after_tool", None)]), "go", &[]).await;
    let result = done.events("tool_settled")[0]["result"].as_str().unwrap().to_string();
    assert!(result.starts_with("raw-output\n"), "{result}");
    assert!(result.contains("[gate gate_probe] mind the typo"), "{result}");

    let mock = Mock::start(one_call_then("done", "echo raw-output")).await;
    let probe = Probe::new(json!({"after_tool": {"mode": "rewrite", "input": {"result": "replaced"}}}));
    let done = drive(&mock, &probe.config(&[("after_tool", None)]), "go", &[]).await;
    assert_eq!(done.events("tool_settled")[0]["result"], "replaced");
}
