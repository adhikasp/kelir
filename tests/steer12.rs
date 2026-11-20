//! STEER-12: the measured prompt change.
//!
//! Two claims are testable here and nowhere else. First, `scripts/metrics/
//! trajectory.py` reads a session log and prints the numbers the brief argues
//! from, so the BEFORE/AFTER tables in `docs/PROMPT.md` can be re-derived by
//! anyone: the fixture under `tests/fixtures/steer12/` pins the arithmetic.
//! Second, the base prompt is the head of every request's frozen prefix, so the
//! batching and notes-file rules it carries stay pinned by
//! `src/sources.rs`; these tests check the shipped bytes and that the doc that
//! quotes the numbers agrees with the fixture.
//!
//! Only processes started here are Python (no window, own process group) and
//! each is waited for, so nothing survives the test (AGENTS.md hard rules).

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

mod support;

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
        command.creation_flags(kelir::proc::win::CREATE_NO_WINDOW | 0x0000_0200);
    }
}

fn run_script(arguments: &[&str]) -> Option<Output> {
    let interpreter = python()?;
    let mut command = Command::new(interpreter);
    command
        .arg(repo().join("scripts").join("metrics").join("trajectory.py"))
        .args(arguments)
        .current_dir(repo());
    support::scrub_std(&mut command);
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
        "tests/fixtures/steer12/events.jsonl",
        "tests/fixtures/steer12/expected.json",
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

/// The same fixture through the printed table: the deltas the brief argues
/// from (reads share, big -> cheap share, model vs tool minutes) are in the
/// output, so a reader can check a BEFORE/AFTER table against a command.
#[test]
fn the_metrics_script_prints_the_deltas_the_brief_argues_from() {
    let Some(output) = run_script(&["tests/fixtures/steer12/events.jsonl", "--commits", "0"])
    else {
        eprintln!("no python3/python 3.9+ here: skipping the metrics table");
        return;
    };
    assert!(output.status.success(), "{}", stderr_of(&output));
    let text = stdout_of(&output);
    for line in [
        "read calls                        5",
        "big -> cheap share                0.5",
        "model minutes                     1.37",
        "tool minutes (union)              2.22",
        "calls per step                    1.2",
    ] {
        assert!(text.contains(line), "no {line:?} in:\n{text}");
    }
}

/// A quoted `|` is an argument, not a pipe: the read classifier must not cut a
/// `grep -n 'a\|b'` in half and call it a non-read. The fixture holds one such
/// call (step 4); counted wrongly it would drop `read calls` from 5 to 4.
#[test]
fn a_quoted_pipe_inside_grep_is_still_one_read_call() {
    let Some(output) = run_script(&[
        "tests/fixtures/steer12/events.jsonl",
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
        "notes file",
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
        read("tests/fixtures/prompt.md.snapshot"),
        "the byte snapshot and src/prompt.md disagree"
    );
}

/// `docs/PROMPT.md` quotes the BEFORE table and the command that produced it,
/// so the numbers in the repo can be re-derived rather than believed.
#[test]
fn the_prompt_doc_holds_the_before_table_and_its_command() {
    let doc = read("docs/PROMPT.md");
    for needle in [
        "scripts/metrics/trajectory.py",
        "--commits 0",
        "| read calls |",
        "| big -> cheap |",
        "| model minutes |",
        "| tool minutes |",
    ] {
        assert!(doc.contains(needle), "no {needle:?} in docs/PROMPT.md");
    }
    // The six session logs the BEFORE row was measured over, by name.
    for job in [
        "buildinfrafix",
        "steer1fix",
        "steer3",
        "steer5",
        "tui2",
        "tui2fix",
    ] {
        assert!(doc.contains(job), "docs/PROMPT.md does not name {job}");
    }
}
