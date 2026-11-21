//! M11 end-to-end: `kelir attach` replaying a finished session and `[notify]`
//! hooks firing on turn completion. Every process lives in a TreeGuard, so a
//! failure cannot leave a `kelir` behind.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::support::*;
use serde_json::Value;

/// Run `kelir attach` against a finished session's directories. The child is
/// adopted by a fresh TreeGuard: a panicking test cannot leak the viewer.
async fn attach(run: &Run, base_url: &str, args: &[&str]) -> (i32, String, String) {
    let state = run
        .session_dir
        .parent()
        .and_then(Path::parent)
        .expect("state/sessions/<id>");
    let config = state.parent().unwrap().join("config.toml");
    let guard = TreeGuard::new("m11-attach");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_kelir"));
    crate::support::scrub_session_env(&mut command);
    command
        .args(["attach"])
        .args(args)
        .current_dir(&run.cwd)
        .env("KELIR_CONFIG", &config)
        .env("KELIR_CONFIG_DIR", state.join("no-config"))
        .env("KELIR_STATE_DIR", state)
        .env("KELIR_BASE_URL", base_url)
        .env("KELIR_HOME", state.join("home"))
        .env("OPENROUTER_API_KEY", "test-openrouter-key")
        .env("KELIR_GO_API_KEY", "test-go-key")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = guard.spawn(&mut command).await.expect("spawn attach");
    let out = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("attach did not finish in 30 s")
        .expect("wait for attach");
    let code = out.status.code().unwrap_or(-1);
    (
        code,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn snapshot(dir: &Path) -> Vec<(String, u64)> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().to_string(),
                entry.metadata().map(|m| m.len()).unwrap_or(0),
            )
        })
        .collect();
    files.sort();
    files
}

/// Attach to a finished session replaying its events (no follow): the output
/// carries the turn's text and the viewer writes nothing into the session dir.
#[tokio::test(flavor = "multi_thread")]
async fn attach_replays_a_finished_session_without_writing_to_it() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("hello from the log", 0)))]).await;
    let run = run_task(&mock, &["run", "--json", "say hi"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let id = run
        .session_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let before = snapshot(&run.session_dir);
    let (code, stdout, stderr) = attach(&run, &mock.base_url(), &[&id, "--plain"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("hello from the log"),
        "the viewer replays the assistant text: {stdout}"
    );
    assert_eq!(
        snapshot(&run.session_dir),
        before,
        "attach wrote to the log dir"
    );
}

/// `--raw` prints the original NDJSON events, one per line, and nothing else.
#[tokio::test(flavor = "multi_thread")]
async fn attach_raw_prints_the_original_ndjson() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("raw replay", 0)))]).await;
    let run = run_task(&mock, &["run", "--json", "say hi"], &[], None, "").await;
    assert_eq!(run.code, 0);
    let id = run
        .session_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let (code, stdout, stderr) = attach(&run, &mock.base_url(), &[&id, "--raw"]).await;
    assert_eq!(code, 0, "stderr: {stderr}");
    let lines: Vec<Value> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a raw NDJSON line"))
        .collect();
    assert!(!lines.is_empty(), "the viewer printed events");
    assert!(
        lines.iter().all(|line| line.get("seq").is_some()),
        "every line is a log event"
    );
}

/// A missing session is a usage error, not a panic or a silent empty view.
#[tokio::test(flavor = "multi_thread")]
async fn attach_to_an_unknown_session_fails_cleanly() {
    let mock = Mock::start(vec![]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = cwd.join("config.toml");
    std::fs::write(&config, test_config("")).unwrap();
    let guard = TreeGuard::new("m11-attach-missing");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_kelir"));
    crate::support::scrub_session_env(&mut command);
    command
        .args(["attach", "no-such-session-xyz"])
        .current_dir(&cwd)
        .env("KELIR_CONFIG", &config)
        .env("KELIR_CONFIG_DIR", temp.path().join("no-config"))
        .env("KELIR_STATE_DIR", temp.path().join("state"))
        .env("KELIR_HOME", temp.path().join("home"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = guard.spawn(&mut command).await.expect("spawn attach");
    let out = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .expect("attach did not finish")
        .expect("wait");
    let _ = mock;
    let _ = mock;
    assert_ne!(out.status.code(), Some(0));
}

/// A `[notify]` hook on `turn_done` runs once, gets the scrubbed event env,
/// and never changes the turn's outcome. A marker file is the observable.
#[tokio::test(flavor = "multi_thread")]
async fn notify_hook_fires_once_on_turn_done() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("hook-fired.txt");
    let config = format!(
        "shell = '{}'\n[notify]\nturn_done = '{}'\n",
        test_shell(),
        hook_touch_command(&marker)
    );
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("done", 0)))]).await;
    let run = run_task(&mock, &["run", "--json", "say hi"], &[], None, &config).await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    // Hooks are detached: wait briefly for the marker to appear.
    let started = Instant::now();
    while !marker.exists() && started.elapsed() < Duration::from_secs(20) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        marker.exists(),
        "the notify hook ran and touched its marker"
    );
    let body = std::fs::read_to_string(&marker).unwrap();
    assert!(
        body.contains("turn_done"),
        "the hook saw KELIR_NOTIFY_EVENT (body={body:?}): {body}"
    );
    let events = run.events_of("notify");
    assert_eq!(events.len(), 1, "one notify event in the log");
    assert_eq!(events[0]["event"], "turn_done");
    assert_eq!(events[0]["hooks"], 1);
}

/// A command that writes the event name to `marker`, using only the configured
/// shell and the scrubbed `KELIR_NOTIFY_EVENT`.
fn hook_touch_command(marker: &Path) -> String {
    let path = marker.to_string_lossy().replace('\\', "/");
    format!("printf %s \"$KELIR_NOTIFY_EVENT\" > {path}")
}

/// The shell the test asks for, so the command above is portable.
fn test_shell() -> &'static str {
    "bash"
}
