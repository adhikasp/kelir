//! M5-A review (reviewer's adversarial tests): sources and skills.
//!
//! Tests that pass pin behaviour the milestone promises. The ones that once
//! carried `#[ignore = "FINDING Fn: ..."]` each proved one defect in
//! docs/reviews/m5a-review.md; the M5-A fixes job un-ignored them and they now
//! pass. Where a fix made the old assertion wrong rather than the code, the
//! assertion was corrected and says so in place (the 3 MB file is now capped,
//! F7; an edit back is now announced, F1); nothing was loosened. Two tests also
//! the boundary after the call and writing the file afterwards raced it.
//! Every directory is a tempdir: nothing here reads or writes a real home.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::{Path, PathBuf};

use crate::support::*;
use serde_json::{Value, json};

pub(super) fn system_of(request: &Recorded) -> String {
    request.messages()[0]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

pub(super) fn of_type<'a>(log: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|event| event["type"] == event_type)
        .collect()
}

pub(super) fn source_names(started: &Value) -> Vec<String> {
    started["sources"]
        .as_array()
        .expect("sources[]")
        .iter()
        .map(|source| source["name"].as_str().unwrap().to_string())
        .collect()
}

pub(super) fn write(path: &Path, text: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

pub(super) const SKILL: &str = "---\nname: sift\ndescription: use when a log is too long to read\n---\n\n\
Read the tail first.\n";

pub(super) fn skill(name: &str) -> String {
    format!("---\nname: {name}\ndescription: a skill called {name}\n---\n\nbody of {name}\n")
}

/// A scratch tree: `machine/{config,home}` and `repo/...`.
pub(super) struct Tree {
    pub(super) _temp: tempfile::TempDir,
    pub(super) root: PathBuf,
}

impl Tree {
    pub(super) fn new() -> Tree {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        for dir in ["machine/config", "machine/home", "repo"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        Tree { _temp: temp, root }
    }
    pub(super) fn config(&self) -> PathBuf {
        self.root.join("machine").join("config")
    }
    pub(super) fn home(&self) -> PathBuf {
        self.root.join("machine").join("home")
    }
    pub(super) fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }
    pub(super) fn env(&self) -> Vec<(String, String)> {
        vec![
            (
                "KELIR_CONFIG_DIR".into(),
                self.config().to_string_lossy().into(),
            ),
            ("KELIR_HOME".into(), self.home().to_string_lossy().into()),
        ]
    }
}

pub(super) async fn run_in(tree: &Tree, cwd: &Path, extra_config: &str) -> (Run, Recorded) {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let owned = tree.env();
    let env: Vec<(&str, &str)> = owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let run = run_task(
        &mock,
        &["run", "--json", "--cwd", &cwd_arg, "do the thing"],
        &env,
        None,
        extra_config,
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let first = mock.requests().remove(0);
    (run, first)
}

// ------------------------------------------------------------ discovery

/// A directory symlink (Unix) or junction (Windows, no privilege needed).
pub(super) fn link_dir(link: &Path, target: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    {
        let native = |p: &Path| p.to_string_lossy().replace('/', "\\");
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J", &native(link), &native(target)])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }
}
/// Any `dd:dd:dd` in the text (a clock reading).
pub(super) fn has_clock(text: &str) -> bool {
    text.as_bytes().windows(8).any(|w| {
        w[2] == b':' && w[5] == b':' && [0, 1, 3, 4, 6, 7].iter().all(|i| w[*i].is_ascii_digit())
    })
}
// ------------------------------------------------- mid-session behaviour

struct Live {
    mock: Mock,
    live: LiveSession,
    repo: PathBuf,
    _tree: Tree,
}

/// A session whose first step sleeps 1 s in `bash`, so the test can edit
/// something before the next boundary; then `tail` more replies.
async fn live_with(tree: Tree, cwd: PathBuf, mut script: Vec<Reply>, extra_args: &[&str]) -> Live {
    script.insert(
        0,
        Reply::Sse(sse(&tool_turn(
            "call_wait",
            "bash",
            &json!({"command": "sleep 1"}),
        ))),
    );
    let mock = Mock::start(script).await;
    let cwd_arg = cwd.to_string_lossy().to_string();
    let mut args = vec!["run", "--keep", "--json", "--cwd", cwd_arg.as_str()];
    args.extend_from_slice(extra_args);
    args.push("go");
    let live = LiveSession::start(&mock, &args, "").await;
    Live {
        mock,
        live,
        repo: cwd,
        _tree: tree,
    }
}

fn tail_texts(request: &Recorded) -> Vec<String> {
    request
        .messages()
        .iter()
        .skip(1)
        .map(|m| m["content"].as_str().unwrap_or("").to_string())
        .collect()
}

#[tokio::test]
async fn cache_prefix_only_grows_across_two_source_changes() {
    let tree = Tree::new();
    write(&tree.repo().join("AGENTS.md"), "V1");
    write(&tree.repo().join(".claude/skills/sift/SKILL.md"), SKILL);
    let repo = tree.repo();
    let mut t = live_with(
        tree,
        repo,
        vec![
            Reply::Sse(sse(&text_turn("a", 0))),
            Reply::Sse(sse(&text_turn("b", 0))),
            Reply::Sse(sse(&text_turn("c", 0))),
        ],
        &[],
    )
    .await;
    t.live.wait_for("call_started").await;
    write(&t.repo.join("AGENTS.md"), "V2");
    t.live.wait_for("turn_ended").await;
    write(
        &t.repo.join(".claude/skills/sift/SKILL.md"),
        skill("sift").replace("body", "NEWER body"),
    );
    assert_eq!(t.live.ctl(&["say", "go on"]).await.code, 0);
    t.live.wait_for("turn_ended").await;
    write(&t.repo.join("AGENTS.md"), "V3");
    assert_eq!(t.live.ctl(&["say", "go on 2"]).await.code, 0);
    t.live.wait_for("turn_ended").await;
    let requests = t.mock.requests();
    assert_eq!(requests.len(), 4);
    for pair in requests.windows(2) {
        let (a, b) = (pair[0].messages(), pair[1].messages());
        assert!(b.len() > a.len());
        assert_eq!(&b[..a.len()], a, "the prefix changed between two requests");
    }
    assert_eq!(of_type(&t.live.log(), "source").len(), 3);
    let (code, stderr) = t.live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn a_skill_added_by_path_is_announced_once() {
    let tree = Tree::new();
    let repo = tree.repo();
    let extra = repo.join("extras/newskill");
    write(&extra.join("SKILL.md"), skill("newskill"));
    let mut t = live_with(
        tree,
        repo,
        (0..4)
            .map(|i| Reply::Sse(sse(&text_turn(&format!("r{i}"), 0))))
            .collect(),
        &[],
    )
    .await;
    t.live.wait_for("turn_ended").await;
    let path = extra.to_string_lossy().to_string();
    let answer = t
        .live
        .ctl(&["skills", "--name", "newskill", "--path", &path])
        .await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    for task in ["apply", "and again"] {
        assert_eq!(t.live.ctl(&["say", task]).await.code, 0);
        t.live.wait_for("turn_ended").await;
    }
    let log = t.live.log();
    let sources = of_type(&log, "source");
    assert_eq!(
        sources.len(),
        1,
        "the file did not change but produced {} source events: {sources:?}",
        sources.len()
    );
}

#[tokio::test]
async fn a_resume_does_not_repeat_a_source_the_log_already_has() {
    let tree = Tree::new();
    write(&tree.repo().join("AGENTS.md"), "ORIGINAL");
    let repo = tree.repo();
    let mut first = live_with(
        tree,
        repo.clone(),
        vec![Reply::Sse(sse(&text_turn("done", 0)))],
        &[],
    )
    .await;
    first.live.wait_for("call_started").await;
    write(&repo.join("AGENTS.md"), "EDITED");
    first.live.wait_for("turn_ended").await;
    assert_eq!(of_type(&first.live.log(), "source").len(), 1);
    let id = first.live.id.clone();
    first.live.kill().await;

    let mock2 = Mock::start(vec![
        Reply::Sse(sse(&text_turn("r1", 0))),
        Reply::Sse(sse(&text_turn("r2", 0))),
    ])
    .await;
    let repo_arg = repo.to_string_lossy().to_string();
    let mut second = LiveSession::start_reusing(
        first.live.state.clone(),
        &mock2,
        &[
            "run", "--keep", "--json", "--cwd", &repo_arg, "--resume", &id,
        ],
        "",
    )
    .await;
    // A resume continues by itself: no `say` needed (and none, or the test races).
    second.wait_for("turn_ended").await;
    let sources = of_type(&second.log(), "source").len();
    assert_eq!(sources, 1, "the resume appended a duplicate of EDITED");
    let _ = second.end().await;
}

#[tokio::test]
async fn a_file_named_embedded_is_not_mistaken_for_the_base_prompt() {
    let tree = Tree::new();
    write(&tree.repo().join("AGENTS.md"), "RULES");
    let repo = tree.repo();
    let mut t = live_with(
        tree,
        repo.clone(),
        vec![Reply::Sse(sse(&text_turn("ok", 0)))],
        &[],
    )
    .await;
    // The session's cwd and the process's cwd both get an innocent file.
    write(&repo.join("embedded"), "notes about the embedded firmware");
    write(
        &t.live.cwd.join("embedded"),
        "notes about the embedded firmware",
    );
    t.live.wait_for("turn_ended").await;
    let log = t.live.log();
    let sources = of_type(&log, "source");
    assert!(
        sources.is_empty(),
        "an unrelated file called `embedded` became a source message: {sources:?}"
    );
    let _ = t.live.end().await;
}

#[tokio::test]
async fn a_skill_named_like_a_markdown_file_stays_a_skill() {
    let tree = Tree::new();
    let repo = tree.repo();
    write(
        &repo.join(".claude/skills/notes.md/SKILL.md"),
        skill("notes.md"),
    );
    let mut t = live_with(
        tree,
        repo.clone(),
        vec![Reply::Sse(sse(&text_turn("ok", 0)))],
        &[],
    )
    .await;
    t.live.wait_for("call_started").await;
    write(
        &repo.join(".claude/skills/notes.md/SKILL.md"),
        skill("notes.md").replace("body of", "EDITED body of"),
    );
    let source = t.live.wait_for("source").await;
    assert_eq!(source["kind"], "skill", "{source}");
    let _ = t.live.end().await;
}

#[tokio::test]
async fn a_resume_keeps_a_system_prompt_that_was_spilled_to_a_blob() {
    // Instruction files make the system prompt big enough to be stored as a
    // blob (`session_started.system.text` = {spilled, path}); a resume must read
    // it back whole, not start with an empty system.
    let tree = Tree::new();
    let big = "rule: keep steps small and verified\n".repeat(600);
    write(&tree.repo().join("AGENTS.md"), &big);
    let repo = tree.repo();
    let mut first = live_with(
        tree,
        repo.clone(),
        vec![Reply::Sse(sse(&text_turn("done", 0)))],
        &[],
    )
    .await;
    first.live.wait_for("turn_ended").await;
    let started = first
        .live
        .log()
        .into_iter()
        .find(|e| e["type"] == "session_started")
        .unwrap();
    assert!(
        started["system"]["text"].is_object(),
        "not spilled: raise the file size"
    );
    let original = system_of(&first.mock.requests()[0]);
    assert!(original.contains("keep steps small"));
    let id = first.live.id.clone();
    first.live.kill().await;

    let mock2 = Mock::start(vec![Reply::Sse(sse(&text_turn("r1", 0)))]).await;
    let repo_arg = repo.to_string_lossy().to_string();
    let mut second = LiveSession::start_reusing(
        first.live.state.clone(),
        &mock2,
        &[
            "run", "--keep", "--json", "--cwd", &repo_arg, "--resume", &id,
        ],
        "",
    )
    .await;
    second.wait_for("turn_ended").await;
    let requests = mock2.requests();
    assert!(!requests.is_empty(), "{}", second.stderr());
    assert_eq!(
        system_of(&requests[0]).len(),
        original.len(),
        "the resumed request's system prompt is not the frozen one"
    );
    let _ = second.end().await;
}
