//! Fresh-reviewer tests for M11 (branch job/m11rev). Product code is untouched.
//!
//! Every spawned process lives in its own TreeGuard and the suite has a hard
//! cap plus a watchdog: see `watchdog` below. No test kills a process by image
//! name and no test signals a process group it shares with the harness.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;
use support::*;

/// Hard cap on live reviewer children at any one time.
static LIVE: AtomicU32 = AtomicU32::new(0);
const MAX_LIVE: u32 = 6;

/// Take a slot or panic before spawning anything.
fn slot() -> Slot {
    let n = LIVE.fetch_add(1, Ordering::SeqCst);
    assert!(n < MAX_LIVE, "process cap reached ({MAX_LIVE})");
    Slot
}

struct Slot;

impl Drop for Slot {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A watchdog the human running this suite can leave in another terminal.
/// Prints a line every 20 s and aborts the test process after 6 minutes if a
/// slot is still held: no unbounded spawns, no orphans.
fn watchdog() -> std::thread::JoinHandle<()> {
    std::thread::spawn(|| {
        let start = Instant::now();
        loop {
            std::thread::sleep(Duration::from_secs(20));
            let n = LIVE.load(Ordering::SeqCst);
            if n > 0 {
                eprintln!(
                    "review_m11: watchdog: {n} child slot(s) held for {}s",
                    start.elapsed().as_secs()
                );
            }
            if start.elapsed() > Duration::from_secs(360) {
                if n > 0 {
                    eprintln!("review_m11: watchdog: aborting with {n} live children");
                    std::process::abort();
                }
                return;
            }
        }
    })
}

static ONCE: std::sync::Once = std::sync::Once::new();

fn init() {
    ONCE.call_once(|| {
        let _ = watchdog();
    });
}

// -------------------------------------------------------------- attach helpers

struct Attach {
    code: i32,
    stdout: String,
    stderr: String,
    _slot: Slot,
}

/// Run `kelir attach` with the same environment the M11 e2e uses, but let the
/// caller add flags and optionally feed stdin. The child is adopted by a
/// TreeGuard; even a panic cannot leak it.
async fn attach_in(
    state: &Path,
    cwd: &Path,
    session: &str,
    args: &[&str],
    stdin: Option<&str>,
) -> Attach {
    init();
    let slot = slot();
    let config = state.parent().unwrap().join("config.toml");
    let guard = TreeGuard::new("review-m11-attach");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_kelir"));
    support::scrub_session_env(&mut command);
    command
        .args(["attach"])
        .arg(session)
        .args(args)
        .current_dir(cwd)
        .env("KELIR_CONFIG", &config)
        .env("KELIR_CONFIG_DIR", state.join("no-config"))
        .env("KELIR_STATE_DIR", state)
        .env("KELIR_HOME", state.join("home"))
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = guard.spawn(&mut command).await.expect("spawn attach");
    if let Some(input) = stdin {
        use tokio::io::AsyncWriteExt as _;
        let mut handle = child.stdin.take().unwrap();
        handle.write_all(input.as_bytes()).await.unwrap();
        drop(handle);
    }
    let out = tokio::time::timeout(Duration::from_secs(25), child.wait_with_output())
        .await
        .expect("attach timed out")
        .expect("wait for attach");
    Attach {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        _slot: slot,
    }
}

/// Build a finished session from a synthetic log (no processes needed). The
/// state layout is what `attach` resolves: `<state>/sessions/<id>/events.jsonl`.
fn synthetic_session(state: &Path, id: &str, lines: &[String]) -> PathBuf {
    let dir = state.join("sessions").join(id);
    fs::create_dir_all(&dir).unwrap();
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    fs::write(dir.join("events.jsonl"), text).unwrap();
    let config = state.parent().unwrap().join("config.toml");
    fs::write(&config, test_config("")).unwrap();
    dir
}

fn event_line(seq: u64, kind: &str, text: &str) -> String {
    let body = match kind {
        "user" => json!({"text": text,
                          "from": "cli", "delivery": "now", "attachments": []}),
        "session_ended" => json!({"reason": text}),
        "assistant" => json!({"step": 1, "text": text, "reasoning": null,
                               "tool_calls": [], "finish_reason": "stop"}),
        "turn_ended" => json!({"outcome": text,
              "final_text": "the answer"}),
        _ => json!({"text": text}),
    };
    let mut line = body;
    line["type"] = json!(kind);
    line["seq"] = json!(seq);
    line["ts"] = json!("2026-11-21T00:00:00Z");
    line.to_string()
}

fn temp_state() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let cwd = temp.path().join("work");
    fs::create_dir_all(&cwd).unwrap();
    (temp, state, cwd)
}

// ------------------------------------------------------------------- findings

/// S1: `--since N` skips the last *complete* line when the log has no trailing
/// newline (the normal case for a live log: the writer appends "\n"+event, so
/// the final bytes are a newline? No: a finished log ends with a newline from
/// `writeln!`; a torn append is the other case). This test uses the log shape
/// the real writer produces and checks the documented "last N events".
#[tokio::test(flavor = "multi_thread")]
async fn r1_since_returns_the_documented_last_n_events() {
    let (_temp, state, cwd) = temp_state();
    let lines: Vec<String> = (1..=6)
        .map(|i| event_line(i, "user", &format!("message-{i}")))
        .collect();
    synthetic_session(&state, "ses_since", &lines);
    let a = attach_in(
        &state,
        &cwd,
        "ses_since",
        &["--since", "2", "--plain"],
        None,
    )
    .await;
    assert_eq!(a.code, 0, "stderr: {}", a.stderr);
    for i in 1..=4 {
        assert!(
            !a.stdout.contains(&format!("message-{i}")),
            "since=2 showed message-{i}: {}",
            a.stdout
        );
    }
    assert!(
        a.stdout.contains("message-5"),
        "since=2 must show message-5: {}",
        a.stdout
    );
    assert!(
        a.stdout.contains("message-6"),
        "since=2 must show message-6: {}",
        a.stdout
    );
}

/// S2: a session still running (a live `owner` file naming this test process)
/// is followed until `session_ended` arrives; the viewer must not exit early.
/// Here we only require the finished-session replay to include the final
/// outcome and to print a status line in the styled (non-plain) mode.
#[tokio::test(flavor = "multi_thread")]
async fn r2_finished_replay_shows_outcome_and_status() {
    let (_temp, state, cwd) = temp_state();
    let lines = vec![
        event_line(1, "user", "the question"),
        event_line(2, "assistant", "the answer"),
        event_line(3, "turn_ended", "done"),
        event_line(4, "session_ended", "done"),
    ];
    let dir = synthetic_session(&state, "ses_final", &lines);
    let before: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let a = attach_in(&state, &cwd, "ses_final", &[], None).await;
    assert_eq!(a.code, 0, "stderr: {}", a.stderr);
    assert!(
        a.stdout.contains("the answer"),
        "outcome line missing: {}",
        a.stdout
    );
    let after: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(before, after, "attach modified the session dir");
}

/// S3: `--steer` refuses anything but say/stop, before any socket access.
#[tokio::test(flavor = "multi_thread")]
async fn r3_steer_refuses_other_verbs_and_empty_say() {
    let (_temp, state, cwd) = temp_state();
    synthetic_session(
        &state,
        "ses_steer",
        &[
            event_line(1, "user", "hello"),
            event_line(2, "session_ended", "done"),
        ],
    );
    let mut err = String::new();
    for command in ["end", "observe", "say", "say hello from the viewer"] {
        let a = attach_in(
            &state,
            &cwd,
            "ses_steer",
            &["--steer", "--plain", "--follow"],
            Some(&format!("{command}\n")),
        )
        .await;
        err.push_str(&a.stderr);
    }
    assert!(
        err.contains("only `say <text>` and `stop`"),
        "no refusal: {err}"
    );
    for name in ["end", "observe"] {
        assert!(
            err.contains(&format!("got `{name}`")),
            "{name} not refused: {err}"
        );
    }
    assert!(
        err.contains("`say` needs text"),
        "empty say not refused: {err}"
    );
    assert!(
        !err.contains("say hello from the viewer"),
        "valid say refused: {err}"
    );
}

/// S4: attach is read-only even with --follow and --steer against a live
/// session: no writes to the session directory from the viewer itself.
#[tokio::test(flavor = "multi_thread")]
async fn r4_a_viewer_disconnect_never_stops_the_session() {
    init();
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn(
        "long live the session",
        0,
    )))])
    .await;
    let live = LiveSession::start(&mock, &["run", "--json", "--keep", "say watch me"], "").await;
    let _slot = slot();
    let session = live.id.clone();
    let state = live.state.clone();
    let cwd = live.cwd.clone();
    // A follow viewer with no log events appended yet: it must sit and wait;
    // dropping it after a short while must leave the session's log intact.
    let guard = TreeGuard::new("review-m11-follow");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_kelir"));
    support::scrub_session_env(&mut command);
    command
        .args(["attach", &session, "--plain", "--follow"])
        .current_dir(&cwd)
        .env("KELIR_CONFIG", &live.config)
        .env("KELIR_CONFIG_DIR", state.join("no-config"))
        .env("KELIR_STATE_DIR", &state)
        .env("KELIR_HOME", state.join("home"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut viewer = guard.spawn(&mut command).await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        viewer.try_wait().unwrap().is_none(),
        "follow viewer exited early"
    );
    let before = live.log().len();
    let _ = viewer.kill().await;
    let _ = viewer.wait().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after = live.log().len();
    assert_eq!(before, after, "disconnecting a viewer wrote to the log");
    let (code, _stderr) = live.end().await;
    assert_eq!(code, 0, "session died with its viewer");
}

/// A real bg job settling must fire the `bg_settled` notification (M11 Build 4).
/// This was the blocker proof for M11 B1: it failed deterministically before the
/// `note_bg` dispatch was added, and is part of the normal suite again.
#[tokio::test]
async fn r5_a_settled_bg_job_fires_its_notify_hook() {
    init();
    let _slot = slot();
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("bg-notify.txt");
    // Use the config's default shell: the gate's env may not put `bash` on
    // PATH. `sleep 5` is long enough for promotion to win the race on both
    // Windows and Linux (`sleep 1` sometimes finishes first on Linux).
    let config = format!(
        "promote_after = 1\n[notify]\nbg_settled = '{}'\n",
        hook_touch_command(&marker)
    );
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_p",
            "bash",
            &json!({"command": "sleep 5; echo the-job-ran"}),
        ))),
        Reply::Sse(sse(&text_turn("carrying on while it runs", 0))),
        Reply::Sse(sse(&text_turn("noted: the job finished", 0))),
    ])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], &config).await;
    let settled = live.wait_for("bg_settled").await;
    assert_eq!(settled["state"], "exited", "{settled}");
    let started = Instant::now();
    while !marker.exists() && started.elapsed() < Duration::from_secs(8) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let fired = marker.exists();
    let notify_events: Vec<_> = live
        .log()
        .into_iter()
        .filter(|e| e["type"] == "notify")
        .collect();
    assert_eq!(
        notify_events.len(),
        1,
        "exactly one notify event for the settled job: {notify_events:?}"
    );
    assert_eq!(notify_events[0]["event"], "bg_settled", "{notify_events:?}");
    let (code, _stderr) = live.end().await;
    assert_eq!(code, 0, "session exited badly");
    assert!(fired, "bg_settled produced no notify event");
}

/// Regression for B1's double-dispatch risk: a job settling while its parent
/// turn also ends produces exactly one `bg_settled` notify and one `turn_done`
/// notify. Repeated status reports for the same job are deduped by `BgSettled`.
#[tokio::test]
async fn r9_a_settled_job_and_its_turn_notify_once_each() {
    init();
    let _slot = slot();
    // The default shell and a long enough command: promotion must win the
    // race on both OSes (see r5).
    let config = "promote_after = 1\n[notify]\nbg_settled = 'true'\nturn_done = 'true'\n";
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_dual",
            "bash",
            &json!({"command": "sleep 5; echo dual-job"}),
        ))),
        Reply::Sse(sse(&text_turn("carrying on", 0))),
        Reply::Sse(sse(&text_turn("noted", 0))),
    ])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], config).await;
    let settled = live.wait_for("bg_settled").await;
    assert_eq!(settled["state"], "exited", "{settled}");
    let started = Instant::now();
    let counts = loop {
        let events: Vec<_> = live
            .log()
            .into_iter()
            .filter(|e| e["type"] == "notify")
            .collect();
        let bg = events.iter().filter(|e| e["event"] == "bg_settled").count();
        let turn = events.iter().filter(|e| e["event"] == "turn_done").count();
        if (bg >= 1 && turn >= 1) || started.elapsed() > Duration::from_secs(8) {
            break (bg, turn);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(counts, (1, 1), "one notify per kind, not double-dispatched");
    let (code, _stderr) = live.end().await;
    assert_eq!(code, 0, "session exited badly");
}

/// S2 (M11fix): `--steer` without `--follow` must be refused, not silently
/// ignored. The refusal comes before any socket access and exits 2 (usage).
#[tokio::test(flavor = "multi_thread")]
async fn r8_steer_without_follow_is_refused() {
    let (_temp, state, cwd) = temp_state();
    let lines = vec![
        event_line(1, "user", "hi"),
        event_line(2, "session_ended", "done"),
    ];
    synthetic_session(&state, "ses_nosteer", &lines);
    let attach = attach_in(
        &state,
        &cwd,
        "ses_nosteer",
        &["--steer", "--plain"],
        Some("stop\n"),
    )
    .await;
    assert_eq!(attach.code, 2, "usage error: {}", attach.stderr);
    assert!(
        attach.stderr.contains("--steer requires --follow"),
        "a clear message is required: {}",
        attach.stderr
    );
}

/// A provider error that ends the turn must fire the `error` notification.
#[tokio::test]
async fn r6_a_failed_turn_fires_its_notify_hook() {
    init();
    let _slot = slot();
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("error-notify.txt");
    let config = format!(
        "shell = 'bash'\n[notify]\nerror = '{}'\n",
        hook_touch_command(&marker)
    );
    let mock = Mock::start(vec![Reply::Http(400, "provider refused".into())]).await;
    let run = run_task(&mock, &["run", "--json", "say hi"], &[], None, &config).await;
    assert_ne!(run.code, 0, "the provider error should end with an error");
    let started = Instant::now();
    while !marker.exists() && started.elapsed() < Duration::from_secs(8) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(marker.exists(), "error notification never fired");
    assert_eq!(run.events_of("notify").len(), 1, "one notify event");
    assert_eq!(run.events_of("notify")[0]["event"], "error");
}

/// A turn paused at an explicit step cap must fire `paused`.
#[tokio::test]
async fn r7_a_paused_turn_fires_its_notify_hook() {
    init();
    let _slot = slot();
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("paused-notify.txt");
    let config = format!(
        "shell = 'bash'\n[notify]\npaused = '{}'\n",
        hook_touch_command(&marker)
    );
    let mock = Mock::start(vec![Reply::Sse(sse(&tool_turn(
        "call_cap",
        "bash",
        &json!({"command": "echo cap"}),
    )))])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "--max-steps", "1", "say hi"],
        &[],
        None,
        &config,
    )
    .await;
    assert_eq!(run.one_event("turn_ended")["outcome"], "paused");
    let started = Instant::now();
    while !marker.exists() && started.elapsed() < Duration::from_secs(8) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(marker.exists(), "paused notification never fired");
}

fn hook_touch_command(marker: &Path) -> String {
    let path = marker.to_string_lossy().replace('\\', "/");
    format!("printf %s \"$KELIR_NOTIFY_EVENT\" > {path}")
}
