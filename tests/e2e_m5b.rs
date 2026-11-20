//! M5-B end-to-end: the Herdr line, the pushed hint, memory, media notes and
//! `observe`, against the mock provider (and a fake `hc` on PATH).
//!
//! The headline: `kelir observe <s> --raw` equals, byte for byte, the body the
//! mock received for the request it describes.

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use support::*;

fn system_of(request: &Recorded) -> String {
    request.messages()[0]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn of_type<'a>(log: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|event| event["type"] == event_type)
        .collect()
}

/// A project with one skill `sift` (and an optional memory directory).
struct Project {
    _temp: tempfile::TempDir,
    cwd: PathBuf,
}

const SKILL: &str = "---\nname: sift\ndescription: use when a log is too long to read\n---\n\n\
Read the tail first. BODY-SECRET\n";

impl Project {
    fn new() -> Project {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        let skill = cwd.join(".claude").join("skills").join("sift");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), SKILL).unwrap();
        Project { _temp: temp, cwd }
    }

    fn path(&self) -> String {
        self.cwd.to_string_lossy().to_string()
    }

    fn memory(&self) -> PathBuf {
        let dir = self.cwd.join("notes");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

fn toml_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

// ------------------------------------------------------------------ fake hc

/// A directory holding a fake `hc` that prints `json` (or fails, or hangs),
/// and the PATH that finds it first.
struct FakeHc {
    _dir: tempfile::TempDir,
    path_value: String,
}

#[derive(Clone, Copy)]
enum HcMode {
    Print,
    Fail,
    Hang,
}

fn fake_hc(json: &str, mode: HcMode) -> FakeHc {
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

const PANE_JSON: &str = r#"{"address":"t9","labels":{"herdr.space":"spcA","herdr.tab":"tbB","where":"home / build"},"state":"live"}"#;

async fn run_with_pane(hc: Option<&FakeHc>, id: Option<&str>) -> Run {
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

fn system_text(run: &Run) -> String {
    let started = run.one_event("session_started");
    started["system"]["text"]
        .as_str()
        .or_else(|| started["system"]["text"]["preview"].as_str())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn in_a_pane_the_frozen_system_carries_one_herdr_line() {
    let hc = fake_hc(PANE_JSON, HcMode::Print);
    let run = run_with_pane(Some(&hc), Some("t9")).await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let system = system_text(&run);
    assert!(
        system.contains("herdr: pane t9, space spcA, tab tbB, where \"home / build\"\n"),
        "{system}"
    );
    let started = run.one_event("session_started");
    let herdr: Vec<&Value> = started["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|source| source["name"] == "herdr")
        .collect();
    assert_eq!(herdr.len(), 1, "{started}");
    assert_eq!(herdr[0]["kind"], "herdr");
    assert_eq!(herdr[0]["path"], "");
    // No live count is ever frozen in.
    assert!(!system.contains("agents"), "{system}");
}

#[tokio::test]
async fn the_same_pane_renders_the_same_bytes() {
    let hc = fake_hc(PANE_JSON, HcMode::Print);
    let a = run_with_pane(Some(&hc), Some("t9")).await;
    let b = run_with_pane(Some(&hc), Some("t9")).await;
    // Everything else in the system is the same too, except the cwd each
    // sandbox has; the line itself is byte-identical.
    let line = |run: &Run| {
        system_text(run)
            .lines()
            .find(|line| line.starts_with("herdr:"))
            .map(str::to_string)
    };
    assert!(line(&a).is_some());
    assert_eq!(line(&a), line(&b));
}

#[tokio::test]
async fn outside_herdr_there_is_no_line_and_no_note() {
    // `hc` is on PATH, but there is no pane: it must not even be asked.
    let hc = fake_hc(PANE_JSON, HcMode::Print);
    let run = run_with_pane(Some(&hc), None).await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(!system_text(&run).contains("herdr:"));
    assert!(
        run.events_of("error")
            .iter()
            .all(|event| !event["message"].as_str().unwrap_or("").contains("herdr")),
        "{:?}",
        run.events_of("error")
    );
}

#[tokio::test]
async fn a_failing_or_malformed_hc_omits_the_line_with_a_note() {
    for (mode, json) in [
        (HcMode::Fail, ""),
        (HcMode::Print, "this is not json"),
        (HcMode::Print, "[1,2,3]"),
    ] {
        let hc = fake_hc(json, mode);
        let run = run_with_pane(Some(&hc), Some("t9")).await;
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(!system_text(&run).contains("herdr:"));
        let notes: Vec<String> = run
            .events_of("error")
            .iter()
            .filter(|event| event["where"] == "source")
            .filter_map(|event| event["message"].as_str().map(str::to_string))
            .collect();
        assert!(notes.iter().any(|note| note.contains("herdr")), "{notes:?}");
    }
}

#[tokio::test]
async fn a_hanging_hc_costs_about_a_second_and_never_fails_the_session() {
    let hc = fake_hc(PANE_JSON, HcMode::Hang);
    let started = std::time::Instant::now();
    let run = run_with_pane(Some(&hc), Some("t9")).await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "took {:?}",
        started.elapsed()
    );
    assert!(!system_text(&run).contains("herdr:"));
}

#[tokio::test]
async fn a_missing_hc_omits_the_line() {
    // PATH with nothing that is `hc` in it (the empty directory).
    let dir = tempfile::tempdir().unwrap();
    let only = dir.path().to_string_lossy().into_owned();
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let system_root = std::env::var("SystemRoot").unwrap_or_default();
    let path = if cfg!(windows) {
        format!("{only};{system_root}\\System32")
    } else {
        format!("{only}:/usr/bin:/bin")
    };
    let run = run_task(
        &mock,
        &["run", "--json", "hello"],
        &[("PATH", path.as_str()), ("HC_TERMINAL_ID", "t9")],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(!system_text(&run).contains("herdr:"));
}

// ---------------------------------------------------------------- the hint

fn scorer_reply(names: &[&str]) -> Reply {
    let content = serde_json::to_string(names).unwrap();
    Reply::Sse(
        json!({"choices": [{"message": {"role": "assistant", "content": content}}]}).to_string(),
    )
}

const HINT: &str = "[hint]\nmodel = \"scorer/model\"\n";

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
    assert_eq!(mock.request_count(), 1, "no scoring call without [hint]");
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
            "[hint]\nmodel = \"scorer/model\"\ntimeout_ms = 400\n",
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
        "[hint]
model = \"scorer/model\"
endpoint = \"http://127.0.0.1:1/v1\"
",
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
    live.wait_for("tool_started").await;
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

#[tokio::test]
async fn observe_raw_is_the_body_the_mock_received_byte_for_byte() {
    let project = Project::new();
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Gated(gate.clone(), sse(&text_turn("first", 0))),
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
            "look at this",
        ],
        "",
    )
    .await;
    // The first request is in flight (held): nothing has changed since it was
    // built, so what observe says would be sent next is exactly that request.
    live.wait_for("step_started").await;
    for _ in 0..100 {
        if mock.request_count() >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let sandbox = Sandbox::from_live(&live);
    let raw = sandbox
        .kelir(&mock.base_url(), &["observe", &live.id, "--raw"], &[])
        .await;
    assert_eq!(raw.code, 0, "{}", raw.stderr);
    let sent = mock.requests()[0].raw_body.clone();
    assert_eq!(raw.stdout, sent, "observe --raw differs from the wire body");
    gate.notify_one();
    live.wait_for("turn_ended").await;

    // At rest the answer is the next request minus the message that has not
    // been typed yet: a strict prefix of what the mock gets after a `say`.
    let at_rest = sandbox
        .kelir(&mock.base_url(), &["observe", &live.id, "--raw"], &[])
        .await;
    assert_eq!(at_rest.code, 0, "{}", at_rest.stderr);
    let observed: Value = serde_json::from_str(&at_rest.stdout).unwrap();
    let answer = live.ctl(&["say", "and now?"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    live.wait_for("turn_ended").await;
    let next = mock.requests()[1].body.clone();
    let seen = observed["messages"].as_array().unwrap();
    assert_eq!(
        &next["messages"].as_array().unwrap()[..seen.len()],
        &seen[..]
    );
    assert_eq!(observed["tools"], next["tools"]);
    assert_eq!(observed["model"], next["model"]);

    // The human view carries every part, and `ctl observe` is the same read.
    let human = sandbox
        .kelir(&mock.base_url(), &["observe", &live.id], &[])
        .await;
    assert_eq!(human.code, 0, "{}", human.stderr);
    for part in [
        "== sources",
        "== hint ==",
        "== tools",
        "== system",
        "== messages",
        "cache:",
        "estimate:",
    ] {
        assert!(
            human.stdout.contains(part),
            "{part} missing: {}",
            human.stdout
        );
    }
    assert!(human.stdout.contains("look at this"));
    let via_ctl = sandbox
        .kelir(&mock.base_url(), &["ctl", &live.id, "observe", "raw"], &[])
        .await;
    assert_eq!(via_ctl.code, 0, "{}", via_ctl.stderr);
    let after = sandbox
        .kelir(&mock.base_url(), &["observe", &live.id, "--raw"], &[])
        .await;
    assert_eq!(via_ctl.stdout, after.stdout);
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn observe_names_an_unknown_session() {
    let sandbox = Sandbox::new("");
    let out = sandbox
        .kelir(
            "http://127.0.0.1:1/v1",
            &["observe", "ses_nope", "--raw"],
            &[],
        )
        .await;
    assert_eq!(out.code, 1, "{}", out.stderr);
    assert!(out.stdout.is_empty());
}
