//! gates examples tests: git_sync_by_default_commits_tr, git_sync_commits_but_does_not_, git_sync_dry_run_changes_nothi, git_sync_refuses_secret_like_n, note_check_appends_the_checker, numbered_answers_resolves_do_1.
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use super::gates_core::*;
use crate::support::*;
use serde_json::{Value, json};

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
