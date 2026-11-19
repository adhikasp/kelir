//! M5-A end-to-end: sources and skills against the mock provider.
//!
//! The real binary runs with a prepared cwd that holds an `AGENTS.md` and a
//! skill, and the assertions are on the event log (`session_started.sources[]`,
//! `source` events) and on the request bodies the mock received. The headline
//! check is the cache prefix: whatever arrives at the tail, the bytes the
//! provider already cached are unchanged.

mod support;

use std::path::Path;

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

/// A prepared project: `AGENTS.md` and one skill under `.claude/skills`.
struct Project {
    _temp: tempfile::TempDir,
    cwd: std::path::PathBuf,
}

impl Project {
    fn new(agents: &str, skill: &str) -> Project {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("AGENTS.md"), agents).unwrap();
        let skill_dir = cwd.join(".claude").join("skills").join("sift");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), skill).unwrap();
        Project { _temp: temp, cwd }
    }

    fn path(&self) -> String {
        self.cwd.to_string_lossy().to_string()
    }

    fn agents(&self) -> std::path::PathBuf {
        self.cwd.join("AGENTS.md")
    }
}

const SKILL: &str = "---\nname: sift\ndescription: use when a log is too long to read\n---\n\n\
Read the tail first.\n";

#[tokio::test]
async fn session_started_carries_the_real_sources_and_the_system_has_them() {
    let project = Project::new("ALWAYS RUN THE GATES BEFORE REPORTING", SKILL);
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "--cwd", &project.path(), "do the thing"],
        &[],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let started = run.one_event("session_started");
    let sources = started["sources"].as_array().expect("sources[]");
    // The base prompt, the instruction file and the skill: all three are
    // recorded, each with a real path and a hash.
    let names: Vec<&str> = sources
        .iter()
        .map(|source| source["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["prompt.md", "AGENTS.md", "sift"], "{sources:?}");
    assert_eq!(sources[0]["path"], "embedded");
    for source in &sources[1..] {
        let path = Path::new(source["path"].as_str().unwrap());
        assert!(path.is_file(), "{} is not a file", path.display());
        assert_eq!(source["hash"].as_str().unwrap().len(), 16);
    }
    assert!(sources[1]["path"].as_str().unwrap().ends_with("AGENTS.md"));

    // The system prompt carries the instruction text and the skills directory,
    // and no skill body: skills are pull.
    let system = started["system"]["text"].as_str().unwrap();
    assert!(
        system.contains("ALWAYS RUN THE GATES BEFORE REPORTING"),
        "{system}"
    );
    assert!(system.contains(".claude/skills"), "{system}");
    assert!(
        !system.contains("Read the tail first"),
        "a skill body is pushed:\n{system}"
    );
    assert_eq!(
        started["system"]["hash"].as_str().unwrap(),
        kelir::util::fnv1a64_hex(system.as_bytes())
    );

    // The first request's system message is exactly the recorded text.
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(system_of(&requests[0]), system);
    assert_eq!(requests[0].roles(), vec!["system", "user"]);
}

#[tokio::test]
async fn an_edited_instruction_file_is_a_tail_message_and_the_prefix_is_unchanged() {
    let project = Project::new("ORIGINAL INSTRUCTIONS", SKILL);
    let mock = Mock::start(vec![
        // The first step runs a tool, which gives the test time to edit the
        // file before the next boundary.
        Reply::Sse(sse(&tool_turn(
            "call_wait",
            "bash",
            &json!({"command": "sleep 1"}),
        ))),
        Reply::Sse(sse(&text_turn("carrying on", 0))),
        Reply::Sse(sse(&text_turn("still here", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "--cwd", &project.path(), "go"],
        "",
    )
    .await;

    // While the call runs, the instruction file changes on disk.
    let settled = live.wait_for("tool_settled").await;
    assert_eq!(settled["call_id"], "call_wait");
    std::fs::write(project.agents(), "EDITED INSTRUCTIONS").unwrap();

    // The edit arrives as a `source` event at the tail, at the boundary
    // before the next request (not after the turn).
    let source = live.wait_for("source").await;
    assert_eq!(source["kind"], "instruction", "{source}");
    assert_eq!(source["name"], "AGENTS.md", "{source}");
    let source_text = source["text"]
        .as_str()
        .or_else(|| source["text"]["preview"].as_str())
        .expect("the source text");
    assert_eq!(source_text, "EDITED INSTRUCTIONS");
    let ended = live.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done");

    // ...and the system prefix of the next request is byte-identical.
    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "{}", live.stderr());
    assert_eq!(
        system_of(&requests[1]),
        system_of(&requests[0]),
        "an edit rewrote the system prompt"
    );
    // The second request is the first one plus the call's result and the
    // source message: a prefix that only grows.
    let first = requests[0].messages().to_vec();
    let second = requests[1].messages().to_vec();
    assert_eq!(&second[..first.len()], &first[..], "the prefix changed");
    let last = second.last().unwrap();
    assert_eq!(last["role"], "user");
    assert!(
        last["content"]
            .as_str()
            .unwrap()
            .contains("EDITED INSTRUCTIONS"),
        "{last}"
    );

    // Editing it back to text the context already carries is not a duplicate.
    std::fs::write(project.agents(), "ORIGINAL INSTRUCTIONS").unwrap();
    let answer = live.ctl(&["say", "and now?"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    live.wait_for("turn_ended").await;
    let source_events = of_type(&live.log(), "source").len();
    assert_eq!(source_events, 1, "an edit back duplicated the source");
    assert_eq!(mock.requests().len(), 3);

    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn a_skill_edited_mid_session_is_a_source_too() {
    let project = Project::new("INSTRUCTIONS", SKILL);
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_wait",
            "bash",
            &json!({"command": "sleep 1"}),
        ))),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "--cwd", &project.path(), "go"],
        "",
    )
    .await;
    live.wait_for("tool_settled").await;
    let skill = project
        .cwd
        .join(".claude")
        .join("skills")
        .join("sift")
        .join("SKILL.md");
    std::fs::write(&skill, "---\nname: sift\ndescription: d\n---\n\nNEW BODY\n").unwrap();
    let source = live.wait_for("source").await;
    assert_eq!(source["kind"], "skill", "{source}");
    assert_eq!(source["name"], "sift", "{source}");
    // The skills directory's name is the `sources[]` name; the message carries
    // the file's whole text, front matter included.
    assert!(
        source["text"].as_str().unwrap().contains("NEW BODY"),
        "{source}"
    );
    live.wait_for("turn_ended").await;
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn the_skills_verb_adds_and_replaces_a_source() {
    let project = Project::new("INSTRUCTIONS", SKILL);
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("noted", 0))),
        Reply::Sse(sse(&text_turn("replaced", 0))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "--cwd", &project.path(), "go"],
        "",
    )
    .await;
    live.wait_for("turn_ended").await;

    // Add one source.
    let answer = live
        .ctl(&[
            "skills",
            "--name",
            "house",
            "--text",
            "always run the gates",
        ])
        .await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    assert_eq!(answer.response()["ok"], true);
    let answer = live.ctl(&["say", "apply it"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    live.wait_for("turn_ended").await;

    // Replace the same name with another body: a second `source`, same name.
    let answer = live
        .ctl(&[
            "skills",
            "--name",
            "house",
            "--text",
            "always write the test first",
        ])
        .await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let answer = live.ctl(&["say", "apply the new one"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    live.wait_for("turn_ended").await;

    let log = live.log();
    let sources = of_type(&log, "source");
    assert_eq!(sources.len(), 2, "{sources:?}");
    assert_eq!(sources[0]["kind"], "skill");
    assert_eq!(sources[0]["name"], "house");
    assert_eq!(sources[0]["text"], "always run the gates");
    assert_eq!(sources[1]["name"], "house");
    assert_eq!(sources[1]["text"], "always write the test first");

    // The verb is in the log like any other, and applies at the next step.
    let verbs = of_type(&log, "verb");
    assert!(
        verbs
            .iter()
            .any(|event| event["name"] == "skills" && event["boundary"] == "next_step"),
        "{verbs:?}"
    );

    // The model saw the new body at the tail of a growing prefix.
    let requests = mock.requests();
    let last = requests.last().unwrap();
    let user_texts: Vec<String> = last
        .messages()
        .iter()
        .filter(|message| message["role"] == "user")
        .map(|message| message["content"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        user_texts
            .iter()
            .any(|text| text.contains("always write the test first")),
        "{user_texts:?}"
    );
    assert_eq!(
        system_of(&requests[2]),
        system_of(&requests[0]),
        "a skills verb rewrote the system prompt"
    );

    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn the_skills_verb_refuses_a_source_with_nothing_to_add() {
    let project = Project::new("INSTRUCTIONS", SKILL);
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("ok", 0))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "--cwd", &project.path(), "go"],
        "",
    )
    .await;
    live.wait_for("turn_ended").await;
    // `ctl` validates locally: nothing is logged, the exit code says why.
    let answer = live.ctl(&["skills", "--name", "house"]).await;
    assert_eq!(answer.code, 2, "{}{}", answer.stdout, answer.stderr);
    // A directory with no SKILL.md is refused by the session, recorded as an
    // error rather than a silent success.
    let empty = project.cwd.join("empty-skill");
    std::fs::create_dir_all(&empty).unwrap();
    let answer = live
        .ctl(&[
            "skills",
            "--name",
            "empty",
            "--path",
            &empty.to_string_lossy(),
        ])
        .await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let answer = live.ctl(&["say", "go on"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    live.wait_for("turn_ended").await;
    let log = live.log();
    assert!(
        of_type(&log, "error")
            .iter()
            .any(|event| event["where"] == "verb"),
        "{log:?}"
    );
    assert!(of_type(&log, "source").is_empty());
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn a_resumed_session_keeps_watching_the_sources_it_recorded() {
    let project = Project::new("ORIGINAL", SKILL);
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("done", 0)))]).await;
    let mut first = LiveSession::start(
        &mock,
        &[
            "run",
            "--keep",
            "--json",
            "--cwd",
            &project.path(),
            "first task",
        ],
        "",
    )
    .await;
    first.wait_for("turn_ended").await;
    let session_id = first.id.clone();
    let system = first
        .log()
        .into_iter()
        .find(|event| event["type"] == "session_started")
        .expect("session_started")["system"]["text"]
        .as_str()
        .unwrap()
        .to_string();
    // `kill` (not `end`) leaves the first session's temp dir — and so its
    // state — alive for the resume; the guard still reaps the process.
    first.kill().await;

    // The file changes while no session runs; the resume re-checks what the
    // log says the system was rendered from.
    std::fs::write(project.agents(), "CHANGED WHILE DOWN").unwrap();
    let resumed_mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("continuing", 0))),
        Reply::Sse(sse(&text_turn("still going", 0))),
    ])
    .await;
    let mut live = LiveSession::start_reusing(
        first.state.clone(),
        &resumed_mock,
        &[
            "resume",
            "--keep",
            "--json",
            "--cwd",
            &project.path(),
            &session_id,
        ],
        "",
    )
    .await;
    // A resume records `resumed`, not a new `session_started`: the frozen
    // system text is the log's, byte for byte.
    let started = live
        .log()
        .into_iter()
        .find(|event| event["type"] == "session_started")
        .expect("the log's session_started");
    assert_eq!(started["system"]["text"], system);
    // And the edit is a `source` message at the tail.
    let source = live.wait_for("source").await;
    assert_eq!(source["name"], "AGENTS.md");
    assert_eq!(
        source["text"].as_str().unwrap_or_default(),
        "CHANGED WHILE DOWN"
    );
    live.wait_for("turn_ended").await;
    let requests = resumed_mock.requests();
    assert!(!requests.is_empty(), "{}", live.stderr());
    assert_eq!(
        system_of(&requests[0]),
        system,
        "the resume re-rendered the system prompt"
    );
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}
