//! M5-B review (QA): adversarial tests for the Herdr line, the pushed hint, memory, media notes and
//! `observe`, against the mock provider. A failing test here is a finding (see docs/reviews/m5b-review.md).

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::support::*;
use serde_json::{Value, json};

// ------------------------------------------------------------------ helpers

pub(super) fn of_type<'a>(log: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|event| event["type"] == event_type)
        .collect()
}

pub(super) fn toml_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub(super) fn text_of(value: &Value) -> String {
    value
        .as_str()
        .or_else(|| value["preview"].as_str())
        .unwrap_or_default()
        .to_string()
}

pub(super) fn write_skill(root: &Path) {
    let skill = root.join(".claude").join("skills").join("sift");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: sift\ndescription: use when a log is too long to read\n---\n\nBODY-SECRET\n",
    )
    .unwrap();
}

pub(super) fn scorer_reply(names: &[&str]) -> Reply {
    let content = serde_json::to_string(names).unwrap();
    Reply::Sse(
        json!({"choices": [{"message": {"role": "assistant", "content": content}}],
               "usage": {"prompt_tokens": 50, "completion_tokens": 7}})
        .to_string(),
    )
}

const HINT: &str = "[gates.hint]\nbuiltin = 'hint'\nmodel = 'scorer/model'\n";

/// A directory holding a fake `hc` of the given flavour, and the PATH that finds it first.
pub(super) struct FakeHc {
    pub(super) _dir: tempfile::TempDir,
    pub(super) path_value: String,
}

pub(super) enum Hc<'a> {
    Print(&'a str),
    PrintFile(PathBuf),
    Hang(u32),
}

pub(super) fn fake_hc(kind: Hc<'_>) -> FakeHc {
    let dir = tempfile::tempdir().unwrap();
    match &kind {
        Hc::Print(json) => std::fs::write(dir.path().join("hc.json"), json).unwrap(),
        Hc::PrintFile(path) => {
            std::fs::copy(path, dir.path().join("hc.json")).unwrap();
        }
        Hc::Hang(_) => {}
    }
    #[cfg(windows)]
    {
        let body = match &kind {
            Hc::Hang(marker) => format!("@echo off\r\nping -n {marker} 127.0.0.1 >nul\r\n"),
            _ => "@echo off\r\ntype \"%~dp0hc.json\"\r\n".to_string(),
        };
        std::fs::write(dir.path().join("hc.cmd"), body).unwrap();
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let body = match &kind {
            Hc::Hang(marker) => format!("#!/bin/sh\nsleep {marker}\n"),
            _ => "#!/bin/sh\ncat \"$(dirname \"$0\")/hc.json\"\n".to_string(),
        };
        let file = dir.path().join("hc");
        std::fs::write(&file, body).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(&existing));
    let path_value = std::env::join_paths(paths)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    FakeHc {
        _dir: dir,
        path_value,
    }
}

pub(super) async fn run_in_pane(hc: &FakeHc) -> (Run, Duration) {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let started = Instant::now();
    let run = run_task(
        &mock,
        &["run", "--json", "hello"],
        &[("PATH", hc.path_value.as_str()), ("HC_TERMINAL_ID", "t9")],
        None,
        "",
    )
    .await;
    (run, started.elapsed())
}

pub(super) fn system_of_run(run: &Run) -> String {
    text_of(&run.one_event("session_started")["system"]["text"])
}

/// How many processes have a command line containing `needle` (a unique marker such as `-n 37`).
pub(super) fn processes_with(needle: &str) -> usize {
    #[cfg(windows)]
    {
        let script = format!(
            "(Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -like '*{needle}*' -and $_.Name -in @('ping.exe','cmd.exe') }} | Measure-Object).Count"
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .unwrap_or(0)
    }
    #[cfg(not(windows))]
    {
        let out = std::process::Command::new("pgrep")
            .args(["-f", needle])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).lines().count()
    }
}

// ------------------------------------------------------------- (1) Herdr line
// --------------------------------------------------------------- (2) the hint

/// Hint call: a slow scorer is cut at the 2 s default, one error note, the turn still runs.
#[tokio::test]
async fn a_slow_scorer_costs_about_two_seconds_and_no_hint() {
    let sandbox_cwd = tempfile::tempdir().unwrap();
    write_skill(sandbox_cwd.path());
    let mock = Mock::start(vec![
        Reply::SseAfter(
            Duration::from_secs(8),
            json!({"choices": [{"message": {"content": "[\"sift\"]"}}]}).to_string(),
        ),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    let started = Instant::now();
    let run = run_task(
        &mock,
        &[
            "run",
            "--json",
            "--cwd",
            &sandbox_cwd.path().to_string_lossy(),
            "this log is too long",
        ],
        &[],
        None,
        HINT,
    )
    .await;
    let took = started.elapsed();
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        took < Duration::from_secs(7),
        "waited for the scorer: {took:?}"
    );
    assert!(
        took >= Duration::from_millis(1800),
        "gave up early: {took:?}"
    );
    assert!(run.events_of("source").is_empty());
    let notes: Vec<_> = run
        .events_of("error")
        .into_iter()
        .filter(|e| e["where"] == "gate")
        .collect();
    assert_eq!(notes.len(), 1, "{notes:?}");
}

/// The configured `endpoint` is a different host from the provider: the session provider's key must
/// not be sent to it (a leak of the OpenRouter key to whatever URL `[hint] endpoint` names).
#[tokio::test]
async fn the_session_key_is_not_sent_to_a_different_hint_endpoint() {
    let cwd = tempfile::tempdir().unwrap();
    write_skill(cwd.path());
    let other = Mock::start(vec![scorer_reply(&["sift"])]).await;
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let config = format!(
        "[gates.hint]\nbuiltin = 'hint'\nmodel = 'scorer/model'\nendpoint = \"{}\"\n",
        other.base_url()
    );
    let run = run_task(
        &mock,
        &[
            "run",
            "--json",
            "--cwd",
            &cwd.path().to_string_lossy(),
            "this log is too long",
        ],
        &[],
        None,
        &config,
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let seen = other.requests();
    assert_eq!(seen.len(), 1, "the other endpoint was called once");
    assert_eq!(
        seen[0].header("authorization"),
        None,
        "the session's OpenRouter key was sent to a different host: {:?}",
        seen[0].header("authorization")
    );
}

/// One scoring call per human prompt, however many steps the turn takes; a resumed process does not
/// re-score the prompts already in the log.
#[tokio::test]
async fn one_scoring_call_per_prompt_across_steps_and_none_on_resume() {
    let cwd = tempfile::tempdir().unwrap();
    write_skill(cwd.path());
    let mock = Mock::start(vec![
        scorer_reply(&["sift"]),
        Reply::Sse(sse(&tool_turn(
            "c1",
            "bash",
            &json!({"command": "echo hi"}),
        ))),
        Reply::Sse(sse(&tool_turn(
            "c2",
            "bash",
            &json!({"command": "echo hi"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
        // The resume + one new prompt: a second scoring call, then its step.
        scorer_reply(&["sift"]),
        Reply::Sse(sse(&text_turn("again", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &[
            "run",
            "--keep",
            "--json",
            "--cwd",
            &cwd.path().to_string_lossy(),
            "this log is too long",
        ],
        HINT,
    )
    .await;
    live.wait_for("turn_ended").await;
    let requests = mock.requests();
    let scoring = requests
        .iter()
        .filter(|r| r.body["stream"] == false)
        .count();
    assert_eq!(scoring, 1, "3 steps, 1 prompt: one scoring call");
    let hints: Vec<_> = live
        .log()
        .into_iter()
        .filter(|e| e["type"] == "source" && e["kind"] == "hint")
        .collect();
    assert_eq!(hints.len(), 1);
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

// ------------------------------------------------------------------ (3) memory
#[cfg(unix)]
// --------------------------------------------------------------- (4) attachments
// ------------------------------------------------------------------ (5) observe

/// Observe at a held (in-flight) request equals that request's wire body, byte for byte.
pub(super) async fn observe_equals(
    mock: &Mock,
    sandbox: &Sandbox,
    live: &LiveSession,
    request_index: usize,
    label: &str,
) {
    eventually(
        &format!("{label}: the request to arrive"),
        || mock.request_count() > request_index,
        || live.dump(),
    )
    .await;
    let wire = mock.requests()[request_index].raw_body.clone();
    let raw = sandbox
        .kelir(&mock.base_url(), &["observe", &live.id, "--raw"], &[])
        .await;
    assert_eq!(raw.code, 0, "{label}: {}", raw.stderr);
    if raw.stdout != wire {
        let a: Value = serde_json::from_str(&raw.stdout).unwrap();
        let b: Value = serde_json::from_str(&wire).unwrap();
        let mut diff = Vec::new();
        for key in [
            "model",
            "tools",
            "reasoning_effort",
            "parallel_tool_calls",
            "messages",
        ] {
            if a[key] != b[key] {
                diff.push(key);
            }
        }
        panic!(
            "{label}: observe --raw differs from the wire body; differing keys {diff:?}; observe has {} messages, wire {}",
            a["messages"].as_array().map_or(0, Vec::len),
            b["messages"].as_array().map_or(0, Vec::len)
        );
    }
}

#[tokio::test]
async fn observe_equals_the_wire_after_a_source_change() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first", 0))),
        Reply::Gated(gate.clone(), sse(&text_turn("second", 0))),
    ])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    std::fs::write(live.cwd.join("AGENTS.md"), "RULES v1").unwrap();
    live.wait_for("turn_ended").await;
    std::fs::write(live.cwd.join("AGENTS.md"), "RULES v2 changed").unwrap();
    let sandbox = Sandbox::from_live(&live);
    live.ctl(&["say", "again"]).await;
    observe_equals(&mock, &sandbox, &live, 1, "source change").await;
    gate.notify_one();
    live.wait_for("turn_ended").await;
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn observe_equals_the_wire_after_a_hint() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        scorer_reply(&[]),
        Reply::Sse(sse(&text_turn("first", 0))),
        scorer_reply(&["sift"]),
        Reply::Gated(gate.clone(), sse(&text_turn("second", 0))),
    ])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], HINT).await;
    live.wait_for("turn_ended").await;
    write_skill(&live.cwd);
    let sandbox = Sandbox::from_live(&live);
    live.ctl(&["say", "this log is too long"]).await;
    observe_equals(&mock, &sandbox, &live, 3, "hint").await;
    let body = mock.requests()[3].body.to_string();
    assert!(body.contains("might be relevant: sift"), "{body}");
    gate.notify_one();
    live.wait_for("turn_ended").await;
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn observe_equals_the_wire_after_a_model_swap() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first", 0))),
        Reply::Gated(gate.clone(), sse(&text_turn("second", 0))),
    ])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    live.wait_for("turn_ended").await;
    let swap = live.ctl(&["model", "anthropic/claude-sonnet-4.5"]).await;
    assert_eq!(swap.code, 0, "{}{}", swap.stdout, swap.stderr);
    let sandbox = Sandbox::from_live(&live);
    live.ctl(&["say", "again"]).await;
    observe_equals(&mock, &sandbox, &live, 1, "model swap").await;
    assert_eq!(
        mock.requests()[1].body["model"],
        "anthropic/claude-sonnet-4.5"
    );
    gate.notify_one();
    live.wait_for("turn_ended").await;
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn observe_equals_the_wire_on_a_retry() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Http(503, "{\"error\":\"busy\"}".into()),
        Reply::Gated(gate.clone(), sse(&text_turn("second", 0))),
    ])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    let sandbox = Sandbox::from_live(&live);
    observe_equals(&mock, &sandbox, &live, 1, "retry").await;
    assert_eq!(
        mock.requests()[0].raw_body,
        mock.requests()[1].raw_body,
        "a retry re-sends the identical body"
    );
    gate.notify_one();
    live.wait_for("turn_ended").await;
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn observe_equals_the_wire_after_a_compaction() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn(&"a long answer ".repeat(400), 0))),
        Reply::Sse(sse(&text_turn("SUMMARY OF EARLIER WORK", 0))),
        Reply::Gated(gate.clone(), sse(&text_turn("after", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "compact_keep = 0
",
    )
    .await;
    live.wait_for("turn_ended").await;
    let compacted = live.ctl(&["compact"]).await;
    assert_eq!(
        compacted.code, 0,
        "{}{}",
        compacted.stdout, compacted.stderr
    );
    let sandbox = Sandbox::from_live(&live);
    live.ctl(&["say", "continue"]).await;
    observe_equals(&mock, &sandbox, &live, 2, "compaction").await;
    gate.notify_one();
    live.wait_for("turn_ended").await;
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn observe_on_damaged_or_missing_sessions_fails_cleanly() {
    let sandbox = Sandbox::new("");
    let base = "http://127.0.0.1:9/api/v1";
    // A session dir with no events, one with a torn first line, one with only garbage.
    for (name, content) in [
        ("ses_empty", ""),
        ("ses_torn", "{\"type\":\"session_sta"),
        ("ses_garbage", "not json\nstill not json\n"),
    ] {
        let dir = sandbox.state.join("sessions").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events.jsonl"), content).unwrap();
        let out = sandbox.kelir(base, &["observe", name], &[]).await;
        assert_ne!(out.code, 0, "{name}: {}", out.stdout);
        assert!(
            !out.stderr.contains("panicked"),
            "{name}: panic: {}",
            out.stderr
        );
        assert!(!out.stderr.trim().is_empty(), "{name}: no explanation");
    }
    let out = sandbox.kelir(base, &["observe", "ses_nope"], &[]).await;
    assert_ne!(out.code, 0);
    assert!(!out.stderr.contains("panicked"));
}

/// A viewed image (TUI v2 pointer path) and a media note together: observe still equals the wire,
/// the image is sent once, and the human view does not dump base64.
#[tokio::test]
async fn observe_equals_the_wire_with_an_image_and_a_media_note() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first", 0))),
        Reply::Gated(gate.clone(), sse(&text_turn("second", 0))),
    ])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    live.wait_for("turn_ended").await;
    // A 1x1 PNG.
    let png: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0,
        0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0x0d, 0x49, 0x44,
        0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0, 5, 0, 1, 0xff, 0x89, 0x99,
        0x3d, 0x1d, 0, 0, 0, 0, 0x49, 0x45, 0x4e,
    ];
    let image = live.cwd.join("pic.png");
    std::fs::write(&image, png).unwrap();
    let audio = live.cwd.join("memo.wav");
    std::fs::write(&audio, b"RIFFxxxx").unwrap();
    for path in [&image, &audio] {
        let answer = live.ctl(&["view", "--path", &path.to_string_lossy()]).await;
        assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    }
    let sandbox = Sandbox::from_live(&live);
    live.ctl(&["say", "look"]).await;
    observe_equals(&mock, &sandbox, &live, 1, "image + media note").await;
    let body = mock.requests()[1].body.to_string();
    assert_eq!(
        body.matches("data:image").count(),
        1,
        "the image is sent once"
    );
    let human = sandbox
        .kelir(&mock.base_url(), &["observe", &live.id], &[])
        .await;
    assert!(
        !human.stdout.contains("base64,"),
        "observe dumped image bytes"
    );
    gate.notify_one();
    live.wait_for("turn_ended").await;
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}
