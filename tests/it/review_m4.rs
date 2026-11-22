//! Reviewer-added adversarial tests for M4 (see docs/reviews/M4-review.md).
//!
//! These try to break background jobs rather than mirror the implementation: real
//! processes, real shims, no mocks of the OS. A failing test here is a real bug;
//! each names its finding.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::Path;
use std::time::{Duration, Instant};

use crate::support::*;
use serde_json::{Value, json};

const NO_PROVIDER: &str = "http://127.0.0.1:9/v1";

fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let tasklist = Path::new(&system_root)
            .join("System32")
            .join("tasklist.exe");
        std::process::Command::new(tasklist)
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\"")))
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("ps")
            .args(["-p", &pid.to_string()])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
}

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

fn bytes_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
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

/// `bg run` prints the id first.
fn id_of(output: &Output) -> String {
    output
        .stdout
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

fn status_of(sandbox: &Sandbox, id: &str) -> Value {
    let path = sandbox.state.join("bg").join(format!("{id}.status"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_default())
        .unwrap_or_else(|_| panic!("no readable status at {}", path.display()))
}

/// The first `commands/*/out` of the only session in the sandbox.
fn first_command_out(sandbox: &Sandbox) -> String {
    let session = sandbox.session_dir(&sandbox.only_session());
    let mut dirs: Vec<_> = std::fs::read_dir(session.join("commands"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    std::fs::read_to_string(dirs[0].join("out")).unwrap_or_default()
}

// --------------------------------------------------------------------------

/// R1 (F1): `--cwd` is the directory a bash call runs in. M0..M3 set
/// `current_dir(ctx.cwd)` on the shell; since M4 the shell is started by the
/// shim, which is started with no `current_dir`, so the call runs in the
/// *process's* directory instead.
#[tokio::test]
async fn r1_bash_calls_run_in_the_cwd_the_session_was_given() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_cwd",
            "bash",
            &json!({"command": "pwd > where.txt"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("");
    let other = sandbox.cwd.parent().unwrap().join("other");
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
        "the call ran in the process directory ({}), not in --cwd {}: where.txt = {:?}",
        sandbox.cwd.display(),
        other.display(),
        std::fs::read_to_string(sandbox.cwd.join("where.txt")).ok()
    );
}

/// R2 (F2): `kelir bg run` joins its words with spaces, so any argument that
/// contains a space or a quote loses its boundary. A model writes
/// `kelir bg run npm test -- --grep "a b"` the way it would write any command.
#[tokio::test]
async fn r2_bg_run_keeps_argument_boundaries() {
    let sandbox = Sandbox::new("");
    let started = sandbox
        .kelir(
            NO_PROVIDER,
            &["bg", "run", "printf", "[%s]", "a b", "c"],
            &[],
        )
        .await;
    assert_eq!(started.code, 0, "{}", started.stderr);
    let id = id_of(&started);
    let waited = sandbox
        .kelir(NO_PROVIDER, &["bg", "wait", &id, "--timeout", "20"], &[])
        .await;
    assert_eq!(waited.code, 0, "{}", waited.stderr);
    let out = sandbox.kelir(NO_PROVIDER, &["bg", "out", &id], &[]).await;
    assert_eq!(
        out.stdout, "[a b][c]",
        "argument boundaries were lost: {:?}",
        out.stdout
    );
}

/// R3 (F3): a shim killed from outside (`taskkill /F /IM kelir.exe`, an OOM
/// kill, a reboot) leaves `state: running` for ever. `bg wait` hangs, `bg ls`
/// lies, `resume` never settles the job.
#[tokio::test]
async fn r3_a_dead_shim_does_not_leave_the_job_running_for_ever() {
    let sandbox = Sandbox::new("");
    let started = sandbox
        .kelir(NO_PROVIDER, &["bg", "run", "sleep 20"], &[])
        .await;
    assert_eq!(started.code, 0, "{}", started.stderr);
    let id = id_of(&started);
    let status = status_of(&sandbox, &id);
    let shim = status["shim"].as_u64().expect("shim pid") as u32;
    let command = status["pid"].as_u64().expect("command pid") as u32;

    force_kill(shim);
    wait_until("the shim to die", CEILING, || !pid_alive(shim)).await;
    let waited = sandbox
        .kelir(NO_PROVIDER, &["bg", "wait", &id, "--timeout", "4"], &[])
        .await;
    let listed = sandbox.kelir(NO_PROVIDER, &["bg", "ls"], &[]).await;
    // Clean up whatever survived before asserting.
    force_kill(command);
    assert_eq!(
        waited.code, 0,
        "bg wait on a job whose shim is dead: code {} stderr {:?}; bg ls says {:?}",
        waited.code, waited.stderr, listed.stdout
    );
}

/// R4: two cancels, a cancel after the job finished, and an unknown id.
#[tokio::test]
async fn r4_cancel_twice_cancel_after_finish_and_unknown_id() {
    let sandbox = Sandbox::new("");
    let started = sandbox
        .kelir(NO_PROVIDER, &["bg", "run", "sleep 20"], &[])
        .await;
    let id = id_of(&started);
    let first = sandbox
        .kelir(NO_PROVIDER, &["bg", "cancel", &id], &[])
        .await;
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(first.stdout.contains("cancelled"), "{}", first.stdout);
    let second = sandbox
        .kelir(NO_PROVIDER, &["bg", "cancel", &id], &[])
        .await;
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert!(
        second.stdout.contains("nothing to cancel"),
        "{}",
        second.stdout
    );
    assert_eq!(status_of(&sandbox, &id)["state"], "cancelled");

    let quick = sandbox
        .kelir(NO_PROVIDER, &["bg", "run", "true"], &[])
        .await;
    let quick_id = id_of(&quick);
    let waited = sandbox
        .kelir(
            NO_PROVIDER,
            &["bg", "wait", &quick_id, "--timeout", "20"],
            &[],
        )
        .await;
    assert_eq!(waited.code, 0, "{}", waited.stderr);
    let late = sandbox
        .kelir(NO_PROVIDER, &["bg", "cancel", &quick_id], &[])
        .await;
    assert_eq!(late.code, 0, "{}", late.stderr);
    assert_eq!(
        status_of(&sandbox, &quick_id)["state"],
        "exited",
        "a cancel after the job finished rewrote its status"
    );
    let unknown = sandbox
        .kelir(NO_PROVIDER, &["bg", "cancel", "bg_nope"], &[])
        .await;
    assert_eq!(unknown.code, 2, "{}", unknown.stderr);
}

/// R5 (F3b): `cancel` trusts the pid in the status file. A status that outlived
/// its shim (a reboot) names a pid some other program owns now; cancelling it
/// must not kill that program.
#[tokio::test]
async fn r5_cancel_does_not_kill_an_unrelated_process_with_a_recycled_pid() {
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
    let bg = sandbox.state.join("bg");
    std::fs::create_dir_all(&bg).unwrap();
    let stale = json!({
        "id": "bg_stale", "state": "running", "pid": pid, "shim": pid,
        "started": "2026-01-01T00:00:00Z", "cmd": "long gone",
        "out": bg.join("bg_stale.out").to_str().unwrap().replace('\\', "/"),
        "kind": "job"
    });
    std::fs::write(bg.join("bg_stale.status"), stale.to_string()).unwrap();
    let cancelled = sandbox
        .kelir(NO_PROVIDER, &["bg", "cancel", "bg_stale"], &[])
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let alive = victim.try_wait().unwrap().is_none();
    let _ = victim.kill();
    assert!(
        alive,
        "bg cancel killed pid {pid}, which has nothing to do with the job (stdout {:?})",
        cancelled.stdout
    );
}

/// R6: a hundred quick jobs, started concurrently: all settle, none is left
/// `running`, every id is unique, no temp file is left behind.
#[tokio::test]
async fn r6_a_hundred_quick_jobs() {
    let sandbox = std::sync::Arc::new(Sandbox::new(""));
    let mut handles = Vec::new();
    for batch in 0..5 {
        let mut group = Vec::new();
        for n in 0..20 {
            let sandbox = sandbox.clone();
            group.push(tokio::spawn(async move {
                let output = sandbox
                    .kelir(
                        NO_PROVIDER,
                        &["bg", "run", &format!("echo job-{batch}-{n}")],
                        &[],
                    )
                    .await;
                assert_eq!(output.code, 0, "{}", output.stderr);
                id_of(&output)
            }));
        }
        for handle in group {
            handles.push(handle.await.unwrap());
        }
    }
    let mut unique = handles.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 100, "ids collided");
    let bg = sandbox.state.join("bg");
    wait_until("every job to settle", Duration::from_secs(60), || {
        handles.iter().all(|id| {
            std::fs::read_to_string(bg.join(format!("{id}.status")))
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .map(|status| status["state"] == "exited" && status["exit_code"] == 0)
                .unwrap_or(false)
        })
    })
    .await;
    let leftovers: Vec<String> = std::fs::read_dir(&bg)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let listed = sandbox.kelir(NO_PROVIDER, &["bg", "ls"], &[]).await;
    assert_eq!(listed.stdout.lines().count(), 100, "{}", listed.stdout);
}

/// R7: a job writing a lot: the status settles, the output is complete, and
/// `bg out --follow` on the finished file stays quick.
#[tokio::test]
async fn r7_a_job_writing_a_lot_of_output() {
    let sandbox = Sandbox::new("");
    let started = sandbox
        .kelir(
            NO_PROVIDER,
            &[
                "bg",
                "run",
                "yes abcdefghijklmnopqrstuvwxyz | head -c 120000000",
            ],
            &[],
        )
        .await;
    let id = id_of(&started);
    let waited = sandbox
        .kelir(NO_PROVIDER, &["bg", "wait", &id, "--timeout", "120"], &[])
        .await;
    assert_eq!(waited.code, 0, "{}", waited.stderr);
    let out_file = sandbox.state.join("bg").join(format!("{id}.out"));
    assert_eq!(bytes_len(&out_file), 120_000_000);
    let began = Instant::now();
    let mut follow = tokio::process::Command::new(env!("CARGO_BIN_EXE_kelir"));
    // Never the session that runs the suite: the helper removes every
    // `KELIR_*` (M5-A fixes N2), before this test's own environment goes on.
    crate::support::scrub_tokio(&mut follow);
    follow
        .args(["bg", "out", &id, "--follow"])
        .env("KELIR_STATE_DIR", &sandbox.state)
        .env("KELIR_CONFIG", &sandbox.config)
        .stdout(std::process::Stdio::null());
    let followed = follow.status().await.unwrap();
    assert!(followed.success());
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "bg out --follow of a finished 120 MB job took {:?}",
        began.elapsed()
    );
}

/// R8: a command that cannot run, and a shell that does not exist.
#[tokio::test]
async fn r8_invalid_command_and_missing_shell() {
    let sandbox = Sandbox::new("");
    let started = sandbox
        .kelir(
            NO_PROVIDER,
            &["bg", "run", "definitely_not_a_command_zz"],
            &[],
        )
        .await;
    assert_eq!(started.code, 0, "{}", started.stderr);
    let id = id_of(&started);
    let waited = sandbox
        .kelir(NO_PROVIDER, &["bg", "wait", &id, "--timeout", "20"], &[])
        .await;
    assert_eq!(waited.code, 0, "{}", waited.stderr);
    assert_eq!(status_of(&sandbox, &id)["exit_code"], 127);

    let broken = Sandbox::new("shell = [\"C:/definitely/not/here/bash.exe\", \"-c\"]\n");
    let started = broken.kelir(NO_PROVIDER, &["bg", "run", "true"], &[]).await;
    assert_ne!(started.code, 0, "a job with no shell reported success");
    assert!(
        started.stdout.contains("error:") || started.stderr.contains("shell"),
        "{} / {}",
        started.stdout,
        started.stderr
    );

    // The same through a real bash call: the model must be told it did not run.
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_x",
            "bash",
            &json!({"command": "echo hi"}),
        ))),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    let run = broken
        .kelir(&mock.base_url(), &["run", "--json", "go"], &[])
        .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let settled = run
        .json_lines()
        .into_iter()
        .find(|event| event["type"] == "call_settled")
        .expect("call_settled");
    assert_eq!(settled["outcome"], "error", "{settled}");
    assert!(
        settled["result"]
            .as_str()
            .unwrap_or("")
            .contains("did not run"),
        "{settled}"
    );
}

/// R9: quoting. Every command goes through two Windows command lines (session
/// -> shim -> shell); each must come out byte for byte.
#[tokio::test]
async fn r9_shell_quoting_survives_the_shim() {
    let cases: Vec<(&str, String)> = vec![
        (r#"printf '%s' "he said \"hi\"""#, r#"he said "hi""#.into()),
        (r"echo hi\\", String::from("hi\\\n")),
        (
            "printf '%s' 'h\u{e9}llo \u{2713} \u{65e5}\u{672c}\u{8a9e}'",
            "h\u{e9}llo \u{2713} \u{65e5}\u{672c}\u{8a9e}".into(),
        ),
        (
            "cat <<'EOF'\nline1\nline \"two\" 'three'\nEOF",
            "line1\nline \"two\" 'three'\n".into(),
        ),
        ("printf '%s' '%PATH% ^& | > <'", "%PATH% ^& | > <".into()),
        ("printf '%s' \"$((6*7))\"", "42".into()),
    ];
    for (command, expected) in cases {
        let mock = Mock::start(vec![
            Reply::Sse(sse(&tool_turn(
                "call_q",
                "bash",
                &json!({"command": command}),
            ))),
            Reply::Sse(sse(&text_turn("ok", 0))),
        ])
        .await;
        let sandbox = Sandbox::new("");
        let run = sandbox
            .kelir(&mock.base_url(), &["run", "--json", "go"], &[])
            .await;
        assert_eq!(run.code, 0, "{command:?}: {}", run.stderr);
        assert_eq!(first_command_out(&sandbox), expected, "command {command:?}");
    }
}

/// R10 (F4): a command near the Windows command-line ceiling (32,767 UTF-16
/// units) ran in M0..M3 because `bash.exe -c <command>` is short. The shim's
/// command line repeats the command plus six paths and flags.
#[tokio::test]
async fn r10_a_very_long_command_still_runs() {
    let payload = "x".repeat(32_000);
    let command = format!("echo {payload} | wc -c");
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_l",
            "bash",
            &json!({"command": command}),
        ))),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "go"], &[])
        .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let settled = run
        .json_lines()
        .into_iter()
        .find(|event| event["type"] == "call_settled")
        .expect("call_settled");
    assert_eq!(settled["outcome"], "ok", "{settled}");
}

/// R11 (F5): after a session is killed, a child that a *finished* call left
/// running (`cmd &`) must not survive. Through M3 the Windows Job Object was
/// kill-on-close, so killing the session killed it; M4 made the job not
/// kill-on-close and the shim is already gone.
#[tokio::test]
async fn r11_a_leftover_background_child_dies_with_a_killed_session() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_left",
            "bash",
            &json!({"command": "( for i in $(seq 1 60); do echo tick >> left.txt; sleep 0.2; done ) > /dev/null 2>&1 &"}),
        ))),
        Reply::Sse(sse(&text_turn("left it running", 0))),
    ])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    live.wait_for("turn_ended").await;
    let ticks = live.cwd.join("left.txt");
    wait_until("the leftover to write", CEILING, || bytes_len(&ticks) > 0).await;
    live.kill().await;
    // The leftover may be mid-write when the kill lands, and on a loaded box
    // the tree takes a while to go (STEER-9): `first` is taken once the file has
    // stopped moving, and the window after it is what proves nothing writes.
    wait_until("the file to stop growing", CEILING, || {
        let before = bytes_len(&ticks);
        std::thread::sleep(Duration::from_millis(500));
        before == bytes_len(&ticks)
    })
    .await;
    let first = bytes_len(&ticks);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let second = bytes_len(&ticks);
    // (the loop is bounded to 60 iterations, so a survivor cleans itself up)
    assert!(
        second == first,
        "a leftover `&` child kept writing after its session was killed ({first} -> {second} bytes)"
    );
}

/// R12: `ctl end` (a graceful exit) leaves a promoted job running and writing.
#[tokio::test]
async fn r12_a_graceful_exit_leaves_a_promoted_job_running() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_g",
            "bash",
            &json!({"command": "for i in $(seq 1 100); do echo tick >> job.txt; sleep 0.2; done"}),
        ))),
        Reply::Sse(sse(&text_turn("promoted, leaving", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "promote_after = 1\n",
    )
    .await;
    let settled = live.wait_for("call_settled").await;
    assert_eq!(settled["outcome"], "promoted", "{settled}");
    live.wait_for("turn_ended").await;
    let out_path = settled["out_path"].as_str().unwrap_or("").to_string();
    let id = Path::new(&out_path)
        .parent()
        .and_then(|dir| dir.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap();
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(live.session_dir.join("bg").join(format!("{id}.status"))).unwrap(),
    )
    .unwrap();
    let pid = status["pid"].as_u64().expect("job pid") as u32;
    let shim = status["shim"].as_u64().expect("shim pid") as u32;
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let (command_alive, shim_alive) = (pid_alive(pid), pid_alive(shim));
    force_kill(pid);
    force_kill(shim);
    assert!(
        command_alive && shim_alive,
        "the promoted job stopped when the session exited gracefully (command {command_alive}, shim {shim_alive})"
    );
}

/// R13 (F6): a batch of two parallel calls where the first is promoted and its
/// job settles before the batch's results are written. The wake message is a
/// `user` message; written between two `tool` results it makes the next request
/// invalid for any provider that wants a call's results contiguous. Try a range
/// of durations around the promotion edge and check every request.
#[tokio::test]
async fn r13_a_wake_never_lands_between_the_results_of_one_batch() {
    let mut worst: Option<String> = None;
    for millis in [
        1000u64, 1010, 1020, 1030, 1040, 1050, 1060, 1080, 1100, 1130,
    ] {
        let command = format!("sleep {}.{:03}", millis / 1000, millis % 1000);
        let mock = Mock::start(vec![
            Reply::Sse(sse(&two_tool_turn(
                ("call_a", "bash", json!({"command": command})),
                ("call_b", "bash", json!({"command": "echo second"})),
            ))),
            Reply::Sse(sse(&text_turn("done", 0))),
        ])
        .await;
        let sandbox = Sandbox::new("promote_after = 1\n");
        let run = sandbox
            .kelir(&mock.base_url(), &["run", "--json", "go"], &[])
            .await;
        assert_eq!(run.code, 0, "{}", run.stderr);
        let requests = mock.requests();
        let last = requests.last().expect("a second request");
        let roles = last.roles();
        // After the assistant's tool_calls message, the next two must be tool.
        if let Some(position) = roles.iter().position(|role| *role == "assistant") {
            let next: Vec<&str> = roles.iter().skip(position + 1).take(2).copied().collect();
            if next != ["tool", "tool"] {
                worst = Some(format!("sleep {millis} ms: roles {roles:?}"));
                break;
            }
        }
    }
    assert!(worst.is_none(), "{}", worst.unwrap_or_default());
}

/// R14 (scenario 5 in a test): `kelir run --json | consumer` must see EOF when
/// kelir exits, even with a `cmd &` left behind.
#[tokio::test]
async fn r14_run_json_does_not_hold_the_pipe_open_for_a_background_child() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_amp",
            "bash",
            &json!({"command": "sleep 12 &\necho started"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("");
    let began = Instant::now();
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "go"], &[])
        .await;
    let took = began.elapsed();
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        took < Duration::from_secs(6),
        "stdout stayed open {took:?} for a `sleep 12 &` (pipe held by the background child)"
    );
}

/// R15: stale `.promote` marker / torn status / garbage in bg/: a session and
/// the CLI keep working.
#[tokio::test]
async fn r15_garbage_in_the_bg_directory_is_survivable() {
    let sandbox = Sandbox::new("");
    let bg = sandbox.state.join("bg");
    std::fs::create_dir_all(&bg).unwrap();
    std::fs::write(bg.join("torn.status"), "{\"id\":\"torn\",\"sta").unwrap();
    std::fs::write(bg.join("empty.status"), "").unwrap();
    std::fs::write(bg.join("binary.status"), [0xffu8, 0xfe, 0x00, 0x01]).unwrap();
    std::fs::write(bg.join("wrongtype.status"), "[1,2,3]").unwrap();
    std::fs::write(bg.join("nulls.status"), "{\"id\":null,\"state\":5}").unwrap();
    let listed = sandbox.kelir(NO_PROVIDER, &["bg", "ls"], &[]).await;
    assert_eq!(listed.code, 0, "{}", listed.stderr);
    for id in [
        "torn",
        "empty",
        "binary",
        "wrongtype",
        "nulls",
        "../escape",
        "",
    ] {
        let out = sandbox.kelir(NO_PROVIDER, &["bg", "out", id], &[]).await;
        assert!(
            out.code == 2 || out.code == 0,
            "bg out {id:?}: code {} {}",
            out.code,
            out.stderr
        );
    }
    let started = sandbox
        .kelir(NO_PROVIDER, &["bg", "run", "echo ok"], &[])
        .await;
    assert_eq!(started.code, 0, "{}", started.stderr);
}

/// R16: a status file that is torn (or deleted) while its job runs does not hurt
/// the session: the shim rewrites the whole file when the job exits, and the
/// wake still arrives.
#[tokio::test]
async fn r16_a_torn_or_deleted_status_mid_job_does_not_break_the_session() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_t",
            "bash",
            &json!({"command": "sleep 4; echo survived"}),
        ))),
        Reply::Sse(sse(&text_turn("waiting", 0))),
        Reply::Sse(sse(&text_turn("saw it finish", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "promote_after = 1\n",
    )
    .await;
    let settled = live.wait_for("call_settled").await;
    assert_eq!(settled["outcome"], "promoted", "{settled}");
    live.wait_for("turn_ended").await;
    let out_path = settled["out_path"].as_str().unwrap_or("").to_string();
    let id = Path::new(&out_path)
        .parent()
        .and_then(|dir| dir.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap();
    let status_file = live.session_dir.join("bg").join(format!("{id}.status"));
    // Torn, then (a moment later) gone: the watcher polls every 250 ms.
    std::fs::write(&status_file, "{\"id\":\"").unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    std::fs::remove_file(&status_file).unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    // The job finishes on its own and writes the file again.
    let wake = live.wait_for("call_settled").await;
    assert_eq!(wake["exit_code"], 0, "{wake}");
    let user = live.wait_for("user").await;
    assert_eq!(user["from"], "job", "{user}");
    live.wait_for("turn_ended").await;
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// R17: per-call overhead of running every call under the shim. Not an
/// assertion about speed (there is no budget in the spec), it prints the mean
#[tokio::test]
async fn r17_trivial_call_latency() {
    let mut script = Vec::new();
    for n in 0..20 {
        script.push(Reply::Sse(sse(&tool_turn(
            &format!("call_{n}"),
            "bash",
            &json!({"command": "true"}),
        ))));
    }
    script.push(Reply::Sse(sse(&text_turn("done", 0))));
    let mock = Mock::start(script).await;
    let sandbox = Sandbox::new("");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "go"], &[])
        .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let events = run.json_lines();
    let parse = |value: &Value| {
        time::OffsetDateTime::parse(
            value["ts"].as_str().unwrap_or(""),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap()
    };
    let mut total = Duration::ZERO;
    let mut count = 0u32;
    for started in events
        .iter()
        .filter(|event| event["type"] == "call_started")
    {
        let settled = events
            .iter()
            .find(|event| event["type"] == "call_settled" && event["id"] == started["id"])
            .expect("settled");
        total += Duration::from_secs_f64((parse(settled) - parse(started)).as_seconds_f64());
        count += 1;
    }
    eprintln!(
        "R17: {count} calls, mean tool_started->tool_settled {:?}",
        total / count.max(1)
    );
    assert!(total / count.max(1) < Duration::from_secs(2));
}

/// Pids of processes whose command line contains `marker` (the test's own
/// process and the lister are excluded by construction: the marker is built
/// from parts, so the lister's command line does not contain it whole).
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

fn reap(marker: &str) {
    for pid in pids_with(marker) {
        force_kill(pid);
    }
}

/// R18 (F7): the tree of an unpromoted call must die with a killed session even
/// when it holds *exec'd* grandchildren (`sleep &`, a nested `bash -c`), not just
/// fork-only subshells. On Windows with Git's msys runtime these children ask to
/// break away from the call's Job Object, and M4 gave that job
/// `JOB_OBJECT_LIMIT_BREAKAWAY_OK`.
#[tokio::test]
async fn r18_a_killed_session_takes_exec_grandchildren_with_it() {
    let mock = Mock::start(vec![Reply::Sse(sse(&tool_turn(
        "call_gc",
        "bash",
        &json!({"command": "bash -c 'sleep 7311 & sleep 7312 & wait' & sleep 7313 & wait"}),
    )))])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    live.wait_for("call_started").await;
    wait_until("the grandchildren to exist", CEILING, || {
        pids_with("7311").len() + pids_with("7312").len() + pids_with("7313").len() >= 3
    })
    .await;
    live.kill().await;
    settle_to_zero(|| {
        ["7311", "7312", "7313"]
            .iter()
            .map(|marker| pids_with(marker).len())
            .sum()
    })
    .await;
    let left: Vec<u32> = ["7311", "7312", "7313"]
        .iter()
        .flat_map(|marker| pids_with(marker))
        .collect();
    for marker in ["7311", "7312", "7313"] {
        reap(marker);
    }
    assert!(
        left.is_empty(),
        "processes of the killed session's call survived: {left:?}"
    );
}

/// R19 (F7): `bg cancel` must take exec'd grandchildren down too.
#[tokio::test]
async fn r19_cancel_takes_exec_grandchildren_with_it() {
    let sandbox = Sandbox::new("");
    let started = sandbox
        .kelir(
            NO_PROVIDER,
            &[
                "bg",
                "run",
                "bash -c 'sleep 7321 & sleep 7322 & wait' & sleep 7323 & wait",
            ],
            &[],
        )
        .await;
    assert_eq!(started.code, 0, "{}", started.stderr);
    let id = id_of(&started);
    wait_until("the grandchildren to exist", CEILING, || {
        pids_with("7321").len() + pids_with("7322").len() + pids_with("7323").len() >= 3
    })
    .await;
    let cancelled = sandbox
        .kelir(NO_PROVIDER, &["bg", "cancel", &id], &[])
        .await;
    assert_eq!(cancelled.code, 0, "{}", cancelled.stderr);
    settle_to_zero(|| {
        ["7321", "7322", "7323"]
            .iter()
            .map(|marker| pids_with(marker).len())
            .sum()
    })
    .await;
    let left: Vec<u32> = ["7321", "7322", "7323"]
        .iter()
        .flat_map(|marker| pids_with(marker))
        .collect();
    for marker in ["7321", "7322", "7323"] {
        reap(marker);
    }
    assert!(
        left.is_empty(),
        "bg cancel left processes of the job running: {left:?}"
    );
}
