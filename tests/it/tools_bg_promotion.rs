//! tools bg promotion tests: r12_a_graceful_exit_leaves_a_p, r14_run_json_does_not_hold_the, r15_garbage_in_the_bg_director, r16_a_torn_or_deleted_status_m.
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::Path;
use std::time::{Duration, Instant};

use crate::support::*;
use serde_json::{Value, json};

use super::tools_bg::*;

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
        &std::fs::read_to_string(job_status_path(&live.session_dir, &id)).unwrap(),
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
    let bg = global_job_root(&sandbox.state);
    // One job dir per fixture, with the nested path the real reader opens. Keeping the
    // fixtures flat would let `list` skip them and this test prove nothing.
    for (id, bytes) in [
        ("torn", &b"{\"id\":\"torn\",\"sta"[..]),
        ("empty", &b""[..]),
        ("binary", &[0xffu8, 0xfe, 0x00, 0x01][..]),
        ("wrongtype", &b"[1,2,3]"[..]),
        ("nulls", &b"{\"id\":null,\"state\":5}"[..]),
    ] {
        let path = kelir::bg::status_path(&bg, id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    // Each malformed file must sit at the path the reader opens: a flat fixture would be
    // skipped without the parser ever seeing it, and this test would prove nothing.
    for id in ["torn", "empty", "binary", "wrongtype", "nulls"] {
        assert!(
            kelir::bg::status_path(&bg, id).is_file(),
            "the {id} fixture is not at a path the reader opens"
        );
    }
    // One well-formed job beside the garbage: `list` must see exactly that one.
    let good = kelir::bg::Status::new(
        "bg_good",
        kelir::bg::KIND_JOB,
        "echo ok",
        &kelir::bg::out_path(&bg, "bg_good"),
    );
    let good_path = kelir::bg::status_path(&bg, "bg_good");
    std::fs::create_dir_all(good_path.parent().unwrap()).unwrap();
    good.write(&good_path).unwrap();
    assert_eq!(
        kelir::bg::list(&bg)
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        vec!["bg_good"],
        "the malformed fixtures were not at paths the reader opens"
    );
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
    let status_file = job_status_path(&live.session_dir, &id);
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
