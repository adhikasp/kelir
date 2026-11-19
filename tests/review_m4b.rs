//! Reviewer-added regression probes for the M4 fix round (docs/reviews/M4-review-round2.md).
//!
//! They look for what the fixes could have broken: promoted jobs and a killed
//! session, the two ways a shim is started, a dead shim, pid identity, cwd and
//! pipes. Real processes, no mocks of the OS.

mod support;

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::*;

const NO_PROVIDER: &str = "http://127.0.0.1:9/v1";

fn force_kill(pid: u32) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output();
    }
}

async fn wait_until(what: &str, timeout: Duration, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    loop {
        if check() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Pids of processes whose command line contains `marker` (the lister's own
/// command line is built from two halves, so it never contains it whole).
fn pids_with(marker: &str) -> Vec<u32> {
    #[cfg(windows)]
    {
        let (a, b) = marker.split_at(marker.len() / 2);
        let script = format!(
            "Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -like ('*' + '{a}' + '{b}' + '*') -and $_.Name -ne 'powershell.exe' }} | ForEach-Object {{ $_.ProcessId }}"
        );
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .expect("powershell");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect()
    }
    #[cfg(not(windows))]
    {
        let output = std::process::Command::new("pgrep")
            .args(["-f", marker])
            .output()
            .expect("pgrep");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect()
    }
}

/// What is left, for a failure message: `pid ppid name cmdline`.
fn describe(markers: &[&str]) -> String {
    #[cfg(windows)]
    {
        let clauses: Vec<String> = markers
            .iter()
            .map(|marker| {
                let (a, b) = marker.split_at(marker.len() / 2);
                format!("$_.CommandLine -like ('*' + '{a}' + '{b}' + '*')")
            })
            .collect();
        let script = format!(
            "Get-CimInstance Win32_Process | Where-Object {{ ({}) -and $_.Name -ne 'powershell.exe' }} | ForEach-Object {{ \"$($_.ProcessId) $($_.ParentProcessId) $($_.Name) $($_.CommandLine)\" }}",
            clauses.join(" -or ")
        );
        std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        let mut text = String::new();
        for marker in markers {
            if let Ok(output) = std::process::Command::new("pgrep")
                .args(["-af", marker])
                .output()
            {
                text.push_str(&String::from_utf8_lossy(&output.stdout));
            }
        }
        text
    }
}

fn count_with(markers: &[&str]) -> usize {
    markers.iter().map(|marker| pids_with(marker).len()).sum()
}

fn reap(markers: &[&str]) {
    for marker in markers {
        for pid in pids_with(marker) {
            force_kill(pid);
        }
    }
}

fn text_of(event: &Value, key: &str) -> String {
    event[key].as_str().unwrap_or("").to_string()
}

/// The id of a promoted call is its output directory's name.
fn promoted_id(settled: &Value) -> String {
    let out_path = text_of(settled, "out_path");
    Path::new(&out_path)
        .parent()
        .and_then(|dir| dir.file_name())
        .expect("an output directory")
        .to_string_lossy()
        .into_owned()
}

/// The `shim` pid of the status file whose id is `id` (or any status, when `id` is empty).
fn shim_of(bg: &Path, id: &str) -> Option<u32> {
    for entry in std::fs::read_dir(bg).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".status") || (!id.is_empty() && name != format!("{id}.status")) {
            continue;
        }
        let value: Value =
            serde_json::from_str(&std::fs::read_to_string(entry.path()).ok()?).ok()?;
        if let Some(shim) = value["shim"].as_u64() {
            return Some(shim as u32);
        }
    }
    None
}

// --------------------------------------------------------------------------

/// B1: a `--cwd` with a space and a non-ASCII letter reaches the command on
/// both start-up routes: the session's own shim, and `kelir bg run` inside a
/// call (which on Windows asks the session to start the shim).
#[tokio::test]
async fn b1_cwd_with_a_space_and_a_non_ascii_letter_reaches_both_routes() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_a",
            "bash",
            &json!({"command": "pwd > where.txt"}),
        ))),
        Reply::Sse(sse(&tool_turn(
            "call_b",
            "bash",
            &json!({"command": "\"$KELIR_BIN\" bg run \"pwd > job.txt\" > /dev/null"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("");
    let other = sandbox.cwd.parent().unwrap().join("my dir \u{e9}t\u{e9}");
    std::fs::create_dir_all(&other).unwrap();
    let run = sandbox
        .kelir(
            &mock.base_url(),
            &["run", "--json", "--cwd", other.to_str().unwrap(), "go"],
            &[],
        )
        .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        other.join("where.txt").is_file(),
        "the call did not run in --cwd {}",
        other.display()
    );
    wait_until("the bg run job to write", Duration::from_secs(20), || {
        other.join("job.txt").is_file()
    })
    .await;
    assert!(
        std::fs::read_to_string(other.join("job.txt"))
            .unwrap_or_default()
            .contains("t\u{e9}"),
        "the job ran elsewhere"
    );
}

/// B2: a *promoted* call whose tree holds exec'd grandchildren survives a killed
/// session whole (kill-on-close is cleared on promotion), and a later
/// `bg cancel` still takes every one of them down.
#[tokio::test]
async fn b2_a_promoted_exec_tree_survives_a_killed_session_and_cancel_takes_it() {
    let markers = ["7411.11", "7412.12", "7413.13"];
    let mock = Mock::start(vec![Reply::Sse(sse(&tool_turn(
        "call_pt",
        "bash",
        &json!({"command": "bash -c 'sleep 7411.11 & sleep 7412.12 & wait' & sleep 7413.13 & wait"}),
    )))])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "promote_after = 3\n",
    )
    .await;
    let settled = live.wait_for("tool_settled").await;
    assert_eq!(settled["outcome"], "promoted", "{settled}");
    let id = promoted_id(&settled);
    wait_until("the grandchildren", Duration::from_secs(20), || {
        count_with(&markers) >= 3
    })
    .await;
    live.kill().await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let survivors = count_with(&markers);
    let sandbox = Sandbox::from_live(&live);
    let cancelled = sandbox
        .kelir(NO_PROVIDER, &["bg", "cancel", &id], &[])
        .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let left = count_with(&markers);
    let remaining = describe(&markers);
    reap(&markers);
    assert!(
        survivors >= 3,
        "a promoted tree lost {} of 3 processes when its session was killed",
        3 - survivors.min(3)
    );
    assert_eq!(cancelled.code, 0, "{}", cancelled.stderr);
    assert_eq!(
        left, 0,
        "cancel left {left} process(es) of the promoted tree:
{remaining}"
    );
}

/// B3: the shim of an *unpromoted* call is killed from outside while the session
/// lives. The call must settle (`unknown`), not hang, and its tree must go.
#[tokio::test]
async fn b3_an_unpromoted_call_whose_shim_is_killed_settles_and_its_tree_goes() {
    let markers = ["7421.21", "7422.22"];
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_ds",
            "bash",
            &json!({"command": "bash -c 'sleep 7421.21 & sleep 7422.22 & wait'"}),
        ))),
        Reply::Sse(sse(&text_turn("noted", 0))),
    ])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    live.wait_for("tool_started").await;
    let bg = live.session_dir.join("bg");
    wait_until("the call's status", Duration::from_secs(20), || {
        shim_of(&bg, "").is_some() && count_with(&markers) >= 2
    })
    .await;
    force_kill(shim_of(&bg, "").unwrap());
    let settled = live.wait_for("tool_settled").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let left = count_with(&markers);
    reap(&markers);
    live.kill().await;
    assert_eq!(settled["outcome"], "unknown", "{settled}");
    assert_eq!(left, 0, "the dead shim's tree is still running");
}

/// B4 (round-2 finding G1, red at 5779d3d): a status whose pid was recycled *within two minutes* of the job starting
/// is somebody else's process. The identity check tolerates 120 s between the
/// status's `started` and the process's start time, which is exactly the window
/// in which Windows reuses a pid.
#[tokio::test]
async fn b4_cancel_does_not_kill_a_pid_recycled_within_the_tolerance() {
    let sandbox = Sandbox::new("");
    let mut victim = std::process::Command::new("ping")
        .args(if cfg!(windows) {
            vec!["-n", "120", "127.0.0.1"]
        } else {
            vec!["-c", "120", "127.0.0.1"]
        })
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn the victim");
    let pid = victim.id();
    // The job "started" 45 s before this process did: a shim that died 45 s in,
    // whose pid the victim then got.
    let started = (time::OffsetDateTime::now_utc() - time::Duration::seconds(45))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let bg = sandbox.state.join("bg");
    std::fs::create_dir_all(&bg).unwrap();
    let stale = json!({
        "id": "bg_stale45", "state": "running", "pid": pid, "shim": pid,
        "started": started, "cmd": "long gone",
        "out": bg.join("bg_stale45.out").to_str().unwrap().replace('\\', "/"),
        "kind": "job"
    });
    std::fs::write(bg.join("bg_stale45.status"), stale.to_string()).unwrap();
    let cancelled = sandbox
        .kelir(NO_PROVIDER, &["bg", "cancel", "bg_stale45"], &[])
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let alive = victim.try_wait().unwrap().is_none();
    let _ = victim.kill();
    assert!(
        alive,
        "bg cancel killed pid {pid}, started 45 s after the job's `started` (stdout {:?})",
        cancelled.stdout
    );
}

/// B5: a graceful end of a non-`--keep` run takes a leftover `cmd &` child with
/// it, as DEVIATIONS documents ("it dies when the session does").
#[tokio::test]
async fn b5_a_graceful_exit_takes_a_leftover_child_with_it() {
    let markers = ["7451.51"];
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_lo",
            "bash",
            &json!({"command": "sleep 7451.51 > /dev/null 2>&1 &"}),
        ))),
        Reply::Sse(sse(&text_turn("left it", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "go"], &[])
        .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let left = count_with(&markers);
    let remaining = describe(&markers);
    reap(&markers);
    assert_eq!(
        left, 0,
        "a leftover `&` child outlived a graceful exit:
{remaining}"
    );
}

/// B6: `kelir bg run` inside a call, and a promoted call, must not hold the
/// caller's stdout pipe: `kelir run --json | jq` ends with the turn.
#[tokio::test]
async fn b6_neither_a_bg_run_in_a_call_nor_a_promoted_call_holds_the_pipe() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_br",
            "bash",
            &json!({"command": "\"$KELIR_BIN\" bg run 'sleep 14'"}),
        ))),
        Reply::Sse(sse(&tool_turn(
            "call_pr",
            "bash",
            &json!({"command": "sleep 14"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("promote_after = 2\n");
    let started = Instant::now();
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "go"], &[])
        .await;
    let took = started.elapsed();
    reap(&["sleep 14"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        took < Duration::from_secs(10),
        "the pipe stayed open for {took:?} (a 14 s job held it)"
    );
}

/// B7 (round-2 finding G2, red at 5779d3d on Windows): a *promoted* job whose shim dies is `lost`; `bg cancel` must still be able
/// to stop its tree (Windows reaches it by the job object's name).
#[tokio::test]
async fn b7_a_lost_promoted_job_can_still_be_cancelled() {
    let markers = ["7471.71", "7472.72"];
    let mock = Mock::start(vec![Reply::Sse(sse(&tool_turn(
        "call_lp",
        "bash",
        &json!({"command": "bash -c 'sleep 7471.71 & sleep 7472.72 & wait'"}),
    )))])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "promote_after = 3\n",
    )
    .await;
    let settled = live.wait_for("tool_settled").await;
    assert_eq!(settled["outcome"], "promoted", "{settled}");
    let id = promoted_id(&settled);
    let bg = live.session_dir.join("bg");
    wait_until("the grandchildren", Duration::from_secs(20), || {
        count_with(&markers) >= 2
    })
    .await;
    live.kill().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    force_kill(shim_of(&bg, &id).expect("a shim pid"));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let sandbox = Sandbox::from_live(&live);
    let waited = sandbox
        .kelir(NO_PROVIDER, &["bg", "wait", &id, "--timeout", "10"], &[])
        .await;
    let cancelled = sandbox
        .kelir(NO_PROVIDER, &["bg", "cancel", &id], &[])
        .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let left = count_with(&markers);
    let remaining = describe(&markers);
    reap(&markers);
    assert!(
        waited.stdout.contains("lost"),
        "bg wait on a job without a shim: {:?} {:?}",
        waited.stdout,
        waited.stderr
    );
    assert_eq!(
        left, 0,
        "cancel on a lost job left {left} process(es) running (stdout {:?}):
{remaining}",
        cancelled.stdout
    );
}
