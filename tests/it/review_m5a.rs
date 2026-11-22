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

fn source_names(started: &Value) -> Vec<String> {
    started["sources"]
        .as_array()
        .expect("sources[]")
        .iter()
        .map(|source| source["name"].as_str().unwrap().to_string())
        .collect()
}

fn write(path: &Path, text: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

const SKILL: &str = "---\nname: sift\ndescription: use when a log is too long to read\n---\n\n\
Read the tail first.\n";

fn skill(name: &str) -> String {
    format!("---\nname: {name}\ndescription: a skill called {name}\n---\n\nbody of {name}\n")
}

/// A scratch tree: `machine/{config,home}` and `repo/...`.
struct Tree {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Tree {
    fn new() -> Tree {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        for dir in ["machine/config", "machine/home", "repo"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        Tree { _temp: temp, root }
    }
    fn config(&self) -> PathBuf {
        self.root.join("machine").join("config")
    }
    fn home(&self) -> PathBuf {
        self.root.join("machine").join("home")
    }
    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }
    fn env(&self) -> Vec<(String, String)> {
        vec![
            (
                "KELIR_CONFIG_DIR".into(),
                self.config().to_string_lossy().into(),
            ),
            ("KELIR_HOME".into(), self.home().to_string_lossy().into()),
        ]
    }
}

async fn run_in(tree: &Tree, cwd: &Path, extra_config: &str) -> (Run, Recorded) {
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
fn link_dir(link: &Path, target: &Path) {
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

#[tokio::test]
async fn project_wins_over_machine_and_the_nearest_file_wins() {
    let tree = Tree::new();
    // Machine layer.
    write(&tree.config().join("AGENTS.md"), "MACHINE-AGENTS");
    write(&tree.config().join("CLAUDE.md"), "MACHINE-CLAUDE");
    write(
        &tree.home().join(".claude/skills/shared/SKILL.md"),
        skill("shared").replace("body of", "MACHINE body of"),
    );
    write(
        &tree.home().join(".claude/skills/only-machine/SKILL.md"),
        skill("only-machine"),
    );
    // Project layer: a root file and a nearer one.
    write(&tree.repo().join("AGENTS.md"), "ROOT-AGENTS");
    write(&tree.repo().join("sub/AGENTS.md"), "SUB-AGENTS");
    write(&tree.repo().join("sub/CLAUDE.md"), "SUB-CLAUDE");
    let cwd = tree.repo().join("sub/leaf");
    std::fs::create_dir_all(&cwd).unwrap();
    write(
        &cwd.join(".claude/skills/shared/SKILL.md"),
        skill("shared").replace("body of", "PROJECT body of"),
    );

    let (run, first) = run_in(&tree, &cwd, "").await;
    let started = run.one_event("session_started");
    let system = system_of(&first);
    // Project over machine on the same name; the nearer file over the farther.
    assert!(system.contains("SUB-AGENTS"), "{system}");
    assert!(system.contains("SUB-CLAUDE"), "{system}");
    assert!(!system.contains("MACHINE-AGENTS"), "{system}");
    assert!(!system.contains("MACHINE-CLAUDE"), "{system}");
    // The machine-only skill is still discovered; the clash resolves to the project.
    let sources = started["sources"].as_array().unwrap();
    let shared: Vec<&Value> = sources.iter().filter(|s| s["name"] == "shared").collect();
    assert_eq!(shared.len(), 1, "{sources:?}");
    assert!(
        shared[0]["path"]
            .as_str()
            .unwrap()
            .contains("leaf/.claude/skills/shared"),
        "{shared:?}"
    );
    assert!(source_names(started).contains(&"only-machine".to_string()));
    // Skills are pull: no body, no listing in the prompt.
    assert!(!system.contains("PROJECT body"), "{system}");
    assert!(!system.contains("only-machine"), "{system}");
    assert!(!system.contains("a skill called"), "{system}");
}

#[tokio::test]
async fn an_ancestors_instruction_file_is_collected_too() {
    let tree = Tree::new();
    write(&tree.repo().join("AGENTS.md"), "ROOT-AGENTS");
    write(&tree.repo().join("sub/AGENTS.md"), "SUB-AGENTS");
    let (_, first) = run_in(&tree, &tree.repo().join("sub"), "").await;
    let system = system_of(&first);
    assert!(system.contains("SUB-AGENTS"), "{system}");
    assert!(
        system.contains("ROOT-AGENTS"),
        "the brief says walk up and *collect*; the parent's file is missing:\n{system}"
    );
}

#[tokio::test]
async fn nothing_outside_the_intended_places_is_read() {
    let tree = Tree::new();
    // Decoys: a sibling, a child, an ancestor's .claude/skills (ancestors only
    // contribute instruction files), and a skills dir in the config's parent.
    write(&tree.root.join("sibling/AGENTS.md"), "SIBLING-DECOY");
    write(&tree.repo().join("work/child/AGENTS.md"), "CHILD-DECOY");
    write(
        &tree.repo().join("work/child/.claude/skills/c/SKILL.md"),
        skill("child-skill"),
    );
    write(
        &tree.repo().join(".claude/skills/anc/SKILL.md"),
        skill("ancestor-skill"),
    );
    write(
        &tree.repo().join(".kelir/prompt.md"),
        "ANCESTOR-PROMPT-DECOY",
    );
    write(
        &tree.root.join("machine/skills/m/SKILL.md"),
        skill("config-parent-skill"),
    );
    write(
        &tree.home().join("skills/h/SKILL.md"),
        skill("home-skills-decoy"),
    );
    write(&tree.home().join("CLAUDE.md"), "HOME-CLAUDE-DECOY");
    write(&tree.repo().join("work/AGENTS.md"), "REAL");
    let (run, first) = run_in(&tree, &tree.repo().join("work"), "").await;
    let system = system_of(&first);
    for decoy in [
        "SIBLING-DECOY",
        "CHILD-DECOY",
        "ANCESTOR-PROMPT-DECOY",
        "HOME-CLAUDE-DECOY",
    ] {
        assert!(!system.contains(decoy), "{decoy} leaked into:\n{system}");
    }
    let names = source_names(run.one_event("session_started"));
    for decoy in [
        "child-skill",
        "ancestor-skill",
        "config-parent-skill",
        "home-skills-decoy",
    ] {
        assert!(!names.contains(&decoy.to_string()), "{names:?}");
    }
    assert!(system.contains("REAL"));
}

#[tokio::test]
async fn the_system_render_is_byte_identical_and_has_no_time() {
    let tree = Tree::new();
    write(&tree.repo().join("AGENTS.md"), "SAME");
    write(&tree.repo().join(".claude/skills/sift/SKILL.md"), SKILL);
    // The session id is per-session, so strip the session block before the
    // byte-identical check; both prompts must otherwise be the same bytes.
    let (a, first_a) = run_in(&tree, &tree.repo(), "").await;
    let (b, first_b) = run_in(&tree, &tree.repo(), "").await;
    let strip_session = |text: &str| {
        text.split("\n# Session\n")
            .next()
            .unwrap_or(text)
            .to_string()
    };
    assert_eq!(
        strip_session(&system_of(&first_a)),
        strip_session(&system_of(&first_b))
    );
    let (sa, sb) = (
        a.one_event("session_started"),
        b.one_event("session_started"),
    );
    // Each prompt is self-aware: it carries its own id and resume command.
    let id_a = sa["id"].as_str().unwrap();
    let id_b = sb["id"].as_str().unwrap();
    assert_ne!(id_a, id_b);
    assert!(system_of(&first_a).contains(&format!("id: {id_a}")));
    assert!(system_of(&first_b).contains(&format!("resume: kelir tui --resume {id_b}")));
    assert_eq!(
        sa["system"]["hash"].as_str().is_some(),
        sb["system"]["hash"].as_str().is_some()
    );
    assert_eq!(sa["sources"], sb["sources"]);
    let system = system_of(&first_a);
    let date = system.lines().find(|l| l.starts_with("date:")).unwrap();
    assert_eq!(date.len(), "date: 2026-11-19".len(), "{date}");
    assert!(
        !has_clock(&system),
        "a time of day is in the system prompt:
{system}"
    );
}

/// Any `dd:dd:dd` in the text (a clock reading).
fn has_clock(text: &str) -> bool {
    text.as_bytes().windows(8).any(|w| {
        w[2] == b':' && w[5] == b':' && [0, 1, 3, 4, 6, 7].iter().all(|i| w[*i].is_ascii_digit())
    })
}

#[tokio::test]
async fn awkward_files_are_tolerated() {
    let tree = Tree::new();
    let repo = tree.repo();
    // CRLF instruction file with a BOM.
    write(&repo.join("AGENTS.md"), "\u{feff}line one\r\nline two\r\n");
    // Invalid UTF-8.
    write(&repo.join("CLAUDE.md"), [b'x', 0xff, 0xfe, b'y']);
    let skills = repo.join(".claude/skills");
    write(&skills.join("good/SKILL.md"), skill("good"));
    write(&skills.join(".hidden/SKILL.md"), skill("hidden"));
    write(&skills.join("stray.md"), "not a skill dir");
    std::fs::create_dir_all(skills.join("empty")).unwrap();
    let (run, first) = run_in(&tree, &repo, "").await;
    let names = source_names(run.one_event("session_started"));
    assert!(names.contains(&"good".to_string()), "{names:?}");
    assert!(!names.contains(&"hidden".to_string()), "{names:?}");
    assert!(!names.contains(&"empty".to_string()), "{names:?}");
    assert!(!names.contains(&"stray.md".to_string()), "{names:?}");
    let system = system_of(&first);
    assert!(system.contains("line one"), "{system}");
    assert!(system.contains("line two"), "{system}");
}

#[tokio::test]
async fn a_non_utf8_instruction_file_is_not_silently_dropped() {
    let tree = Tree::new();
    write(
        &tree.repo().join("AGENTS.md"),
        b"caf\xe9 rules: ALWAYS-RUN-GATES",
    );
    let (run, first) = run_in(&tree, &tree.repo(), "").await;
    let names = source_names(run.one_event("session_started"));
    let system = system_of(&first);
    assert!(
        names.contains(&"AGENTS.md".to_string()) || system.contains("ALWAYS-RUN-GATES"),
        "the file vanished without a trace: {names:?}"
    );
}

#[tokio::test]
async fn a_malformed_skill_is_not_recorded_as_a_source() {
    let tree = Tree::new();
    let skills = tree.repo().join(".claude/skills");
    write(&skills.join("good/SKILL.md"), skill("good"));
    write(
        &skills.join("nofm/SKILL.md"),
        "just prose, no front matter\n",
    );
    write(
        &skills.join("unclosed/SKILL.md"),
        "---\nname: unclosed\n\nno closing fence\n",
    );
    write(
        &skills.join("noname/SKILL.md"),
        "---\ndescription: d\n---\nbody\n",
    );
    let (run, _) = run_in(&tree, &tree.repo(), "").await;
    let names = source_names(run.one_event("session_started"));
    for bad in ["nofm", "unclosed", "noname"] {
        assert!(
            !names.contains(&bad.to_string()),
            "{bad} is not a valid Agent Skill but is in sources[]: {names:?}"
        );
    }
    // And the skip is explained, once per file, with the reason.
    let notes: Vec<String> = run
        .events_of("error")
        .iter()
        .map(|event| event["message"].as_str().unwrap_or("").to_string())
        .collect();
    for bad in ["nofm", "unclosed", "noname"] {
        assert!(
            notes
                .iter()
                .any(|message| message.contains(&format!("{bad}/SKILL.md"))
                    && message.contains("is not a skill")),
            "{bad} was skipped without a reason: {notes:?}"
        );
    }
}

#[tokio::test]
async fn a_huge_instruction_file_still_starts_and_is_recorded() {
    // Characterisation (see F7): a 3 MB instruction file is capped, not inlined
    // whole, and the truncation is visible in the prompt and in the log. Before
    // the fix the system prompt was 3.1 MB.
    let tree = Tree::new();
    let big = "rule: keep steps small and verified\n".repeat(3 * 1024 * 1024 / 36);
    write(&tree.repo().join("AGENTS.md"), &big);
    let started = std::time::Instant::now();
    let (run, first) = run_in(&tree, &tree.repo(), "").await;
    let system = system_of(&first);
    eprintln!(
        "huge AGENTS.md: system prompt {} bytes, run took {:?}",
        system.len(),
        started.elapsed()
    );
    assert!(
        system.len() < 512 * 1024,
        "the system prompt is {} bytes: the cap (F7) did not hold",
        system.len()
    );
    assert!(
        system.contains("[kelir: truncated"),
        "the truncation is not visible in the prompt"
    );
    assert_eq!(
        run.one_event("session_started")["system"]["hash"]
            .as_str()
            .unwrap()
            .len(),
        16
    );
    // And the log says why the file is short.
    let warnings: Vec<String> = run
        .events_of("error")
        .iter()
        .map(|event| event["message"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        warnings
            .iter()
            .any(|message| message.contains("AGENTS.md") && message.contains("only the first")),
        "no truncation note: {warnings:?}"
    );
}

#[tokio::test]
async fn a_symlink_or_junction_loop_in_a_skills_dir_is_harmless() {
    let tree = Tree::new();
    let skills = tree.repo().join(".claude/skills");
    write(&skills.join("sift/SKILL.md"), SKILL);
    let link = skills.join("self");
    link_dir(&link, &skills);
    // And a loop through the cwd itself.
    let cwd_link = tree.repo().join("loop");
    link_dir(&cwd_link, &tree.repo());
    let (run, _) = run_in(&tree, &tree.repo().join("loop"), "").await;
    let names = source_names(run.one_event("session_started"));
    assert_eq!(
        names.iter().filter(|n| *n == "sift").count(),
        1,
        "{names:?}"
    );
    // Remove the junction without following it.
    #[cfg(windows)]
    {
        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir(&cwd_link);
    }
}

#[tokio::test]
async fn base_prompt_layers_and_the_skills_placeholder() {
    let tree = Tree::new();
    // Machine prompt without the placeholder: the dirs are appended, never lost.
    write(&tree.config().join("prompt.md"), "MACHINE BASE PROMPT");
    let (run, first) = run_in(&tree, &tree.repo(), "").await;
    let system = system_of(&first);
    assert!(system.starts_with("MACHINE BASE PROMPT"), "{system}");
    assert!(system.contains(".claude/skills"), "{system}");
    let started = run.one_event("session_started");
    assert_eq!(started["sources"][0]["name"], "prompt.md");
    assert!(
        started["sources"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("prompt.md")
            && started["sources"][0]["path"] != "embedded",
        "{}",
        started["sources"][0]
    );
    // A project prompt wins over the machine one.
    write(
        &tree.repo().join(".kelir/prompt.md"),
        "PROJECT BASE PROMPT {{skills_dirs}}",
    );
    let (run, first) = run_in(&tree, &tree.repo(), "").await;
    let system = system_of(&first);
    assert!(system.starts_with("PROJECT BASE PROMPT"), "{system}");
    assert!(!system.contains("MACHINE BASE PROMPT"), "{system}");
    assert!(
        run.one_event("session_started")["sources"][0]["path"]
            .as_str()
            .unwrap()
            .contains(".kelir/prompt.md")
    );
}

#[tokio::test]
async fn an_explicit_prompt_file_is_recorded_with_its_path() {
    let tree = Tree::new();
    let prompt = tree.root.join("my-prompt.txt");
    write(&prompt, "EXPLICIT BASE PROMPT");
    let extra = format!(
        "prompt = \"{}\"\n",
        prompt.to_string_lossy().replace('\\', "/")
    );
    let (run, first) = run_in(&tree, &tree.repo(), &extra).await;
    assert!(system_of(&first).starts_with("EXPLICIT BASE PROMPT"));
    let path = run.one_event("session_started")["sources"][0]["path"].clone();
    assert_ne!(
        path, "embedded",
        "the source says it is the embedded default"
    );
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
async fn a_deleted_source_is_quiet_and_a_recreated_one_is_reported() {
    let tree = Tree::new();
    write(&tree.repo().join("AGENTS.md"), "ORIGINAL");
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
    // *after* the call settles, so the file has to change while the call is
    // still running (the scripted call sleeps a second) or the edit races it.
    t.live.wait_for("call_started").await;
    std::fs::remove_file(t.repo.join("AGENTS.md")).unwrap();
    t.live.wait_for("turn_ended").await;
    // There is no text to send, so no `source` event — but the deletion is one
    // `error`, not silence (N4).
    let log = t.live.log();
    assert!(of_type(&log, "source").is_empty(), "deleting sent a source");
    let notes: Vec<String> = of_type(&log, "error")
        .iter()
        .map(|event| event["message"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(
        notes
            .iter()
            .filter(|message| message.contains("was deleted"))
            .count(),
        1,
        "{notes:?}"
    );
    // Re-created with the same text: still nothing (the context carries it).
    write(&t.repo.join("AGENTS.md"), "ORIGINAL");
    assert_eq!(t.live.ctl(&["say", "next"]).await.code, 0);
    t.live.wait_for("turn_ended").await;
    assert!(of_type(&t.live.log(), "source").is_empty());
    // Re-created with other text: one source.
    write(&t.repo.join("AGENTS.md"), "REWRITTEN");
    assert_eq!(t.live.ctl(&["say", "again"]).await.code, 0);
    t.live.wait_for("turn_ended").await;
    let log = t.live.log();
    assert_eq!(
        of_type(&log, "source").len(),
        1,
        "{:?}",
        of_type(&log, "source")
    );
    let (code, stderr) = t.live.end().await;
    assert_eq!(code, 0, "{stderr}");
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
async fn editing_a_file_back_leaves_the_model_with_the_current_text() {
    let tree = Tree::new();
    write(&tree.repo().join("AGENTS.md"), "ORIGINAL-RULE");
    let repo = tree.repo();
    let mut t = live_with(
        tree,
        repo,
        vec![
            Reply::Sse(sse(&text_turn("a", 0))),
            Reply::Sse(sse(&text_turn("b", 0))),
        ],
        &[],
    )
    .await;
    t.live.wait_for("call_started").await;
    write(&t.repo.join("AGENTS.md"), "TEMPORARY-RULE");
    t.live.wait_for("turn_ended").await;
    write(&t.repo.join("AGENTS.md"), "ORIGINAL-RULE");
    assert_eq!(t.live.ctl(&["say", "now?"]).await.code, 0);
    t.live.wait_for("turn_ended").await;
    let requests = t.mock.requests();
    let last = requests.last().unwrap();
    // The most recent message that speaks about the file's text.
    let latest = tail_texts(last)
        .into_iter()
        .rev()
        .find(|t| t.contains("TEMPORARY-RULE") || t.contains("ORIGINAL-RULE"));
    assert!(
        latest
            .as_deref()
            .is_some_and(|t| t.contains("ORIGINAL-RULE")),
        "the file on disk says ORIGINAL-RULE but the latest thing the model was told is {latest:?}"
    );
}

#[tokio::test]
async fn the_skills_verb_can_restore_an_earlier_body() {
    let tree = Tree::new();
    let repo = tree.repo();
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
    for body in ["rule A", "rule B", "rule A"] {
        let answer = t
            .live
            .ctl(&["skills", "--name", "house", "--text", body])
            .await;
        assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
        assert_eq!(t.live.ctl(&["say", "apply"]).await.code, 0);
        t.live.wait_for("turn_ended").await;
    }
    let log = t.live.log();
    let texts: Vec<String> = of_type(&log, "source")
        .iter()
        .map(|s| s["text"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(
        texts,
        vec!["rule A", "rule B", "rule A"],
        "errors: {:?}",
        of_type(&log, "error")
    );
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
