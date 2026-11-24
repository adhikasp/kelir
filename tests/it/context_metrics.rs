//! STEER-12: the measured prompt change.
//!
//! Two claims are testable here and nowhere else. First, `scripts/metrics/
//! trajectory.py` reads a session log and prints the numbers the brief argues
//! from, so the BEFORE/AFTER tables in `docs/PROMPT.md` can be re-derived by
//! anyone: the fixture under `tests/it/fixtures/steer12/` pins the arithmetic.
//! Second, the base prompt is the head of every request's frozen prefix, so the
//! batching and notes-file rules it carries stay pinned by
//! `src/sources.rs`; these tests check the shipped bytes and that the doc that
//! quotes the numbers agrees with the fixture.
//!
//! Only processes started here are Python (no window, own process group) and
//! each is waited for, so nothing survives the test (AGENTS.md hard rules).

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let path = repo().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("cannot read {rel}: {error}"))
}

/// The interpreter the metrics script runs under, or `None` when this machine
/// has no Python at all. `python3` first (the Linux box), then `python`
/// (Windows); the script itself is standard library only, 3.9+.
fn python() -> Option<String> {
    for name in ["python3", "python"] {
        let mut probe = Command::new(name);
        probe
            .arg("-c")
            .arg("import sys; sys.exit(0 if sys.version_info >= (3, 9) else 1)")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        quiet(&mut probe);
        let works = probe
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if works {
            return Some(name.to_string());
        }
    }
    None
}

/// No console window from a console-less parent, and no shared process group:
/// a stray interpreter cannot be hit by a signal aimed at the test session.
fn quiet(command: &mut Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(kelir::sys::proc::win::CREATE_NO_WINDOW | 0x0000_0200);
    }
}

fn run_script(arguments: &[&str]) -> Option<Output> {
    let interpreter = python()?;
    let mut command = Command::new(interpreter);
    command
        .arg(repo().join("scripts").join("metrics").join("trajectory.py"))
        .args(arguments)
        .current_dir(repo());
    crate::support::scrub_std(&mut command);
    quiet(&mut command);
    Some(command.output().expect("the metrics script runs"))
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The fixture's numbers, field by field, from `expected.json`. This is the
/// unit test the brief asks for: a small `events.jsonl` with known numbers.
#[test]
fn the_metrics_script_reproduces_the_fixtures_known_numbers() {
    let Some(output) = run_script(&[
        "--self-test",
        "tests/it/fixtures/steer12/events.jsonl",
        "tests/it/fixtures/steer12/expected.json",
    ]) else {
        eprintln!("no python3/python 3.9+ here: skipping the metrics self-test");
        return;
    };
    assert!(
        output.status.success(),
        "self-test failed: {}{}",
        stdout_of(&output),
        stderr_of(&output)
    );
    assert!(
        stdout_of(&output).contains("18 fields match"),
        "{}",
        stdout_of(&output)
    );
}

/// A quoted `|` is an argument, not a pipe: the read classifier must not cut a
/// `grep -n 'a\|b'` in half and call it a non-read. The fixture holds one such
/// call (step 4); counted wrongly it would drop `read calls` from 5 to 4.
#[test]
fn a_quoted_pipe_inside_grep_is_still_one_read_call() {
    let Some(output) = run_script(&[
        "tests/it/fixtures/steer12/events.jsonl",
        "--commits",
        "0",
        "--json",
    ]) else {
        eprintln!("no python3/python 3.9+ here: skipping the read classifier");
        return;
    };
    assert!(output.status.success(), "{}", stderr_of(&output));
    let total: serde_json::Value =
        serde_json::from_str(&stdout_of(&output)).expect("the script prints JSON");
    assert_eq!(total["total"]["bash_calls"], 6, "{total}");
    assert_eq!(
        total["total"]["read_calls"], 5,
        "a build piped through tail is not a read; the quoted pipe is: {total}"
    );
}

/// The shipped base prompt: at most 1 KB, the rules the brief asks for, and
/// nothing it dropped.
#[test]
fn the_base_prompt_carries_the_batching_and_the_notes_file() {
    let prompt = read("src/prompt.md");
    assert!(prompt.len() <= 1024, "{} bytes", prompt.len());
    for rule in [
        "One `bash` call per read round",
        "each labelled `echo === name`",
        "three reads in a row is one command",
        "grep -A/-B",
        ".kelir/notes.md",
        "under\n  10 KB",
        // Not a retro rule but a fixture another suite depends on: the live
        // system message is cut at this word to compare the head across runs
        // (`tests/review_steer6.rs`, F1). Renaming it fails that test, so it is
        // pinned here as well as there.
        "Directories:",
    ] {
        assert!(prompt.contains(rule), "no {rule:?} in:\n{prompt}");
    }
    assert!(
        !prompt.contains("Batch independent reads"),
        "the batching rule is now a shell-level one, not a parallel-call claim:\n{prompt}"
    );
    // The parallel-calls capability gate is on for the default model
    // (STEER-6 S2), but the prompt does not lean on it: one command is enough.
    assert!(
        !prompt.contains("in parallel") && !prompt.contains("parallel calls"),
        "the prompt must not promise parallel calls:\n{prompt}"
    );
    assert_eq!(
        prompt,
        read("tests/it/fixtures/prompt.md.snapshot"),
        "the byte snapshot and src/prompt.md disagree"
    );
}
