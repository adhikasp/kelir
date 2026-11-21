//! M8 end-to-end: gates against the real binary and a scripted mock provider.
//!
//! The gate is `tests/support/gate_probe.py` (records what it was given, then
//! does what `plan.json` says) or one of the shipped examples. Every run goes
//! through the `Sandbox` Drop-guard, so a gate that leaves a process behind
//! fails the test that started it.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
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
        format!(
            "{} \"{}\" \"{}\"",
            python(),
            fwd(PROBE),
            fwd(self.dir.path())
        )
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
            assert!(
                doc[key].as_str().is_some_and(|s| !s.is_empty()),
                "{event} {key}"
            );
        }
        assert!(doc["log"].as_str().unwrap().ends_with("events.jsonl"));
    }
    assert_eq!(probe.seen("prompt_submit")[0]["text"], "check the gates");
    let before = &probe.seen("before_tool")[0];
    assert_eq!(before["tool"], "bash");
    assert_eq!(before["input"]["command"], "echo hello-gate");
    let after = &probe.seen("after_tool")[0];
    assert!(
        after["result"].as_str().unwrap().contains("hello-gate"),
        "{after}"
    );
    assert_eq!(after["outcome"], "ok");
    let end = &probe.seen("turn_end")[0];
    assert_eq!(end["final_text"], "done");
    assert_eq!(end["tool_calls"], 1);
    assert_eq!(probe.seen("session_end")[0]["reason"], "done");
    // The log's order: start gate first, session_end gate before session_ended.
    let types: Vec<&str> = done
        .log
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
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
    assert!(
        error.contains("exited with 3") && error.contains("crashed on purpose"),
        "{error}"
    );
    let after = done.gates("after_tool");
    assert!(
        after[0]["error"].as_str().unwrap().contains("not JSON"),
        "{:?}",
        after
    );
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
    assert!(
        started.elapsed() < Duration::from_secs(40),
        "{:?}",
        started.elapsed()
    );
    let runs = done.gates("before_tool");
    assert_eq!(runs[0]["decision"], "allow");
    assert!(
        runs[0]["error"].as_str().unwrap().contains("timed out"),
        "{}",
        runs[0]
    );
    assert!(runs[0]["ms"].as_u64().unwrap() < 15_000, "{}", runs[0]);
    assert_eq!(done.events("tool_settled")[0]["outcome"], "ok");
    // The gate's child would write the flag 3 s after it started.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        !flag_path.exists(),
        "the timed-out gate's child outlived it"
    );
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
    let config = format!("[gates]\nturn_end = '{} \"{}\"'\n", python(), fwd(script));
    let done = drive(&mock, &config, "is it fine?", &[]).await;
    assert_eq!(done.out.code, 0, "{}", done.out.stderr);
    assert_eq!(mock.request_count(), 2, "blocked once, then accepted");

    let runs = done.gates("turn_end");
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0]["decision"], "deny");
    assert!(runs[0]["reason"].as_str().unwrap().contains("hedges"));
    assert_eq!(
        runs[1]["decision"], "allow",
        "the example allows once blocked"
    );
    let from_gate: Vec<_> = done
        .events("user")
        .into_iter()
        .filter(|e| e["from"] == "gate")
        .collect();
    assert_eq!(from_gate.len(), 1);
    assert!(
        from_gate[0]["text"]
            .as_str()
            .unwrap()
            .contains("not accepted")
    );
    // The second request carried the gate's message.
    let second = &mock.requests()[1];
    assert!(
        second.messages().iter().any(|m| m["content"]
            .as_str()
            .is_some_and(|c| c.contains("not accepted"))),
        "the gate's reason never reached the model"
    );
    assert_eq!(
        done.out.result().unwrap()["final_text"],
        "Still probably fine, I think."
    );
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
        errors.iter().any(|e| e["message"]
            .as_str()
            .unwrap_or("")
            .contains("already blocked")),
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
    let done = drive(
        &mock,
        &probe.config(&[("before_tool", Some("bash"))]),
        "go",
        &[],
    )
    .await;
    assert_eq!(done.out.code, 0, "{}", done.out.stderr);
    let started = done.events("tool_started");
    assert_eq!(started[0]["input"]["command"], "echo rewritten");
    let settled = done.events("tool_settled");
    assert_eq!(settled[0]["result"], "rewritten\n");
    // What the model asked for is still what its own message says.
    let assistant = done.events("assistant");
    assert_eq!(
        assistant[0]["tool_calls"][0]["args"]["command"],
        "echo original"
    );
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
    assert!(
        !done.sandbox.cwd.join("denied.txt").exists(),
        "the denied call ran"
    );
    // The model read the denial.
    let second = &mock.requests()[1];
    assert!(second.messages().iter().any(|m| {
        m["content"]
            .as_str()
            .is_some_and(|c| c.contains("denied by gate gate_probe"))
    }));
}

#[tokio::test]
async fn after_tool_can_replace_the_result_or_append_a_note() {
    let mock = Mock::start(one_call_then("done", "echo raw-output")).await;
    let probe =
        Probe::new(json!({"after_tool": {"mode": "rewrite", "input": {"note": "mind the typo"}}}));
    let done = drive(&mock, &probe.config(&[("after_tool", None)]), "go", &[]).await;
    let result = done.events("tool_settled")[0]["result"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(result.starts_with("raw-output\n"), "{result}");
    assert!(
        result.contains("[gate gate_probe] mind the typo"),
        "{result}"
    );

    let mock = Mock::start(one_call_then("done", "echo raw-output")).await;
    let probe =
        Probe::new(json!({"after_tool": {"mode": "rewrite", "input": {"result": "replaced"}}}));
    let done = drive(&mock, &probe.config(&[("after_tool", None)]), "go", &[]).await;
    assert_eq!(done.events("tool_settled")[0]["result"], "replaced");
}

// --------------------------------------------------------------- prompt_submit

#[tokio::test]
async fn prompt_submit_can_rewrite_the_task() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let probe = Probe::new(
        json!({"prompt_submit": {"mode": "rewrite", "input": {"text": "rewritten task"}}}),
    );
    let done = drive(
        &mock,
        &probe.config(&[("prompt_submit", None)]),
        "original task",
        &[],
    )
    .await;
    assert_eq!(done.out.code, 0, "{}", done.out.stderr);
    assert_eq!(done.events("user")[0]["text"], "rewritten task");
    assert_eq!(done.events("user")[0]["from"], "cli");
    let first = &mock.requests()[0];
    assert!(
        first
            .messages()
            .iter()
            .any(|m| m["content"] == "rewritten task")
    );
    assert!(
        !first
            .messages()
            .iter()
            .any(|m| m["content"] == "original task")
    );
}

#[tokio::test]
async fn a_denied_command_line_task_ends_the_run_with_a_reason_and_no_request() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("never", 0)))]).await;
    let probe = Probe::new(json!({"prompt_submit": {"mode": "deny", "reason": "not today"}}));
    let done = drive(
        &mock,
        &probe.config(&[("prompt_submit", None), ("session_end", None)]),
        "task",
        &[],
    )
    .await;
    assert_eq!(done.out.code, 1, "{}", done.out.stderr);
    assert!(
        done.out
            .stderr
            .contains("prompt dropped by gate gate_probe: not today"),
        "{}",
        done.out.stderr
    );
    assert_eq!(mock.request_count(), 0);
    assert!(done.events("user").is_empty());
    assert_eq!(done.gates("prompt_submit")[0]["decision"], "deny");
    let ended = done.events("session_ended");
    assert_eq!(ended[0]["reason"], "denied");
    assert_eq!(probe.seen("session_end")[0]["reason"], "denied");
}

// ------------------------------------------------------------------- observe

#[tokio::test]
async fn observe_lists_the_gates_and_what_fired_without_secrets() {
    let mock = Mock::start(one_call_then("done", "echo hi")).await;
    let probe = Probe::new(json!({"before_tool": {"mode": "crash"}}));
    let config = probe.config(&[("before_tool", None), ("turn_end", None)]);
    let done = drive(&mock, &config, "go", &[]).await;
    let id = done.sandbox.only_session();
    let out = done
        .sandbox
        .kelir(&mock.base_url(), &["observe", &id], &[])
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("== gates (2) =="), "{}", out.stdout);
    assert!(out.stdout.contains("before_tool") && out.stdout.contains("gate_probe"));
    assert!(
        out.stdout.contains("== gates fired (2) =="),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("failed open: exited with 3"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("turn_end gate_probe-2 -> allow"),
        "{}",
        out.stdout
    );
}

#[tokio::test]
async fn observe_prints_env_names_of_a_gate_but_never_their_values() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let config = "[gates]\nsession_end = { name = 'sync', command = 'true', env = { GATE_SECRET_CANARY = 'canary-value-7731' } }\n";
    let done = drive(&mock, config, "go", &[]).await;
    let id = done.sandbox.only_session();
    let out = done
        .sandbox
        .kelir(&mock.base_url(), &["observe", &id], &[])
        .await;
    assert!(
        out.stdout.contains("GATE_SECRET_CANARY") || out.stdout.contains("sync"),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains("canary-value-7731"), "{}", out.stdout);
    let log = std::fs::read_to_string(done.sandbox.session_dir(&id).join("events.jsonl")).unwrap();
    assert!(!log.contains("canary-value-7731"));
}

// ------------------------------------------------------------- secrets (STEER-7)

#[tokio::test]
async fn a_gate_gets_no_token_no_socket_and_no_provider_key() {
    let mock = Mock::start(one_call_then("done", "echo hi")).await;
    let probe = Probe::new(json!({}));
    let events = [
        ("session_start", None),
        ("prompt_submit", None),
        ("before_tool", None),
        ("after_tool", None),
        ("turn_end", None),
        ("session_end", None),
    ];
    let config = probe.config(&events);
    let canaries = [
        ("OPENROUTER_API_KEY", "canary-openrouter-5c1e"),
        ("KELIR_GO_API_KEY", "canary-go-8d2a"),
        ("MY_OWN_KEY_ENV", "canary-custom-3b7f"),
        ("KELIR_TOKEN", "canary-token-1a9c"),
        ("KELIR_SOCK", "canary-sock-6e4d"),
    ];
    let done = drive(&mock, &config, "go", &canaries).await;
    assert_eq!(done.out.code, 0, "{}", done.out.stderr);
    for (event, _) in events {
        let env = probe.env_of(event);
        let names: Vec<String> = env
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.to_ascii_uppercase())
            .collect();
        for banned in [
            "OPENROUTER_API_KEY",
            "KELIR_GO_API_KEY",
            "MY_OWN_KEY_ENV",
            "KELIR_TOKEN",
            "KELIR_SOCK",
        ] {
            assert!(
                !names.contains(&banned.to_string()),
                "{event}: {banned} reached the gate"
            );
        }
        let text = env.to_string();
        for (_, value) in canaries {
            assert!(
                !text.contains(value),
                "{event}: a canary value reached the gate's environment"
            );
        }
        assert_eq!(env["KELIR_GATE_EVENT"], event);
        assert!(
            env["KELIR_GATE_NAME"]
                .as_str()
                .unwrap()
                .starts_with("gate_probe")
        );
        assert!(env.get("KELIR_SESSION_DIR").is_some());
        for doc in probe.seen(event) {
            let text = doc.to_string();
            for (_, value) in canaries {
                assert!(
                    !text.contains(value),
                    "{event}: a canary reached the gate's stdin"
                );
            }
        }
    }
    // And nothing the session wrote or printed holds a canary either.
    let id = done.sandbox.only_session();
    let log = std::fs::read_to_string(done.sandbox.session_dir(&id).join("events.jsonl")).unwrap();
    for (_, value) in canaries {
        assert!(
            !log.contains(value)
                && !done.out.stdout.contains(value)
                && !done.out.stderr.contains(value)
        );
    }
}

// ------------------------------------------------------------ skill-bound gate

#[tokio::test]
async fn a_skill_script_bound_by_metadata_runs_as_a_gate() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let probe = Probe::new(json!({}));
    let sandbox = Sandbox::new("");
    let skill = sandbox.cwd.join(".kelir").join("skills").join("watch");
    std::fs::create_dir_all(skill.join("scripts")).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: watch\ndescription: watches the end of a turn\nmetadata: {kelir-gate: turn_end}\n---\nBody.\n",
    )
    .unwrap();
    std::fs::write(
        skill.join("scripts").join("turn_end.py"),
        format!(
            "import sys,json\nopen(r'{}','w').write(sys.stdin.read())\nprint(json.dumps({{'decision':'allow','reason':'seen'}}))\n",
            fwd(probe.path("skill-saw.json"))
        ),
    )
    .unwrap();
    let out = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "go"],
            &[("KELIR_TRUST_PROJECT", "1")],
        )
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let log = sandbox.log(&sandbox.only_session());
    let gates: Vec<_> = log.iter().filter(|e| e["type"] == "gate").collect();
    assert_eq!(gates.len(), 1, "{log:?}");
    assert_eq!(gates[0]["gate"], "watch.turn_end");
    assert_eq!(gates[0]["reason"], "seen");
    let seen: Value =
        serde_json::from_str(&std::fs::read_to_string(probe.path("skill-saw.json")).unwrap())
            .unwrap();
    assert_eq!(seen["event"], "turn_end");
}

// -------------------------------------------------------------- example gates

fn run_example(name: &str, stdin: &Value, env: &[(&str, &str)]) -> Value {
    use std::io::Write;
    let mut command = std::process::Command::new(python());
    command
        .arg(format!("{EXAMPLES}/{name}"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in env {
        command.env(k, v);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    kelir::gates::parse_decision(&text).unwrap_or_else(|e| panic!("{name}: {e}: {text}"));
    serde_json::from_str(text.trim()).unwrap_or(Value::Null)
}

#[test]
fn numbered_answers_resolves_do_1_4_against_the_last_list() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    let lines = [
        json!({"seq":1,"type":"assistant","text":"1. old one\n2. old two"}),
        json!({"seq":2,"type":"assistant","text":"Options:\n1. add the cache\n2. drop the flag\n3. rename it\n4. write the docs\n"}),
        json!({"seq":3,"type":"assistant","text":"anything else?"}),
    ];
    std::fs::write(
        &log,
        lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let doc = |text: &str| json!({"event":"prompt_submit","text":text,"log":fwd(&log)});
    let answer = run_example("numbered_answers.py", &doc("do 1,4"), &[]);
    assert_eq!(answer["decision"], "rewrite");
    let text = answer["input"]["text"].as_str().unwrap();
    assert!(
        text.contains("1. add the cache") && text.contains("4. write the docs"),
        "{text}"
    );
    assert!(!text.contains("rename it"));
    let range = run_example("numbered_answers.py", &doc("2-3"), &[]);
    assert!(
        range["input"]["text"]
            .as_str()
            .unwrap()
            .contains("3. rename it")
    );
    for untouched in ["please do the thing", "do 9", "1,4 and also explain why"] {
        assert_eq!(
            run_example("numbered_answers.py", &doc(untouched), &[])["decision"],
            "allow",
            "{untouched}"
        );
    }
}

#[test]
fn note_check_appends_the_checkers_output_and_is_silent_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("note.md");
    std::fs::write(&file, "x").unwrap();
    let script = dir.path().join("checker.py");
    std::fs::write(
        &script,
        "import sys
print('bad: ' + sys.argv[1][-7:])
sys.exit(1)
",
    )
    .unwrap();
    let checker = format!("{} {}", python(), fwd(&script));
    let doc = json!({"event":"after_tool","tool":"edit","outcome":"ok","cwd":fwd(dir.path()),"input":{"path":fwd(&file)},"result":"ok"});
    let answer = run_example("note_check.py", &doc, &[("NOTE_CHECK_CMD", &checker)]);
    assert_eq!(answer["decision"], "rewrite");
    assert!(
        answer["input"]["note"].as_str().unwrap().contains("bad:"),
        "{answer}"
    );
    assert_eq!(run_example("note_check.py", &doc, &[])["decision"], "allow");
}

#[test]
fn git_sync_commits_but_does_not_push_when_told_not_to() {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap()
    };
    if git(&["init", "-q"]).status.code() != Some(0) {
        return;
    }
    git(&["config", "user.email", "t@example.invalid"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
    let doc =
        json!({"event":"session_end","session":"ses_x","cwd":fwd(dir.path()),"reason":"done"});
    let answer = run_example(
        "git_sync.py",
        &doc,
        &[("GIT_SYNC_PUSH", "0"), ("GIT_SYNC_ADD_NEW", "1")],
    );
    assert_eq!(answer["decision"], "allow");
    let log = String::from_utf8_lossy(&git(&["log", "--oneline"]).stdout).to_string();
    assert!(log.contains("kelir: ses_x"), "{log}");
}

/// A scratch repo with one tracked file (`tracked.txt`) committed.
fn git_scratch() -> Option<(tempfile::TempDir, impl Fn(&[&str]) -> std::process::Output)> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let git = move |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap()
    };
    if git(&["init", "-q"]).status.code() != Some(0) {
        return None;
    }
    git(&["config", "user.email", "t@example.invalid"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.path().join("tracked.txt"), "one").unwrap();
    git(&["add", "tracked.txt"]);
    git(&["commit", "-q", "-m", "init"]);
    Some((dir, git))
}

fn end_doc(dir: &std::path::Path) -> Value {
    json!({"event":"session_end","session":"ses_y","cwd":fwd(dir),"reason":"done"})
}

/// F4: by default only tracked files are committed; a new file (here a `.env`)
/// is not staged, so it can never be pushed.
#[test]
fn git_sync_by_default_commits_tracked_changes_only() {
    let Some((dir, git)) = git_scratch() else {
        return;
    };
    std::fs::write(dir.path().join("tracked.txt"), "two").unwrap();
    std::fs::write(dir.path().join(".env"), "TOKEN=abc").unwrap();
    std::fs::write(dir.path().join("new.txt"), "new").unwrap();
    run_example(
        "git_sync.py",
        &end_doc(dir.path()),
        &[("GIT_SYNC_PUSH", "0")],
    );
    let shown = String::from_utf8_lossy(&git(&["show", "--stat", "--format=%s", "HEAD"]).stdout)
        .to_string();
    assert!(
        shown.contains("kelir: ses_y") && shown.contains("tracked.txt"),
        "{shown}"
    );
    assert!(
        !shown.contains(".env") && !shown.contains("new.txt"),
        "{shown}"
    );
}

/// F4: even with `GIT_SYNC_ADD_NEW`, a secret-like name refuses the whole
/// commit and leaves the index clean.
#[test]
fn git_sync_refuses_secret_like_names_even_when_adding_new_files() {
    let Some((dir, git)) = git_scratch() else {
        return;
    };
    std::fs::write(dir.path().join("tracked.txt"), "two").unwrap();
    std::fs::write(dir.path().join(".env"), "TOKEN=abc").unwrap();
    run_example(
        "git_sync.py",
        &end_doc(dir.path()),
        &[("GIT_SYNC_PUSH", "0"), ("GIT_SYNC_ADD_NEW", "1")],
    );
    let log = String::from_utf8_lossy(&git(&["log", "--oneline"]).stdout).to_string();
    assert_eq!(log.lines().count(), 1, "nothing may be committed: {log}");
    let staged =
        String::from_utf8_lossy(&git(&["diff", "--cached", "--name-only"]).stdout).to_string();
    assert!(staged.trim().is_empty(), "index not reset: {staged}");
}

/// F4: a dry run changes nothing (no commit, nothing staged, no push).
#[test]
fn git_sync_dry_run_changes_nothing() {
    let Some((dir, git)) = git_scratch() else {
        return;
    };
    std::fs::write(dir.path().join("tracked.txt"), "two").unwrap();
    std::fs::write(dir.path().join("new.txt"), "new").unwrap();
    let answer = run_example(
        "git_sync.py",
        &end_doc(dir.path()),
        &[("GIT_SYNC_DRY_RUN", "1"), ("GIT_SYNC_ADD_NEW", "1")],
    );
    assert_eq!(answer["decision"], "allow");
    let log = String::from_utf8_lossy(&git(&["log", "--oneline"]).stdout).to_string();
    assert_eq!(log.lines().count(), 1, "{log}");
    let staged =
        String::from_utf8_lossy(&git(&["diff", "--cached", "--name-only"]).stdout).to_string();
    assert!(staged.trim().is_empty(), "{staged}");
    let start = run_example(
        "git_sync.py",
        &json!({"event":"session_start","session":"s","cwd":fwd(dir.path())}),
        &[("GIT_SYNC_DRY_RUN", "1")],
    );
    assert_eq!(start["decision"], "allow");
}

// ----------------------------------------------------------------------- MCP

#[tokio::test]
async fn mcp_calls_are_gated_as_mcp_server_dot_tool() {
    let mcp_script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mcp_server.py");
    let files = tempfile::tempdir().unwrap();
    let mcp_toml = files.path().join("mcp.toml");
    std::fs::write(
        &mcp_toml,
        format!(
            "[servers.demo]\ncommand = '{}'\nargs = ['{}']\n",
            python(),
            mcp_script
        ),
    )
    .unwrap();
    let probe = Probe::new(json!({
        "before_tool": {"mode": "rewrite", "input": {"text": "rewritten-arg"}},
        "after_tool": {"mode": "rewrite", "input": {"note": "mcp noted"}},
    }));
    let config = probe.config(&[
        ("before_tool", Some("mcp:demo")),
        ("after_tool", Some("mcp:*")),
    ]);
    let mock = Mock::start(one_call_then(
        "done",
        "\"$KELIR_BIN\" mcp demo echo text=original",
    ))
    .await;
    let sandbox = Sandbox::new(&config);
    let mcp = mcp_toml.to_string_lossy().into_owned();
    let out = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "go"],
            &[("KELIR_MCP_CONFIG", mcp.as_str())],
        )
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let log = sandbox.log(&sandbox.only_session());
    let before = &probe.seen("before_tool")[0];
    assert_eq!(before["tool"], "mcp:demo.echo", "{before}");
    assert_eq!(before["input"]["text"], "original");
    let after = &probe.seen("after_tool")[0];
    assert_eq!(after["tool"], "mcp:demo.echo");
    assert_eq!(
        after["result"], "rewritten-arg",
        "the rewritten input reached the server"
    );
    let result = log.iter().find(|e| e["type"] == "tool_settled").unwrap()["result"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        result.contains("rewritten-arg") && result.contains("mcp noted"),
        "{result}"
    );
    let gates: Vec<_> = log.iter().filter(|e| e["type"] == "gate").collect();
    assert_eq!(gates.len(), 2, "{gates:?}");

    // A deny returns the gate and the reason as the call's stderr, exit 1.
    let probe =
        Probe::new(json!({"before_tool": {"mode": "deny", "reason": "servers are closed"}}));
    let config = probe.config(&[("before_tool", Some("mcp:demo.echo"))]);
    let mock = Mock::start(one_call_then(
        "done",
        "\"$KELIR_BIN\" mcp demo echo text=x 2>&1; echo exit=$?",
    ))
    .await;
    let sandbox = Sandbox::new(&config);
    let out = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "go"],
            &[("KELIR_MCP_CONFIG", mcp.as_str())],
        )
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let log = sandbox.log(&sandbox.only_session());
    let result = log.iter().find(|e| e["type"] == "tool_settled").unwrap()["result"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        result.contains("denied by gate gate_probe: servers are closed"),
        "{result}"
    );
    let settled = log.iter().find(|e| e["type"] == "mcp_settled").unwrap();
    assert_eq!(settled["outcome"], "denied");
}

// ----------------------------------------------------- a console-less session

#[cfg(windows)]
#[tokio::test]
async fn a_session_without_a_console_runs_its_gates() {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let mock = Mock::start(one_call_then("done", "echo hi")).await;
    let probe = Probe::new(json!({}));
    let config = probe.config(&[("before_tool", None), ("turn_end", None)]);
    let mut live = LiveSession::start_with_flags(
        &mock,
        &["run", "--keep", "--json", "go"],
        &config,
        DETACHED_PROCESS,
    )
    .await;
    live.wait_for("turn_ended").await;
    assert_eq!(probe.seen("before_tool").len(), 1);
    assert_eq!(probe.seen("turn_end").len(), 1);
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

// ------------------------------------------------- a gate that starts kelir again

/// A gate whose command runs kelir with the same config (and so the same gate).
/// Without the guard that is unbounded recursion; the script stops itself at 5
/// so a missing guard shows up as a counter, not a fork bomb.
#[tokio::test]
async fn a_gate_that_starts_kelir_does_not_start_gates_again() {
    let reenter = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/gate_reenter.py");
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("inner done", 0))),
        Reply::Sse(sse(&text_turn("outer done", 0))),
    ])
    .await;
    let config = format!(
        "[gates]
session_start = {{ name = 'again', command = '{} \"{}\" \"{}\"' }}
",
        python(),
        fwd(reenter),
        fwd(dir.path())
    );
    let sandbox = Sandbox::new(&config);
    let plan = json!({
        "bin": env!("CARGO_BIN_EXE_kelir"),
        "args": ["run", "--json", "inner"],
        "env": {
            "KELIR_CONFIG": sandbox.config.to_string_lossy(),
            "KELIR_CONFIG_DIR": sandbox.state.join("no-config").to_string_lossy(),
            "KELIR_STATE_DIR": sandbox.state.to_string_lossy(),
            "KELIR_BASE_URL": mock.base_url(),
            "KELIR_HOME": sandbox.state.join("home").to_string_lossy(),
            "OPENROUTER_API_KEY": "test-openrouter-key",
        },
    });
    std::fs::write(dir.path().join("reenter.json"), plan.to_string()).unwrap();
    let out = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "outer"], &[])
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let runs = std::fs::read_to_string(dir.path().join("count")).unwrap_or_default();
    assert_eq!(runs.lines().count(), 1, "the inner kelir ran gates again");
    let ids = sandbox.session_ids();
    assert_eq!(
        ids.len(),
        2,
        "the gate's kelir should have made a second session"
    );
    let with_gates = ids
        .iter()
        .filter(|id| sandbox.log(id).iter().any(|e| e["type"] == "gate"))
        .count();
    assert_eq!(with_gates, 1, "only the outer session runs gates");
}

// ------------------------------------------------------------- child sessions

#[tokio::test]
async fn a_child_session_runs_its_own_scrubbed_gates_and_status_and_observe_show_them() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "c1",
            "bash",
            &json!({"command": "\"$KELIR_BIN\" run --child \"kid task\""}),
        ))),
        Reply::Sse(sse(&text_turn("kid done", 0))),
        Reply::Sse(sse(&tool_turn(
            "c2",
            "bash",
            &json!({"command": "\"$KELIR_BIN\" ctl \"$KELIR_ROOT\" status"}),
        ))),
        Reply::Sse(sse(&text_turn("root done", 0))),
    ])
    .await;
    let probe = Probe::new(json!({}));
    let events = [
        ("session_start", None),
        ("before_tool", None),
        ("session_end", None),
    ];
    let config = probe.config(&events);
    let canaries = [
        ("OPENROUTER_API_KEY", "canary-openrouter-77aa"),
        ("MY_OWN_KEY_ENV", "canary-custom-91bc"),
        ("KELIR_TOKEN", "canary-token-5d2e"),
    ];
    let done = drive(&mock, &config, "root task", &canaries).await;
    assert_eq!(done.out.code, 0, "{}", done.out.stderr);
    let ids = done.sandbox.session_ids();
    assert_eq!(ids.len(), 2, "{ids:?}");
    let logs: Vec<(String, Vec<Value>)> = ids
        .iter()
        .map(|id| (id.clone(), done.sandbox.log(id)))
        .collect();
    for (id, log) in &logs {
        let fired: Vec<&Value> = log.iter().filter(|e| e["type"] == "gate").collect();
        assert!(
            fired.iter().any(|e| e["event"] == "session_start"),
            "{id} never ran its session_start gate"
        );
        assert!(fired.iter().any(|e| e["event"] == "session_end"), "{id}");
        assert!(
            fired.iter().all(|e| e.get("error").is_none()),
            "{id}: {fired:?}"
        );
    }
    // The last session_start the probe saw is the child's: its own session dir,
    // and nothing secret in its environment.
    let child_id = logs
        .iter()
        .find(|(_, log)| log[0]["parent"].is_string())
        .map(|(id, _)| id.clone())
        .expect("a child session");
    let env = probe.env_of("session_start");
    assert!(
        env["KELIR_SESSION_DIR"]
            .as_str()
            .unwrap()
            .contains(&child_id),
        "{env}"
    );
    for event in ["session_start", "before_tool", "session_end"] {
        let text = probe.env_of(event).to_string();
        for (name, value) in canaries {
            assert!(!text.contains(value), "{event}: {name} reached a gate");
        }
    }
    // The root's `ctl status` result (a tool result in its log) carries the gate counters.
    let root_log = &logs.iter().find(|(id, _)| *id != child_id).unwrap().1;
    let status = root_log
        .iter()
        .filter(|e| e["type"] == "tool_settled")
        .map(|e| e.to_string())
        .find(|text| text.contains("failed open"))
        .unwrap_or_else(|| {
            panic!(
                "no gate counters in ctl status: {:?}",
                root_log
                    .iter()
                    .filter(|e| e["type"] == "tool_settled")
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        status.contains("3 declared") && status.contains("run(s)"),
        "{status}"
    );
    // `observe` on the child lists what fired in it.
    let out = done
        .sandbox
        .kelir(&mock.base_url(), &["observe", &child_id], &[])
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("== gates fired"), "{}", out.stdout);
    assert!(
        out.stdout.contains("session_start gate_probe"),
        "{}",
        out.stdout
    );
}
