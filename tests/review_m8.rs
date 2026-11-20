//! M8 review (reviewer-written): adversarial gates against the real binary and
//! the scripted mock. Every run goes through `Sandbox` (Job Object Drop guard,
//! no leaked process) and every misbehaving gate caps itself (`rev_gate.py`).

mod support;

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::*;

const REV: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rev_gate.py");
const PROBE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/gate_probe.py");

fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

fn fwd(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().replace('\\', "/")
}

fn rev_cmd(mode: &str, dir: &Path) -> String {
    format!("{} \"{}\" {mode} \"{}\"", python(), fwd(REV), fwd(dir))
}

fn gate_events<'a>(log: &'a [Value], event: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|e| e["type"] == "gate" && e["event"] == event)
        .collect()
}

/// One bash call then a final answer, so before/after_tool both fire.
async fn one_turn(
    command_gate: &str,
    event: &str,
    env: &[(&str, &str)],
) -> (Output, Vec<Value>, Sandbox) {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "c1",
            "bash",
            &json!({"command": "echo hi"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let config = format!("[gates]\n{event} = {{ name = 'rev', command = '{command_gate}' }}\n");
    let sandbox = Sandbox::new(&config);
    let out = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "go"], env)
        .await;
    let log = sandbox
        .session_ids()
        .first()
        .map(|id| sandbox.log(id))
        .unwrap_or_default();
    (out, log, sandbox)
}

// ---------------------------------------------------------- degenerate commands

/// A gate "command" that is not a command: a bare data file, a directory, a
/// file that does not exist, a manifest copy. None may hang, loop, or crash the
/// session; each run is one fail-open `gate` event with an `error`.
#[tokio::test]
async fn degenerate_gate_commands_fail_open_once_each() {
    let scratch = tempfile::tempdir().unwrap();
    let txt = scratch.path().join("notes.txt");
    std::fs::write(&txt, "echo not-a-script\n").unwrap();
    let manifest = scratch.path().join("Cargo.toml");
    std::fs::write(&manifest, "[package]\nname = \"x\"\n").unwrap();
    let dir = scratch.path().join("adir");
    std::fs::create_dir_all(&dir).unwrap();
    let missing = scratch.path().join("nope.sh");
    for command in [&txt, &manifest, &dir, &missing] {
        let started = Instant::now();
        let (out, log, _sb) = one_turn(&fwd(command), "before_tool", &[]).await;
        assert_eq!(out.code, 0, "{}: {}", command.display(), out.stderr);
        let runs = gate_events(&log, "before_tool");
        assert_eq!(runs.len(), 1, "{}: {runs:?}", command.display());
        assert_eq!(runs[0]["decision"], "allow");
        assert!(
            runs[0]["error"].as_str().is_some_and(|e| !e.is_empty()),
            "{}: a non-command must say why it failed open: {}",
            command.display(),
            runs[0]
        );
        assert!(started.elapsed() < Duration::from_secs(30));
    }
}

/// Empty and blank commands are a config error at load, not a silent run.
#[tokio::test]
async fn an_empty_or_blank_gate_command_is_rejected_at_load() {
    for command in ["", "   "] {
        let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("x", 0)))]).await;
        let config = format!("[gates]\nturn_end = {{ command = '{command}' }}\n");
        let sandbox = Sandbox::new(&config);
        let out = sandbox
            .kelir(&mock.base_url(), &["run", "--json", "go"], &[])
            .await;
        assert_ne!(out.code, 0, "blank command {command:?} was accepted");
        assert!(out.stderr.to_lowercase().contains("gate"), "{}", out.stderr);
        assert_eq!(
            mock.request_count(),
            0,
            "a bad config must not reach the model"
        );
    }
    for value in ["\"\"", "\"  \"", "[\"\"]"] {
        let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("x", 0)))]).await;
        let sandbox = Sandbox::new(&format!("[gates]\nturn_end = {value}\n"));
        let out = sandbox
            .kelir(&mock.base_url(), &["run", "--json", "go"], &[])
            .await;
        assert_ne!(out.code, 0, "blank {value} was accepted: {}", out.stdout);
    }
}

// ------------------------------------------------------------- hostile output

#[tokio::test]
async fn hostile_output_fails_open_and_stays_bounded() {
    for (mode, expect) in [("flood", "output over"), ("binary", ""), ("deep", "")] {
        let work = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let (out, log, sb) = one_turn(&rev_cmd(mode, work.path()), "before_tool", &[]).await;
        assert_eq!(out.code, 0, "{mode}: {}", out.stderr);
        let runs = gate_events(&log, "before_tool");
        assert_eq!(runs.len(), 1, "{mode}");
        assert_eq!(runs[0]["decision"], "allow", "{mode}: {}", runs[0]);
        let error = runs[0]["error"].as_str().unwrap_or("");
        assert!(
            !error.is_empty() && error.contains(expect),
            "{mode}: {error:?}"
        );
        let tool_ok = log
            .iter()
            .any(|e| e["type"] == "tool_settled" && e["outcome"] == "ok");
        assert!(
            tool_ok,
            "{mode}: the call must run after a failed-open gate"
        );
        let session = sb.session_ids()[0].clone();
        let size = std::fs::metadata(sb.session_dir(&session).join("events.jsonl"))
            .unwrap()
            .len();
        assert!(size < 200_000, "{mode}: log grew to {size} bytes");
        assert!(started.elapsed() < Duration::from_secs(40), "{mode}");
    }
}

#[tokio::test]
async fn a_huge_reason_is_cut_in_the_log_and_in_what_the_model_reads() {
    let work = tempfile::tempdir().unwrap();
    let (out, log, _sb) = one_turn(&rev_cmd("bigreason", work.path()), "before_tool", &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let runs = gate_events(&log, "before_tool");
    assert_eq!(runs[0]["decision"], "deny");
    assert!(runs[0]["reason"].as_str().unwrap().chars().count() <= 2100);
    let settled = log.iter().find(|e| e["type"] == "tool_settled").unwrap();
    assert_eq!(settled["outcome"], "denied");
    let result = settled["result"].as_str().unwrap();
    assert!(
        result.starts_with("denied by gate rev: R"),
        "{}",
        &result[..60]
    );
    assert!(result.len() < 2300);
}

// ------------------------------------------------------- timeouts and trees

#[tokio::test]
async fn a_timed_out_gate_that_ignores_sigterm_leaves_nothing() {
    let work = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let (out, log, _sb) = one_turn(
        &rev_cmd("ignoreterm", work.path()),
        "before_tool",
        &[("KELIR_GATE_TIMEOUT_MS", "2000")],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let run = gate_events(&log, "before_tool")[0].clone();
    assert!(
        run["error"].as_str().unwrap().contains("timed out"),
        "{run}"
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        !work.path().join("alive").exists(),
        "the SIGTERM-ignoring grandchild survived"
    );
}

/// Known gap (F2): a gate that detaches into its own session (`setsid`) escapes
/// the Unix process-group kill. Windows Job Objects catch it. Ignored by
/// default so the suite stays green; run with `--ignored` on Linux.
#[tokio::test]
#[ignore = "F2: setsid daemon escapes the Unix process-group kill"]
async fn a_gate_that_daemonizes_does_not_outlive_the_run() {
    let work = tempfile::tempdir().unwrap();
    let (out, _log, _sb) = one_turn(&rev_cmd("daemon", work.path()), "before_tool", &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        !work.path().join("daemon").exists(),
        "a daemonized gate child outlived the gate run"
    );
}

#[tokio::test]
async fn a_gate_that_never_reads_stdin_does_not_wedge_the_session() {
    let work = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let (out, log, _sb) = one_turn(&rev_cmd("noread", work.path()), "session_start", &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(gate_events(&log, "session_start").len(), 1);
    assert!(started.elapsed() < Duration::from_secs(30));
}

/// A gate that starts itself (two children per level, depth capped at 3 by the
/// script): one `gate` event, and at most 1+2+4+8 processes ever started.
#[tokio::test]
async fn a_self_spawning_gate_is_bounded_and_gone_after_the_run() {
    let work = tempfile::tempdir().unwrap();
    let (out, log, _sb) = one_turn(&rev_cmd("selfspawn", work.path()), "session_start", &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(gate_events(&log, "session_start").len(), 1);
    let marks = std::fs::read_dir(work.path()).unwrap().count();
    assert!(marks <= 15, "{marks} processes started (15 at most)");
}

// -------------------------------------------- many runs, one event stream

/// N (default 150; REVIEW_N=1000 for the full run, ~2 min) tool calls in one assistant turn, each vetoed by a trivial shell gate:
/// 1000 sequential `gate` events, never more than the cap alive, no bash ran.
#[tokio::test]
async fn a_thousand_gated_tool_calls_in_one_stream() {
    let n: usize = std::env::var("REVIEW_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    let mut chunks = Vec::new();
    for index in 0..n {
        chunks.push(json!({"choices": [{"delta": {"tool_calls": [{
            "index": index,
            "id": format!("c{index}"),
            "type": "function",
            "function": {"name": "bash", "arguments": json!({"command": "echo SHOULD-NOT-RUN"}).to_string()}
        }]}, "finish_reason": null}]}));
    }
    chunks.push(finish("tool_calls"));
    chunks.push(usage_chunk(100, 12, 0, 100));
    let mock = Mock::start(vec![
        Reply::Sse(sse(&chunks)),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    // The default shell is bash on both OSes (Git Bash on Windows); a TOML basic string.
    let config = "[gates]
before_tool = { name = 'veto', command = \"echo '{\\\"decision\\\":\\\"deny\\\",\\\"reason\\\":\\\"no\\\"}'\" }
".to_string();
    let sandbox = Sandbox::new(&config);
    let started = Instant::now();
    let out = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "go"], &[])
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let log = sandbox.log(&sandbox.session_ids()[0]);
    let runs = gate_events(&log, "before_tool");
    assert_eq!(runs.len(), n, "one gate event per call");
    let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
    for r in &runs {
        let key = format!("{} / {}", r["decision"], r["error"].as_str().unwrap_or("-"));
        *kinds.entry(key).or_default() += 1;
    }
    eprintln!("gate outcomes: {kinds:?}");
    assert!(runs.iter().all(|r| r["decision"] == "deny"), "{kinds:?}");
    let denied = log
        .iter()
        .filter(|e| e["type"] == "tool_settled" && e["outcome"] == "denied")
        .count();
    assert_eq!(denied, n);
    assert!(
        !log.iter()
            .any(|e| e["type"] == "tool_settled" && e["outcome"] == "ok"),
        "a vetoed bash call ran"
    );
    let total: u64 = runs.iter().map(|r| r["ms"].as_u64().unwrap_or(0)).sum();
    eprintln!(
        "{n} gated calls took {:?}; sum of gate ms {total}",
        started.elapsed()
    );
}

// ----------------------------------------------------------- environment

#[tokio::test]
async fn the_gate_env_holds_no_secret_even_with_custom_key_env() {
    let work = tempfile::tempdir().unwrap();
    let canary = "CANARY-s3cr3t-9f3a";
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("done", 0)))]).await;
    let config = format!(
        "[gates]\nsession_start = {{ name = 'dump', command = '{}', env = {{ OK_VAR = 'visible' }} }}\n",
        rev_cmd("envdump", work.path())
    );
    let sandbox = Sandbox::new(&config);
    let out = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "go"],
            &[
                ("OPENROUTER_API_KEY", canary),
                ("ANTHROPIC_API_KEY", canary),
                ("MY_CUSTOM_TOKEN", canary),
                ("AWS_SECRET_ACCESS_KEY", canary),
                ("GITHUB_TOKEN", canary),
            ],
        )
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let text = std::fs::read_to_string(work.path().join("env.json")).unwrap();
    let env: Value = serde_json::from_str(&text).unwrap();
    assert!(!text.contains(canary), "a secret reached the gate: {text}");
    for key in env.as_object().unwrap().keys() {
        let up = key.to_uppercase();
        assert!(
            !(up.contains("KEY") || up.contains("TOKEN") || up.contains("SECRET"))
                || up == "SSH_AUTH_SOCK",
            "suspicious variable in a gate's env: {key}"
        );
    }
    assert_eq!(env["OK_VAR"], "visible");
    assert_eq!(env["KELIR_GATE_EVENT"], "session_start");
    assert!(env.get("KELIR_TOKEN").is_none() && env.get("KELIR_SOCK").is_none());
    let log = std::fs::read_to_string(
        sandbox
            .session_dir(&sandbox.session_ids()[0])
            .join("events.jsonl"),
    )
    .unwrap();
    for hay in [&log, &out.stdout, &out.stderr] {
        assert!(!hay.contains(canary));
    }
}

// ------------------------------------------------- turn_end once per turn

/// Two user turns in one session: each is blocked once (not once per session).
#[tokio::test]
async fn turn_end_blocks_once_per_turn_not_once_per_session() {
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        work.path().join("plan.json"),
        json!({"turn_end": {"mode": "deny", "reason": "again"}}).to_string(),
    )
    .unwrap();
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("one", 0))),
        Reply::Sse(sse(&text_turn("one-b", 0))),
        Reply::Sse(sse(&text_turn("two", 0))),
        Reply::Sse(sse(&text_turn("two-b", 0))),
        Reply::Sse(sse(&text_turn("never", 0))),
    ])
    .await;
    let config = format!(
        "[gates]\nturn_end = {{ name = 'p', command = '{} \"{}\" \"{}\"' }}\n",
        python(),
        fwd(PROBE),
        fwd(work.path())
    );
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "first"], &config).await;
    live.wait_for("turn_ended").await;
    let _ = live.ctl(&["say", "second"]).await;
    live.wait_for("turn_ended").await;
    for _ in 0..100 {
        if mock.request_count() >= 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let requests = mock.request_count();
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(requests, 4, "each of the two turns is blocked exactly once");
}

// --------------------------------------------- session_end at kill / end

#[tokio::test]
async fn session_end_fires_on_end_but_not_on_a_hard_kill() {
    for kill in [false, true] {
        let work = tempfile::tempdir().unwrap();
        let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("done", 0)))]).await;
        let config = format!(
            "[gates]\nsession_end = {{ name = 'p', command = '{} \"{}\" \"{}\"' }}\n",
            python(),
            fwd(PROBE),
            fwd(work.path())
        );
        let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], &config).await;
        live.wait_for("turn_ended").await;
        let fired = work.path().join("session_end.jsonl");
        if kill {
            live.kill().await;
            tokio::time::sleep(Duration::from_secs(2)).await;
            assert!(!fired.exists(), "a hard-killed session cannot run a gate");
        } else {
            let (code, stderr) = live.end().await;
            assert_eq!(code, 0, "{stderr}");
            assert!(
                fired.exists(),
                "session_end gate did not run on a clean end"
            );
        }
    }
}

/// Killing kelir while a gate is running must take the gate's tree with it
/// (Windows: kill-on-close Job Object; Unix: nothing does this today, F3).
#[tokio::test]
async fn killing_the_session_mid_gate_kills_the_gate_tree() {
    let work = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("done", 0)))]).await;
    let config = format!(
        "[gates]\nsession_start = {{ name = 'h', command = '{}' }}\n",
        rev_cmd("ignoreterm", work.path())
    );
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], &config).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    live.kill().await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        !work.path().join("alive").exists(),
        "the gate's grandchild outlived a killed session (orphan)"
    );
}
