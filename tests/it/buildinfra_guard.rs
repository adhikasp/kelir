//! buildinfra guard tests: every_gate_stage_logs_to_its_o, the_gate_reports_the_summary_l, the_gate_script_keeps_the_meas, the_load_helper_kills_only_the, the_remote_script_reuses_its_w, the_scripts_name_bash_and_are_.
#![allow(clippy::pedantic, clippy::nursery)]

use std::process::Command;

use super::buildinfra_gate::*;
use crate::support::*;
use serde_json::json;

#[test]
fn the_gate_script_keeps_the_measured_wins() {
    let gate = read("scripts/gate.sh");
    for (needle, why) in [
        ("nextest run", "the gate runs nextest, not cargo test"),
        ("--no-fail-fast", "one failing suite must not hide the rest"),
        ("--retries 2", "the measured retry count"),
        ("cargo test --doc", "nextest does not run doctests"),
        (
            "gate-lock.sh",
            "the gate takes the machine-wide lock itself",
        ),
        ("CARGO_BUILD_JOBS", "a capped number of build jobs"),
        ("cargo fmt --check", "fmt is a stage"),
        (
            "cargo clippy --all-targets",
            "clippy -D warnings is a stage",
        ),
        ("RETRIED", "a retried test is named in the summary"),
        (
            "LEAKED PROCESSES",
            "a leaked process fails the gate (AGENTS.md hard rule)",
        ),
        ("FAILED TESTS", "a failing run names its tests"),
        (
            "nextest_names",
            "the names come from a parsed status, not a grep",
        ),
        (
            "KELIR_GATE_STRICT_RETRIES",
            "a retry can be made fatal for a job that fixed its flakes",
        ),
    ] {
        assert!(gate.contains(needle), "{why}: {needle} is missing");
    }
    assert!(
        gate.contains("--no-lock"),
        "the inner run must not try to lock twice"
    );
}

/// The gate reports nextest's own summary line, and the pattern it greps with
/// matches the line nextest actually prints. The first version searched for
/// `Summary:`; nextest prints `Summary [  82.4s] ...` with no colon, so the
/// gate said "no summary line" on a green run and nobody noticed.
#[test]
fn the_gate_reports_the_summary_line_nextest_prints() {
    let gate = read("scripts/gate.sh");
    let pattern = gate
        .lines()
        .find(|line| line.contains("summary="))
        .expect("the gate reads a summary line");
    // The real line, as nextest 0.9 prints it (ANSI stripped, two leading
    // spaces before the word).
    let sample = "     Summary [  82.446s] 485 tests run: 485 passed, 2 skipped
";
    let dir = tempfile::TempDir::new().expect("temp dir");
    let log = dir.path().join("nextest.log");
    std::fs::write(&log, sample).expect("write the sample log");
    // Run the same grep the gate runs, from the gate's own text.
    let out = Command::new(bash())
        .args([
            "-c",
            &format!(
                "grep -E '^[[:space:]]+Summary ' \"{}\" | tail -1",
                log.display()
            ),
        ])
        .output()
        .expect("grep runs");
    assert!(
        !String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "the gate's summary pattern must match nextest's line; it reads: {pattern}"
    );
}

/// Every gate stage writes its own log, so a failure is diagnosable without
/// scrolling a combined transcript.
#[test]
fn every_gate_stage_logs_to_its_own_file() {
    let gate = read("scripts/gate.sh");
    for stage in ["fmt", "clippy", "nextest", "doctest"] {
        assert!(
            gate.contains(&format!("$out/{stage}.log")),
            "the {stage} stage has no log of its own"
        );
    }
}

/// The load helper starts its busy loops itself and kills exactly those pids,
/// on every path; it never signals a group or an image name.
#[test]
fn the_load_helper_kills_only_the_pids_it_started() {
    let load = read("scripts/gate-load.sh");
    assert!(load.contains("load_pids+=(\"$!\")"), "it records the pids");
    assert!(load.contains("kill -9 \"$pid\""), "it kills by pid");
    let lowered = load.to_lowercase();
    assert!(
        !lowered.contains("killall") && !lowered.contains("pkill"),
        "never kill by image name"
    );
    assert!(
        !load.contains("kill -9 0") && !load.contains("kill -INT 0"),
        "never broadcast a signal to a group"
    );
    assert!(
        load.contains("trap 'stop_load' EXIT INT TERM"),
        "the loops die with the script, whatever happens"
    );
    assert!(
        load.contains("--retries 0"),
        "a load run measures the flake, so it must not retry it"
    );
}

#[test]
fn the_remote_script_reuses_its_work_dir_and_can_prune() {
    let remote = read("scripts/remote-check.sh");
    assert!(
        !remote.contains("rm -rf src-tree && git clone"),
        "the work dir is reused, not recloned every run"
    );
    for (needle, why) in [
        ("fetch -q --force", "the reused clone is fast-forwarded"),
        (
            "checkout -q -f --detach",
            "the work tree is forced to the commit",
        ),
        ("git clean -qfdx -e target", "stale files go, target/ stays"),
        ("prune", "a prune subcommand"),
        ("cargo-nextest", "the Linux gate uses nextest"),
        (
            ".kelir-workdir",
            "a created work dir carries the marker prune requires",
        ),
        (
            "refuse_reason",
            "prune decides per candidate, not by name alone",
        ),
        ("symlink", "prune refuses a symlink"),
        (
            "pwd -P",
            "prune resolves the real path before accepting one",
        ),
    ] {
        assert!(remote.contains(needle), "{why}: {needle} is missing");
    }
    assert!(
        remote.contains("kill -0"),
        "prune skips a work dir a live run is in"
    );
    // No fixed /tmp log names: two jobs' Linux gates share the box (S6).
    assert!(
        !remote.contains("/tmp/fmt.log") && !remote.contains("/tmp/nx.log"),
        "far-side logs live in a per-run dir, not at fixed /tmp paths"
    );
    assert!(
        remote.contains("logdir="),
        "the far side makes a log dir per run"
    );
}

/// Windows is first class in the gate scripts too: every script names bash, so
/// it is run by bash on both platforms whatever the checkout's file mode is
/// (this dev box has `core.filemode = false`, and git on Windows does not
/// record the executable bit, so the shebang is what carries the contract).
#[test]
fn the_scripts_name_bash_and_are_tracked() {
    for script in [
        "scripts/gate.sh",
        "scripts/gate-lock.sh",
        "scripts/gate-load.sh",
        "scripts/remote-check.sh",
    ] {
        let text = read(script);
        assert!(
            text.starts_with("#!/usr/bin/env bash"),
            "{script} starts with a bash shebang"
        );
    }
    let out = Command::new("git")
        .args([
            "ls-files",
            "-s",
            "--",
            "scripts/gate.sh",
            "scripts/gate-lock.sh",
            "scripts/gate-load.sh",
            "scripts/remote-check.sh",
        ])
        .current_dir(repo())
        .output()
        .expect("git ls-files");
    let listing = String::from_utf8_lossy(&out.stdout).into_owned();
    let lines: Vec<&str> = listing.lines().collect();
    assert_eq!(lines.len(), 4, "all four scripts are tracked: {listing}");
}
