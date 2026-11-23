//! M5-B end-to-end: the Herdr line, the pushed hint, memory, media notes and
//! `observe`, against the mock provider (and a fake `hc` on PATH).
//!
//! The headline: `kelir observe <s> --raw` equals, byte for byte, the body the
//! mock received for the request it describes.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::support::*;
use serde_json::{Value, json};

pub(super) fn system_of(request: &Recorded) -> String {
    request.messages()[0]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn of_type<'a>(log: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|event| event["type"] == event_type)
        .collect()
}

/// A project with one skill `sift` (and an optional memory directory).
pub(super) struct Project {
    _temp: tempfile::TempDir,
    cwd: PathBuf,
}

const SKILL: &str = "---\nname: sift\ndescription: use when a log is too long to read\n---\n\n\
Read the tail first. BODY-SECRET\n";

impl Project {
    pub(super) fn new() -> Project {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        let skill = cwd.join(".claude").join("skills").join("sift");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), SKILL).unwrap();
        Project { _temp: temp, cwd }
    }

    pub(super) fn path(&self) -> String {
        self.cwd.to_string_lossy().to_string()
    }

    fn memory(&self) -> PathBuf {
        let dir = self.cwd.join("notes");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

pub(super) fn toml_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

// ------------------------------------------------------------------ fake hc

/// A directory holding a fake `hc` that prints `json` (or fails, or hangs),
/// and the PATH that finds it first.
pub(super) struct FakeHc {
    _dir: tempfile::TempDir,
    path_value: String,
}

#[derive(Clone, Copy)]
pub(super) enum HcMode {
    Print,
    Fail,
    Hang,
}

pub(super) fn fake_hc(json: &str, mode: HcMode) -> FakeHc {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hc.json"), json).unwrap();
    #[cfg(windows)]
    {
        let body = match mode {
            HcMode::Print => "@echo off\r\ntype \"%~dp0hc.json\"\r\n",
            HcMode::Fail => "@echo off\r\necho boom 1>&2\r\nexit /b 3\r\n",
            HcMode::Hang => "@echo off\r\nping -n 30 127.0.0.1 >nul\r\n",
        };
        std::fs::write(dir.path().join("hc.cmd"), body).unwrap();
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let body = match mode {
            HcMode::Print => "#!/bin/sh\ncat \"$(dirname \"$0\")/hc.json\"\n",
            HcMode::Fail => "#!/bin/sh\necho boom >&2\nexit 3\n",
            HcMode::Hang => "#!/bin/sh\nsleep 30\n",
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

pub(super) const PANE_JSON: &str = r#"{"address":"t9","labels":{"herdr.space":"spcA","herdr.tab":"tbB","where":"home / build"},"state":"live"}"#;

pub(super) async fn run_with_pane(hc: Option<&FakeHc>, id: Option<&str>) -> Run {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut env: Vec<(&str, &str)> = Vec::new();
    if let Some(hc) = hc {
        env.push(("PATH", hc.path_value.as_str()));
    }
    if let Some(id) = id {
        env.push(("HC_TERMINAL_ID", id));
    }
    run_task(&mock, &["run", "--json", "hello"], &env, None, "").await
}

pub(super) fn system_text(run: &Run) -> String {
    let started = run.one_event("session_started");
    started["system"]["text"]
        .as_str()
        .or_else(|| started["system"]["text"]["preview"].as_str())
        .unwrap_or_default()
        .to_string()
}
// ---------------------------------------------------------------- the hint

pub(super) fn scorer_reply(names: &[&str]) -> Reply {
    let content = serde_json::to_string(names).unwrap();
    Reply::Sse(
        json!({"choices": [{"message": {"role": "assistant", "content": content}}]}).to_string(),
    )
}

const HINT: &str = "[gates.hint]\nbuiltin = 'hint'\nmodel = 'scorer/model'\n";
const HINT_TIMEOUT: &str =
    "[gates.hint]\nbuiltin = 'hint'\nmodel = 'scorer/model'\ntimeout_ms = 400\n";

#[tokio::test]
async fn the_hint_is_off_unless_configured() {
    let project = Project::new();
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = run_task(
        &mock,
        &[
            "run",
            "--json",
            "--cwd",
            &project.path(),
            "this log is too long",
        ],
        &[],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        mock.request_count(),
        1,
        "no scoring call without a hint gate"
    );
    assert!(run.events_of("source").is_empty());
}

#[tokio::test]
async fn a_hint_is_one_scoring_call_and_a_name_with_a_description_never_a_body() {
    let project = Project::new();
    let mock = Mock::start(vec![
        scorer_reply(&["ghost", "sift"]),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    let run = run_task(
        &mock,
        &[
            "run",
            "--json",
            "--cwd",
            &project.path(),
            "this log is too long",
        ],
        &[],
        None,
        HINT,
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "one scoring call, one step");
    // The scoring call: not streamed, shows the prompt and `name: description`,
    // never a skill body, and carries the key as a bearer token.
    assert_eq!(requests[0].body["stream"], false);
    let scorer_body = requests[0].body.to_string();
    assert!(scorer_body.contains("this log is too long"));
    assert!(scorer_body.contains("use when a log is too long to read"));
    assert!(!scorer_body.contains("BODY-SECRET"));
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer test-openrouter-key")
    );
    // The step: the hint is the last message, one line, no body, and the
    // invented name is gone.
    let last = requests[1].messages().last().unwrap();
    assert_eq!(last["role"], "user");
    let text = last["content"].as_str().unwrap();
    assert!(
        text.contains("might be relevant: sift - use when a log is too long to read"),
        "{text}"
    );
    assert!(
        !text.contains("ghost") && !text.contains("BODY-SECRET"),
        "{text}"
    );
    let hints: Vec<&Value> = run
        .events_of("source")
        .into_iter()
        .filter(|event| event["kind"] == "hint")
        .collect();
    assert_eq!(hints.len(), 1);
}

/// The built-in hint must see machine-level skills, i.e. `<config>/skills`,
/// just like the old core hint did (issue #13 review). The sandbox's own temp
/// dir holds the fake config, so the test never touches a real config dir.
#[tokio::test]
async fn the_hint_scores_machine_level_skills_from_the_config_dir() {
    let sandbox = Sandbox::new(HINT);
    let config_dir = sandbox.state.join("machine-config");
    let skill = config_dir.join("skills").join("machine-only");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: machine-only\ndescription: use when a machine config skill matters\n---\n\nBODY-SECRET\n",
    )
    .unwrap();
    let config_dir = toml_path(&config_dir);
    let mock = Mock::start(vec![
        scorer_reply(&["machine-only"]),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    let run = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "this log is too long"],
            &[("KELIR_CONFIG_DIR", &config_dir)],
        )
        .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "one scoring call, one step");
    let scorer_body = requests[0].body.to_string();
    assert!(
        scorer_body.contains("use when a machine config skill matters"),
        "the machine-level skill must be in the scorer catalogue: {scorer_body}"
    );
    let last = requests[1].messages().last().unwrap();
    assert!(
        last["content"]
            .as_str()
            .unwrap_or_default()
            .contains("might be relevant: machine-only")
    );
}

#[tokio::test]
async fn the_request_prefix_stays_byte_stable_across_a_hint() {
    let project = Project::new();
    let mock = Mock::start(vec![
        scorer_reply(&["sift"]),
        Reply::Sse(sse(&text_turn("first", 0))),
        scorer_reply(&["sift"]),
        Reply::Sse(sse(&text_turn("second", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &[
            "run",
            "--keep",
            "--json",
            "--cwd",
            &project.path(),
            "this log is too long",
        ],
        HINT,
    )
    .await;
    live.wait_for("turn_ended").await;
    let answer = live.ctl(&["say", "and this other long log?"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    live.wait_for("turn_ended").await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 4, "{}", live.stderr());
    assert_requests_snapshot("hint", &requests);
    let (step1, step2) = (&requests[1], &requests[3]);
    assert_eq!(system_of(step1), system_of(step2));
    let first = step1.messages().to_vec();
    let second = step2.messages().to_vec();
    assert_eq!(&second[..first.len()], &first[..], "the prefix changed");
    // ...and the tools list (part of the cached prefix) is identical.
    assert_eq!(step1.body["tools"], step2.body["tools"]);
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn a_scorer_that_fails_leaves_no_hint_and_does_not_stop_the_turn() {
    let project = Project::new();
    for reply in [
        Reply::Http(500, "{}".into()),
        Reply::Sse("garbage, not json".into()),
        scorer_reply(&[]),
        Reply::SseAfter(
            Duration::from_secs(4),
            json!({"choices":[{"message":{"content":"[\"sift\"]"}}]}).to_string(),
        ),
    ] {
        let mock = Mock::start(vec![reply, Reply::Sse(sse(&text_turn("ok", 0)))]).await;
        let run = run_task(
            &mock,
            &[
                "run",
                "--json",
                "--cwd",
                &project.path(),
                "this log is too long",
            ],
            &[],
            None,
            HINT_TIMEOUT,
        )
        .await;
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(
            run.events_of("source").is_empty(),
            "no hint on a failed scorer: {:?}",
            run.events_of("source")
        );
        let requests = mock.requests();
        assert!(requests.len() >= 2);
        let step = requests.iter().find(|r| r.body["stream"] == true).unwrap();
        assert!(!step.body.to_string().contains("might be relevant"));
    }
}

#[tokio::test]
async fn an_unreachable_scorer_endpoint_fails_open() {
    let project = Project::new();
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = run_task(
        &mock,
        &[
            "run",
            "--json",
            "--cwd",
            &project.path(),
            "this log is too long",
        ],
        &[],
        None,
        "[gates.hint]\nbuiltin = 'hint'\nmodel = 'scorer/model'\nendpoint = 'http://127.0.0.1:1/v1'\n",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(mock.request_count(), 1, "only the step reached the mock");
    assert!(run.events_of("source").is_empty());
    assert!(
        run.events_of("error")
            .iter()
            .any(|event| event["message"].as_str().unwrap_or("").contains("hint")),
        "a note says the hint was skipped"
    );
}

// ------------------------------------------------------------------ memory

#[tokio::test]
async fn memory_is_frozen_into_the_system_and_changes_arrive_at_the_tail() {
    let project = Project::new();
    let notes = project.memory();
    std::fs::write(notes.join("a.md"), "NOTE-A-ORIGINAL").unwrap();
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_wait",
            "bash",
            &json!({"command": "sleep 1"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let config = format!("memory_dir = \"{}\"\n", toml_path(&notes));
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "--cwd", &project.path(), "go"],
        &config,
    )
    .await;
    live.wait_for("call_started").await;
    std::fs::write(notes.join("a.md"), "NOTE-A-EDITED").unwrap();
    std::fs::write(notes.join("b.md"), "NOTE-B-NEW").unwrap();
    live.wait_for("turn_ended").await;
    let log = live.log();
    let started = of_type(&log, "session_started")[0];
    let system = started["system"]["text"]
        .as_str()
        .or_else(|| started["system"]["text"]["preview"].as_str())
        .unwrap()
        .to_string();
    assert!(
        system.contains("# Memory") && system.contains("NOTE-A-ORIGINAL"),
        "{system}"
    );
    assert!(
        started["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source["name"] == "a.md" && source["kind"] == "memory"),
        "{started}"
    );
    let sources: Vec<&Value> = of_type(&log, "source");
    let texts: Vec<String> = sources
        .iter()
        .filter(|event| event["kind"] == "memory")
        .map(|event| event["text"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(texts.contains(&"NOTE-A-EDITED".to_string()), "{texts:?}");
    assert!(texts.contains(&"NOTE-B-NEW".to_string()), "{texts:?}");
    let requests = mock.requests();
    assert_eq!(system_of(&requests[1]), system_of(&requests[0]));
    let first = requests[0].messages().to_vec();
    let second = requests[1].messages().to_vec();
    assert_eq!(&second[..first.len()], &first[..], "the prefix changed");
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn the_scoring_calls_usage_is_recorded_as_kind_hint() {
    let project = Project::new();
    let scorer = Reply::Sse(
        json!({
            "choices": [{"message": {"role": "assistant", "content": "[\"sift\"]"}}],
            "usage": {"prompt_tokens": 50, "completion_tokens": 70}
        })
        .to_string(),
    );
    let mock = Mock::start(vec![scorer, Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = run_task(
        &mock,
        &[
            "run",
            "--json",
            "--cwd",
            &project.path(),
            "this log is too long",
        ],
        &[],
        None,
        HINT,
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let usage = run.events_of("step_usage");
    let hints: Vec<&Value> = usage
        .iter()
        .copied()
        .filter(|e| e["kind"] == "hint")
        .collect();
    assert_eq!(hints.len(), 1, "{usage:?}");
    assert_eq!(hints[0]["prompt_tokens"], 50);
    assert_eq!(hints[0]["completion_tokens"], 70);
    assert!(hints[0].get("cost_source").is_none(), "removed event field");
    let main = run.one_event("assistant");
    assert!(
        main["usage"].is_object(),
        "the main step still has its own usage: {main:?}"
    );
}

/// m5bfix N2: a deleted memory note is one tail message, said once.
#[tokio::test]
async fn a_deleted_memory_note_is_told_to_the_model_once() {
    let project = Project::new();
    let notes = project.memory();
    std::fs::write(notes.join("a.md"), "NOTE-A-ORIGINAL").unwrap();
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_wait",
            "bash",
            &json!({"command": "sleep 1"}),
        ))),
        Reply::Sse(sse(&tool_turn(
            "call_wait2",
            "bash",
            &json!({"command": "sleep 1"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let config = format!(
        "memory_dir = \"{}\"
",
        toml_path(&notes)
    );
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "--cwd", &project.path(), "go"],
        &config,
    )
    .await;
    live.wait_for("call_started").await;
    std::fs::remove_file(notes.join("a.md")).unwrap();
    live.wait_for("turn_ended").await;
    let log = live.log();
    let deleted: Vec<&Value> = of_type(&log, "source")
        .into_iter()
        .filter(|e| {
            e["kind"] == "memory" && e["text"].as_str().unwrap_or("").contains("was deleted")
        })
        .collect();
    assert_eq!(deleted.len(), 1, "{log:?}");
    let requests = mock.requests();
    let last = requests.last().unwrap().body.to_string();
    assert!(last.contains("was deleted"), "{last}");
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

// ------------------------------------------------------------------- media

#[tokio::test]
async fn audio_and_video_are_stored_with_a_note_and_not_sent() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first", 0))),
        Reply::Sse(sse(&text_turn("second", 0))),
    ])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "hello"], "").await;
    live.wait_for("turn_ended").await;
    let media = live.cwd.join("memo.mp3");
    std::fs::write(&media, b"ID3-not-really-audio").unwrap();
    let answer = live
        .ctl(&["view", "--path", &media.to_string_lossy()])
        .await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let answer = live.ctl(&["say", "what is in the memo?"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    live.wait_for("turn_ended").await;
    let log = live.log();
    let notes: Vec<&Value> = of_type(&log, "source")
        .into_iter()
        .filter(|event| event["kind"] == "attachment-note")
        .collect();
    assert_eq!(notes.len(), 1, "{log:?}");
    let rel = notes[0]["path"].as_str().unwrap().to_string();
    assert!(
        rel.starts_with("attachments/") && rel.ends_with(".mp3"),
        "{rel}"
    );
    assert_eq!(
        std::fs::read(live.session_dir.join(&rel)).unwrap(),
        b"ID3-not-really-audio"
    );
    let requests = mock.requests();
    let body = requests[1].body.to_string();
    assert!(body.contains("audio file stored at"), "{body}");
    assert!(!body.contains("ID3-not-really-audio"));
    assert!(!body.contains("image_url"));
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

// ----------------------------------------------------------------- observe
