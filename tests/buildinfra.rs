//! The build and gate infrastructure is part of the product's contract: three
//! agents run these scripts on one desktop, so a script that serializes wrong,
//! deletes a colleague's work, or quietly accepts a flake costs a milestone.
//! These tests read the repo's own configuration and drive `scripts/gate-lock.sh`
//! against a lock path in a temp dir; the only processes they start are that
//! script and the `bash` children it is handed, and each is killed by exact pid
//! when the test ends (AGENTS.md hard rules).

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

/// The values cargo actually resolved, which is the only thing a build obeys:
/// `cargo config get` reads the same config.toml a build does.
#[test]
fn the_dev_and_test_profiles_keep_only_line_tables_of_debug_info() {
    // The same file and the same parser a human reads; `cargo config get` is
    // nightly-only and `cargo metadata` does not report profiles, so the
    // checked value has to come from the TOML itself.
    let config: toml::Value = read(".cargo/config.toml")
        .parse()
        .expect("the cargo config parses as TOML");
    for profile in ["dev", "test"] {
        let debug = &config["profile"][profile]["debug"];
        assert_eq!(
            debug.as_str(),
            Some("line-tables-only"),
            "profile.{profile}.debug should be line-tables-only, got {debug}"
        );
    }
    // A build that only gets the dev profile right is still slow to test.
    assert!(
        config["profile"]["test"].is_table(),
        "[profile.test] must be set explicitly, not inherited by luck"
    );
}

/// The shipped profiles are not negotiable: `release` (and the live-drive
/// matrix) keeps fat LTO and one codegen unit, `review` is the cheap one for
/// reviewer binaries.
#[test]
fn the_shipped_profile_stays_fat_lto_and_review_is_thin() {
    let manifest = read("Cargo.toml");
    let release = manifest
        .split("[profile.release]")
        .nth(1)
        .expect("a [profile.release] section")
        .split("\n[")
        .next()
        .unwrap_or("");
    for (needle, why) in [
        ("lto = true", "release keeps fat LTO"),
        ("codegen-units = 1", "release keeps one codegen unit"),
        ("strip = true", "release stays stripped"),
    ] {
        assert!(release.contains(needle), "{why}: {needle}");
    }

    let review = manifest
        .split("[profile.review]")
        .nth(1)
        .expect("a [profile.review] section for reviewer binaries")
        .split("\n[")
        .next()
        .unwrap_or("");
    assert!(review.contains("inherits = \"release\""), "{review}");
    assert!(review.contains("lto = \"thin\""), "{review}");
    assert!(review.contains("codegen-units = 16"), "{review}");
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
fn a_lock_whose_owner_is_gone_is_taken_over() {
    let (lock, _dir) = sandbox_lock("stale");
    std::fs::create_dir_all(&lock).expect("lock dir");
    // A pid that cannot be alive on any platform this runs on.
    std::fs::write(
        lock.join("owner"),
        "pid 4294967290\nstarted 2026-01-01T00:00:00Z\ncmd test\n",
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
    ] {
        assert!(remote.contains(needle), "{why}: {needle} is missing");
    }
    assert!(
        remote.contains("kill -0"),
        "prune skips a work dir a live run is in"
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
