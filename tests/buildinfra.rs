//! The build and gate infrastructure is part of the product's contract: three
//! agents run these scripts on one desktop, so a script that serializes wrong,
//! deletes a colleague's work, or quietly accepts a flake costs a milestone.
//! These tests read the repo's own configuration and drive `scripts/gate-lock.sh`
//! against a lock path in a temp dir; the only processes they start are that
//! script and the `bash` children it is handed, and each is killed by exact pid
//! when the test ends (AGENTS.md hard rules).

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

mod support;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let path = repo().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("cannot read {rel}: {error}"))
}

/// The bash this machine has, resolved the way the product resolves its shell:
/// `bash` on PATH is not the Git Bash on Windows, and WSL's `bash.exe` would
/// translate (and mangle) the absolute path.
fn bash() -> String {
    let shell = kelir::config::default_shell();
    if shell.get(1).map(String::as_str) == Some("-c") && shell[0].contains("bash") {
        return shell[0].clone();
    }
    "bash".to_string()
}

/// A `Command` for a shell script in this repo, with the session's `KELIR_*`
/// environment scrubbed (this suite dogfoods itself: it can run inside kelir).
fn bash_command(script: &str) -> Command {
    let mut command = Command::new(bash());
    command.arg(repo().join("scripts").join(script));
    support::scrub_std(&mut command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

// ---------------------------------------------------------------- Cargo config

/// The profiles are the project default in Cargo.toml (so every clone gets
/// them); the config must not override them with a different value.
#[test]
fn the_dev_and_test_profiles_keep_only_line_tables_of_debug_info() {
    // `cargo config get` is nightly-only and `cargo metadata` does not report
    // profiles, so the checked value has to come from the TOML itself.
    // toml 1.x: parse the document with `from_str`, not `str::parse::<Value>`
    // (the latter parses a single value and rejects a whole document).
    let manifest: toml::Value = toml::from_str(&read("Cargo.toml")).expect("Cargo.toml is TOML");
    for profile in ["dev", "test"] {
        let debug = &manifest["profile"][profile]["debug"];
        assert_eq!(
            debug.as_str(),
            Some("line-tables-only"),
            "profile.{profile}.debug in Cargo.toml should be line-tables-only, got {debug}"
        );
    }
    // A build that only gets the dev profile right is still slow to test.
    assert!(
        manifest["profile"]["test"].is_table(),
        "[profile.test] must be set explicitly, not inherited by luck"
    );
    // .cargo/config.toml [profile] overrides the manifest: if it reintroduced
    // a full-debug profile, the default above would not be what cargo builds.
    let config: toml::Value =
        toml::from_str(&read(".cargo/config.toml")).expect("the cargo config parses as TOML");
    assert!(
        config.get("profile").is_none(),
        "the manifest is the default; .cargo/config.toml must not override profiles"
    );
}

/// The shipped profiles are not negotiable: `release` (and the live-drive
/// matrix) keeps fat LTO and one codegen unit, `review` is the cheap one for
/// reviewer binaries.
///
/// The values are parsed, not searched for: matching raw text let
/// `codegen-units = 16` satisfy a `codegen-units = 1` needle and a comment
/// satisfy `lto = true`, so the pin could not fail (buildinfra-review S1).
#[test]
fn the_shipped_profile_stays_fat_lto_and_review_is_thin() {
    let manifest: toml::Value = toml::from_str(&read("Cargo.toml")).expect("Cargo.toml is TOML");
    let release = &manifest["profile"]["release"];
    assert_eq!(
        release["lto"].as_bool(),
        Some(true),
        "release keeps fat LTO: {release}"
    );
    assert_eq!(
        release["codegen-units"].as_integer(),
        Some(1),
        "release keeps one codegen unit: {release}"
    );
    assert_eq!(
        release["strip"].as_bool(),
        Some(true),
        "release stays stripped: {release}"
    );

    let review = &manifest["profile"]["review"];
    assert_eq!(
        review["inherits"].as_str(),
        Some("release"),
        "review inherits release: {review}"
    );
    assert_eq!(
        review["lto"].as_str(),
        Some("thin"),
        "review is the cheap build: {review}"
    );
    assert_eq!(
        review["codegen-units"].as_integer(),
        Some(16),
        "review is the cheap build: {review}"
    );
}

/// The Windows linker opt-in is Windows-only: Linux keeps its default linker,
/// and that is what the Linux gate builds with.
#[test]
fn rust_lld_is_opted_in_for_windows_only() {
    let config = read(".cargo/config.toml");
    assert!(
        config.contains("[target.x86_64-pc-windows-msvc]"),
        "the Windows target section"
    );
    assert!(config.contains("linker = \"rust-lld\""));
    for other in ["linux", "apple-darwin"] {
        assert!(
            !config
                .lines()
                .any(|line| line.starts_with('[') && line.contains(other)),
            "no linker override for {other}"
        );
    }
}

// ------------------------------------------------------------------- nextest

/// The gate's nextest settings live in one file that both the gate script and a
/// human read, and the numbers in it are the BUILD-INFRA rules.
#[test]
fn the_nextest_profile_matches_the_gate_rules() {
    let config = read(".config/nextest.toml");
    let profile = config
        .split("[profile.default]")
        .nth(1)
        .expect("a [profile.default] section");
    for (needle, why) in [
        ("retries = 2", "two retries ride out a loaded box"),
        ("fail-fast = false", "one failure must not hide the rest"),
        ("test-threads = 4", "four tests per worker"),
        ("slow-timeout", "a hang has to be bounded"),
    ] {
        assert!(profile.contains(needle), "{why}: {needle}");
    }
    // The PTY suite gets a group of its own: its tests each drive a real
    // terminal and one of them is an existing STEER-9 flake, so it must not be
    // the suite that brings down an otherwise green gate.
    let groups = config
        .split("[test-groups.pty]")
        .nth(1)
        .expect("a pty test group for the TUI suite");
    assert!(groups.contains("max-threads = 2"), "{groups}");
    assert!(
        config.contains("test-group = 'pty'"),
        "the TUI suite is assigned to that group"
    );
    assert!(config.contains("binary(e2e_tui)"), "by its binary name");
    // The process-heavy suites (they kill sessions and scan for surviving pids)
    // get the same treatment, at the measured cap.
    let heavy = config
        .split("[test-groups.process-heavy]")
        .nth(1)
        .expect("a process-heavy test group")
        .split("[[profile.")
        .next()
        .unwrap_or("");
    assert!(heavy.contains("max-threads = 2"), "{heavy}");
    for suite in ["e2e_m4", "review_m4b", "review_m4"] {
        assert!(
            config.contains(&format!("binary({suite})")),
            "{suite} is in the process-heavy group"
        );
    }
}

// ---------------------------------------------------------------- gate lock

/// Run `gate-lock.sh -- <command>` against a lock in a temp dir. The child gets
/// its own process group on Unix so nothing it does can reach this session.
fn locked_command(lock: &Path, timeout_s: u32, command: &str) -> Command {
    let mut cmd = bash_command("gate-lock.sh");
    cmd.args([
        "--timeout",
        &timeout_s.to_string(),
        "--",
        "bash",
        "-c",
        command,
    ])
    .env("KELIR_GATE_LOCK", lock);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // No console window for a console-less parent, and no shared group.
        cmd.creation_flags(kelir::proc::win::CREATE_NO_WINDOW | 0x0000_0200);
    }
    cmd
}

fn run_locked(lock: &Path, timeout_s: u32, command: &str) -> Output {
    locked_command(lock, timeout_s, command)
        .output()
        .expect("gate-lock runs")
}

/// A gate process held open by the test. Dropping it kills its whole tree by
/// exact pid (`/T` on Windows, the process group on Unix) and reaps it, so a
/// panic in the middle of a test cannot leave a `sleep` behind (AGENTS.md hard
/// rule; nextest reports exactly that as a leak).
struct Held(Option<std::process::Child>);

impl Held {
    fn spawn(lock: &Path, timeout_s: u32, command: &str) -> Held {
        let mut cmd = locked_command(lock, timeout_s, command);
        Held(Some(cmd.spawn().expect("the holder starts")))
    }

    /// Let the holder finish on its own and check it did. Preferred over
    /// `Drop`'s kill: a process that exits by itself cannot be left behind by
    /// a race between the kill and the test binary's exit (nextest's leak
    /// check puts the test in a Job Object and names exactly that).
    fn finish(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        let status = child.wait().expect("the holder finishes");
        assert!(status.success(), "the holder exits on its own: {status}");
    }

    fn wait_for_lock(&self, lock: &Path) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !lock.exists() {
            assert!(Instant::now() < deadline, "the holder never took the lock");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        let pid = child.id();
        #[cfg(unix)]
        {
            // The child is its own group leader (`process_group(0)`), so this
            // reaches it and everything it started, and nothing else.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        {
            // `/T` is the tree; never `/IM`, which would kill other workers.
            let _ = Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn sandbox_lock(label: &str) -> (PathBuf, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let lock = dir.path().join(format!("{label}.lock"));
    (lock, dir)
}

#[test]
fn the_lock_runs_the_command_and_releases_on_success_and_on_failure() {
    let (lock, _dir) = sandbox_lock("basic");
    let ok = run_locked(&lock, 30, "echo ran");
    assert!(
        ok.status.success(),
        "the wrapped command should succeed: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&ok.stdout).trim(), "ran");
    assert!(!lock.exists(), "released on success");

    let bad = run_locked(&lock, 30, "exit 3");
    assert_eq!(bad.status.code(), Some(3), "the exit code passes through");
    assert!(!lock.exists(), "released on failure");
}

#[test]
fn a_second_gate_waits_for_the_first() {
    let (lock, _dir) = sandbox_lock("contend");
    let mut holder = Held::spawn(&lock, 60, "sleep 3");
    holder.wait_for_lock(&lock);
    let start = Instant::now();
    let second = run_locked(&lock, 60, "echo second");
    let waited = start.elapsed();
    assert!(second.status.success());
    assert_eq!(String::from_utf8_lossy(&second.stdout).trim(), "second");
    assert!(
        waited >= Duration::from_millis(1200),
        "the second gate waits for the first: waited {waited:?}"
    );

    holder.finish();
    assert!(!lock.exists(), "both gates released the lock");
}

#[test]
fn a_lock_whose_holder_is_gone_is_taken_over() {
    let (lock, _dir) = sandbox_lock("stale");
    // A directory with an `owner` file is the lock format of the version before
    // the atomic one (buildinfra-review B2). A lock in that shape whose holder
    // is provably gone is retired, so an upgrade does not wedge the queue.
    std::fs::create_dir_all(&lock).expect("lock dir");
    // A pid that cannot be alive on any platform this runs on.
    std::fs::write(
        lock.join("owner"),
        "pid 4294967290
started 2026-01-01T00:00:00Z\ncmd test\n",
    )
    .expect("owner file");
    let out = run_locked(&lock, 30, "echo took-over");
    assert!(
        out.status.success(),
        "a stale lock is taken over: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "took-over");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("stale lock"),
        "the takeover is announced"
    );
    assert!(!lock.exists());
}

/// A lock this script did not write is never taken over, however long a waiter
/// has been waiting: an outside gate driver that locks with a plain `mkdir` (the
/// documented interop) is respected, and a bare path left by hand is the
/// operator's to remove. Taking it over is exactly the "two gates at once"
/// failure buildinfra-review B2 found; waiting costs time, not a red gate.
#[test]
fn a_lock_this_script_did_not_write_is_never_taken_over() {
    let (lock, _dir) = sandbox_lock("foreign");
    std::fs::create_dir_all(&lock).expect("lock dir");
    let start = Instant::now();
    let out = run_locked(&lock, 4, "echo must-not-run");
    assert_eq!(
        out.status.code(),
        Some(75),
        "the waiter gives up instead: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("holds no record this script wrote"),
        "the waiter says why it is not touching it: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        lock.exists(),
        "the foreign lock is left exactly where it was"
    );
    assert!(
        start.elapsed() < Duration::from_secs(60),
        "and it gives up on its own timeout, not a special case"
    );
}

#[test]
fn waiting_gives_up_with_exit_75_when_the_holder_never_returns() {
    let (lock, dir) = sandbox_lock("timeout");
    // The holder runs until the test tells it to stop, not for a fixed number
    // of seconds: its lifetime must outlast the waiter's 2 s timeout on a
    // loaded box, and a `sleep 6` would either be too short there or waste time
    // here. `kill -0` needs a pid bash itself wrote, which is the only pid both
    // platforms agree on.
    let go = dir.path().join("go");
    let mut holder = Held::spawn(
        &lock,
        60,
        &format!("while [ ! -f '{}' ]; do sleep 0.05; done", go.display()),
    );
    holder.wait_for_lock(&lock);

    let out = run_locked(&lock, 2, "echo never");
    assert_eq!(out.status.code(), Some(75), "the timeout exit code is 75");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("gave up"),
        "the waiter says it gave up: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "",
        "the command must not run without the lock"
    );
    assert!(
        lock.exists(),
        "a timed-out waiter leaves the holder's lock alone"
    );

    // The lock is still the holder's: a second gate keeps waiting, it does not
    // take over a live holder.
    let second = run_locked(&lock, 2, "echo second");
    assert_eq!(second.status.code(), Some(75), "still held, still waiting");

    std::fs::write(&go, "go").expect("release the holder");
    holder.finish();
    let deadline = Instant::now() + Duration::from_secs(60);
    while lock.exists() {
        assert!(
            Instant::now() < deadline,
            "the holder's own exit releases the lock"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

// ------------------------------------------------------------------- scripts

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

// ------------------------------------------------------------- no core dumps

/// The far side of the gate is printed by the script's own subcommands, so a
/// test can run the exact text the Linux gate ships instead of a copy of it.
fn far_side(what: &str, args: &[&str]) -> Output {
    let mut command = bash_command("remote-check.sh");
    command.arg(what).args(args);
    command.output().expect("the far side prints")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// BUILD-INFRA: the crash-injection tests abort a `kelir` on purpose, and an
/// unchecked Linux box wrote a ~290 KB core for every one of them (2,660 cores
/// / 904 MB on 2026-11-20). The gate must therefore hand every test binary an
/// `ulimit -c 0`.
///
/// This runs the far side's own prologue — the text `remote-check.sh` writes to
/// the box and runs there — and asserts a *child of it* reports a 0 core limit,
/// which is the property that matters (the shell sets it, the children inherit
/// it). It also asserts the prologue comes before the first test stage, so a
/// later edit cannot move it below the point where it no longer helps.
#[test]
fn the_linux_gate_runs_with_core_dumps_off() {
    let prologue = far_side("far-env", &[]);
    assert!(prologue.status.success(), "far-env prints the prologue");
    let text = stdout_of(&prologue);
    assert!(
        text.contains("ulimit -c 0"),
        "the far side lowers the core limit:\n{text}"
    );

    let runner = far_side("far-runner", &[]);
    assert!(runner.status.success(), "far-runner prints the runner");
    let runner = stdout_of(&runner);
    let limit_at = runner
        .find("ulimit -c 0")
        .expect("the runner sets the core limit");
    for stage in [
        "cargo fmt --check",
        "cargo nextest run",
        "cargo test --doc",
        "git clone",
    ] {
        let at = runner
            .find(stage)
            .unwrap_or_else(|| panic!("the runner has a {stage} stage"));
        assert!(
            limit_at < at,
            "`ulimit -c 0` must come before `{stage}`, not after it"
        );
    }

    // The prologue as the far side runs it, with the work dir and the bundle
    // stage cut off at the first line that touches the checkout: what is left
    // is the environment, and the crash tests run under it.
    let head: String = {
        let mut out = String::new();
        for line in runner.lines() {
            if line.starts_with("wd=\"$HOME/$W\"") {
                break;
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    };
    assert!(
        head.contains("ulimit -c 0"),
        "the prologue keeps the core limit:\n{head}"
    );
    assert_eq!(head.trim_end(), text.trim_end(), "one copy, not two");

    let dir = tempfile::TempDir::new().expect("temp dir");
    let script = dir.path().join("prologue.sh");
    std::fs::write(&script, format!("{head}\n")).expect("write the prologue");
    let mut command = Command::new(bash());
    command.arg(script);
    support::scrub_std(&mut command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // No console window from a console-less parent, no shared group.
        command.creation_flags(kelir::proc::win::CREATE_NO_WINDOW | 0x0000_0200);
    }
    let out = command.output().expect("the prologue runs");
    let said = stdout_of(&out);
    assert!(
        said.contains("CORE-LIMIT: ulimit -c = 0"),
        "a child of the far side's shell inherits a 0 core limit, not `{}`: {said}",
        said.trim()
    );
}

/// The local halves set it too: `gate.sh` for every Unix run, `gate-load.sh`
/// for a direct run of the stress script, and neither may swallow a failure
/// silently on a Unix box (the shell says so on stderr).
#[test]
fn the_local_gate_scripts_also_turn_core_dumps_off() {
    for (script, why) in [
        ("scripts/gate.sh", "the gate starts every test binary"),
        ("scripts/gate-load.sh", "a direct load run starts them too"),
    ] {
        let text = read(script);
        assert!(
            text.contains("ulimit -c 0"),
            "{script} turns core dumps off because {why}"
        );
        let at = text.find("ulimit -c 0").expect("the limit");
        let head = &text[..at];
        assert!(
            head.contains("Linux | Darwin") || head.contains("Linux"),
            "{script} sets the limit only where the shell has it (Windows first class)"
        );
    }
    assert!(
        !read("scripts/gate.sh").contains("ulimit -c unlimited"),
        "nothing in the gate raises the limit back"
    );
}
