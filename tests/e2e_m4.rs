//! M4 end-to-end: background jobs against the mock provider and real processes.
//!
//! These tests are about processes that really exist: a call is promoted past
//! its limit, the job outlives the session that started it (a `taskkill`-style
//! kill of the session, not a graceful exit), `kelir bg out` still reads it,
//! `resume` folds it into the log, a session that dies without promoting takes
//! its tree with it, and `cancel` kills a child of a child.
//!
//! Liveness is proven by the work itself (a file a background loop keeps
//! writing) and, where a pid matters, by asking the OS about that pid.

mod support;

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::*;

/// Is `pid` still a running process?
fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let tasklist = Path::new(&system_root)
            .join("System32")
            .join("tasklist.exe");
        let output = std::process::Command::new(tasklist)
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output();
        match output {
            Ok(output) => String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()),
            // No tasklist on this box: fall back to "assume it died", the
            // behavioural checks below are the real proof.
            Err(_) => false,
        }
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

fn bytes_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// Poll `check` every 100 ms until it holds, or fail with `what`.
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

fn events_of<'a>(log: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|event| event["type"] == event_type)
        .collect()
}

fn text_of(event: &Value, key: &str) -> String {
    event[key].as_str().unwrap_or("").to_string()
}

/// The bash call's output directory name, which is also its job id once it is
/// promoted (`<seq>-<step>-<n>`).
fn promoted_id(settled: &Value) -> String {
    let out_path = text_of(settled, "out_path");
    let dir = Path::new(&out_path)
        .parent()
        .expect("the output file has a directory");
    dir.file_name()
        .expect("the output directory has a name")
        .to_string_lossy()
        .into_owned()
}

/// A call still running past `promote_after` becomes a background job: the
/// model is told its id, the call's result says `promoted`, the job finishes on
/// its own and a message from `job` wakes the model at the next boundary.
#[tokio::test]
async fn a_call_past_the_limit_is_promoted_and_wakes_the_model_when_it_settles() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_p",
            "bash",
            &json!({"command": "sleep 2; echo the-job-ran"}),
        ))),
        Reply::Sse(sse(&text_turn("carrying on while it runs", 0))),
        Reply::Sse(sse(&text_turn("noted: the job finished", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "promote_after = 1\n",
    )
    .await;

    let settled = live.wait_for("tool_settled").await;
    assert_eq!(
        settled["outcome"], "promoted",
        "the call was not promoted: {settled}"
    );
    let id = promoted_id(&settled);
    let result = text_of(&settled, "result");
    assert!(result.contains(&format!("bg job `{id}`")), "{result}");
    assert!(result.contains("kelir bg out"), "{result}");
    assert!(result.contains("kelir bg wait"), "{result}");
    assert!(result.contains("kelir bg cancel"), "{result}");

    let started = live.wait_for("bg_started").await;
    assert_eq!(started["bg_id"], id.as_str(), "{started}");
    assert_eq!(started["call_id"], "call_p", "{started}");
    assert!(
        text_of(&started, "cmd").contains("sleep 2"),
        "the job's command line is not in the log: {started}"
    );

    // The turn carries on while the job runs.
    let ended = live.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done");

    // ...and the job's completion wakes the model as a message from `job`.
    let settled_event = live.wait_for("bg_settled").await;
    assert_eq!(settled_event["bg_id"], id.as_str(), "{settled_event}");
    assert_eq!(settled_event["state"], "exited", "{settled_event}");
    assert_eq!(settled_event["exit_code"], 0, "{settled_event}");
    let wake = live.wait_for("user").await;
    assert_eq!(wake["from"], "job", "{wake}");
    assert!(
        wake["delivery"] == "next" || wake["delivery"] == "now",
        "{wake}"
    );
    let wake_text = text_of(&wake, "text");
    assert!(
        wake_text.contains(&format!("bg job {id} exited 0")),
        "{wake_text}"
    );
    assert!(wake_text.contains("the-job-ran"), "{wake_text}");
    let ended = live.wait_for("turn_ended").await;
    assert_eq!(ended["outcome"], "done");

    // The request after the wake carries the message: the agent saw it.
    let requests = mock.requests();
    assert_eq!(requests.len(), 3, "{}", live.stderr());
    let last = requests.last().unwrap();
    let seen: Vec<String> = last
        .messages()
        .iter()
        .filter(|message| message["role"] == "user")
        .map(|message| message["content"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        seen.iter()
            .any(|text| text.contains(&format!("bg job {id} exited 0"))),
        "the wake message never reached the model: {seen:?}"
    );

    // The output file holds what the job printed, and `kelir bg out` reads it.
    let status = live.session_dir.join("bg").join(format!("{id}.status"));
    let status_text = std::fs::read_to_string(&status).expect("a status file");
    assert!(
        status_text.contains("\"state\":\"exited\""),
        "{status_text}"
    );
    let sandbox = Sandbox::from_live(&live);
    let out = sandbox
        .kelir(&mock.base_url(), &["bg", "out", id.as_str()], &[])
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("the-job-ran"), "{}", out.stdout);

    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// The milestone's headline: kelir is killed (not ended) while a promoted job
/// runs; the job survives, keeps writing, `kelir bg out`/`wait` still read it
/// with no session at all, and `kelir resume` folds it into the log as
/// `bg_settled` plus the message that tells the model.
#[tokio::test]
async fn a_promoted_job_survives_a_killed_session_and_resume_folds_it_in() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_j",
            "bash",
            &json!({"command": "sleep 3; echo from-the-job > job.txt; echo done"}),
        ))),
        Reply::Sse(sse(&text_turn("waiting for the job", 0))),
        Reply::Sse(sse(&text_turn("the job is done", 0))),
    ])
    .await;
    let mut live = LiveSession::start(
        &mock,
        &["run", "--keep", "--json", "go"],
        "promote_after = 1\n",
    )
    .await;
    let settled = live.wait_for("tool_settled").await;
    assert_eq!(settled["outcome"], "promoted", "{settled}");
    let id = promoted_id(&settled);
    live.wait_for("turn_ended").await;

    let status_file = live.session_dir.join("bg").join(format!("{id}.status"));
    let status: Value = serde_json::from_str(&std::fs::read_to_string(&status_file).unwrap())
        .expect("the status file is JSON");
    let pid = status["pid"].as_u64().expect("the job's pid") as u32;
    assert!(pid_alive(pid), "the job's command is not running");

    let sandbox = Sandbox::from_live(&live);
    let session_id = live.id.clone();
    // The kill a `taskkill /F` does: no chance to clean up.
    live.kill().await;
    assert!(
        pid_alive(pid),
        "the promoted job died with the session (pid {pid})"
    );

    // No session to ask: the CLI finds the job through the state dir.
    let waited = sandbox
        .kelir(
            &mock.base_url(),
            &["bg", "wait", id.as_str(), "--timeout", "30"],
            &[],
        )
        .await;
    assert_eq!(waited.code, 0, "{}", waited.stderr);
    assert!(waited.stdout.contains("exited"), "{}", waited.stdout);
    let out = sandbox
        .kelir(&mock.base_url(), &["bg", "out", id.as_str()], &[])
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("done"), "{}", out.stdout);
    assert!(sandbox.cwd.join("job.txt").is_file(), "the job was killed");
    assert!(!pid_alive(pid), "the job outlived its own command");

    // Resume folds the status file into the log and wakes the model with it.
    let resumed = sandbox
        .kelir(
            &mock.base_url(),
            &["resume", "--json", session_id.as_str()],
            &[],
        )
        .await;
    assert_eq!(resumed.code, 0, "{}\n{}", resumed.stdout, resumed.stderr);
    let log = sandbox.log(&session_id);
    let settled_events = events_of(&log, "bg_settled");
    assert_eq!(settled_events.len(), 1, "{log:?}");
    assert_eq!(settled_events[0]["bg_id"], id.as_str());
    assert_eq!(settled_events[0]["exit_code"], 0);
    let wake = log
        .iter()
        .find(|event| event["type"] == "user" && event["from"] == "job")
        .unwrap_or_else(|| panic!("no wake message in {log:?}"));
    let wake_text = text_of(wake, "text");
    assert!(
        wake_text.contains(&format!("bg job {id} exited 0")),
        "{wake_text}"
    );
    assert!(wake_text.contains("done"), "{wake_text}");
    // The resumed turn's request carried it.
    let requests = mock.requests();
    let last = requests.last().expect("the resumed request");
    let seen = last.messages().iter().any(|message| {
        message["content"]
            .as_str()
            .map(|text| text.contains(&format!("bg job {id} exited 0")))
            .unwrap_or(false)
    });
    assert!(seen, "the resumed request has no wake message: {last:?}");
    assert_eq!(log.last().unwrap()["type"], "session_ended");
}

/// `kelir bg run` from inside a bash call: the job is detached from the call
/// (the call returns at once), the session records `bg_started` with the call
/// id that asked for it, and the wake arrives when it settles.
#[tokio::test]
async fn a_job_started_inside_a_call_is_recorded_and_wakes_the_model() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_bg",
            "bash",
            &json!({"command": "\"$KELIR_BIN\" bg run 'sleep 2; echo job-output'"}),
        ))),
        Reply::Sse(sse(&text_turn("started it, carrying on", 0))),
        Reply::Sse(sse(&text_turn("the background job finished", 0))),
    ])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;

    let settled = live.wait_for("tool_settled").await;
    assert_eq!(settled["outcome"], "ok", "{settled}");
    let result = text_of(&settled, "result");
    assert!(result.contains("kelir bg out"), "{result}");
    let id = result
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .expect("the job id on the first line")
        .to_string();
    assert!(id.starts_with("bg_"), "{result}");

    let started = live.wait_for("bg_started").await;
    assert_eq!(started["bg_id"], id.as_str(), "{started}");
    assert_eq!(started["call_id"], "call_bg", "{started}");
    live.wait_for("turn_ended").await;

    let settled_event = live.wait_for("bg_settled").await;
    assert_eq!(settled_event["bg_id"], id.as_str(), "{settled_event}");
    assert_eq!(settled_event["exit_code"], 0, "{settled_event}");
    let wake = live.wait_for("user").await;
    assert_eq!(wake["from"], "job", "{wake}");
    assert!(text_of(&wake, "text").contains("job-output"), "{wake}");
    live.wait_for("turn_ended").await;

    let requests = mock.requests();
    assert_eq!(requests.len(), 3, "{}", live.stderr());
    let seen = requests.last().unwrap().messages().iter().any(|message| {
        message["content"]
            .as_str()
            .map(|text| text.contains("job-output"))
            .unwrap_or(false)
    });
    assert!(seen, "the wake never reached the model");

    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// The `bg` verb is the shim's word only: a client cannot fabricate a job, so
/// the log's `bg_started`/`bg_settled` events can be trusted.
#[tokio::test]
async fn a_client_cannot_fabricate_a_background_job() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &json!({"command": "echo hi"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    live.wait_for("tool_settled").await;
    live.wait_for("turn_ended").await;
    let fake = json!({
        "id": "bg_fake",
        "state": "exited",
        "started": "2026-11-19T00:00:00Z",
        "exit_code": 0,
    });
    let refused = live.ctl(&["bg", &fake.to_string()]).await;
    assert_eq!(refused.code, 1, "{}{}", refused.stdout, refused.stderr);
    let answer = refused.response();
    assert_eq!(answer["ok"], false, "{answer}");
    assert!(
        answer["error"]
            .as_str()
            .unwrap_or("")
            .contains("internal verb"),
        "{answer}"
    );
    assert!(
        !live.log().iter().any(|event| event["type"] == "bg_started"),
        "a fabricated job reached the log"
    );
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// `cancel` takes down the job's whole tree — here two loops that are children
/// of the job's shell — and records `cancelled`. The proof is the work: neither
/// loop writes another byte afterwards.
#[tokio::test]
async fn cancel_kills_a_tree_of_children_and_records_it() {
    let sandbox = Sandbox::new("");
    let command = "( while true; do echo tick >> one.txt; sleep 0.2; done ) & \
                   ( while true; do echo tick >> two.txt; sleep 0.2; done ) & wait";
    let started = sandbox
        .kelir("http://127.0.0.1:9/v1", &["bg", "run", command], &[])
        .await;
    assert_eq!(started.code, 0, "stderr: {}", started.stderr);
    let id = started
        .stdout
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    assert!(id.starts_with("bg_"), "{}", started.stdout);
    assert!(started.stdout.contains("running"), "{}", started.stdout);

    let one = sandbox.cwd.join("one.txt");
    let two = sandbox.cwd.join("two.txt");
    wait_until("both loops to write", Duration::from_secs(20), || {
        bytes_len(&one) > 0 && bytes_len(&two) > 0
    })
    .await;

    let cancelled = sandbox
        .kelir("http://127.0.0.1:9/v1", &["bg", "cancel", id.as_str()], &[])
        .await;
    assert_eq!(cancelled.code, 0, "stderr: {}", cancelled.stderr);
    assert!(
        cancelled.stdout.contains("cancelled"),
        "{}",
        cancelled.stdout
    );

    let (before_one, before_two) = (bytes_len(&one), bytes_len(&two));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        bytes_len(&one),
        before_one,
        "the job's first child survived cancel"
    );
    assert_eq!(
        bytes_len(&two),
        before_two,
        "the job's second child survived cancel"
    );

    let listed = sandbox
        .kelir("http://127.0.0.1:9/v1", &["bg", "ls"], &[])
        .await;
    assert_eq!(listed.code, 0, "{}", listed.stderr);
    assert!(listed.stdout.contains("cancelled"), "{}", listed.stdout);
    let waited = sandbox
        .kelir("http://127.0.0.1:9/v1", &["bg", "wait", id.as_str()], &[])
        .await;
    assert_eq!(waited.code, 0, "{}", waited.stderr);
    assert!(waited.stdout.contains("cancelled"), "{}", waited.stdout);
}

/// The other half of the same guarantee: a session that dies **without**
/// promoting a call takes the call's tree with it, so a `kill -9` leaves no
/// orphan grinding in the background.
#[tokio::test]
async fn a_session_killed_mid_call_leaves_no_orphan_behind() {
    let mock = Mock::start(vec![Reply::Sse(sse(&tool_turn(
        "call_o",
        "bash",
        &json!({"command": "while true; do echo tick >> ticks.txt; sleep 0.2; done"}),
    )))])
    .await;
    // The default promote_after (600 s): this call is never promoted.
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    let started = live.wait_for("tool_started").await;
    assert_eq!(started["name"], "bash");
    let ticks = live.cwd.join("ticks.txt");
    wait_until("the call to write", Duration::from_secs(20), || {
        bytes_len(&ticks) > 0
    })
    .await;

    live.kill().await;

    // The shim notices the session is gone and takes the tree down; nothing
    // may keep writing after that.
    wait_until("the orphan to stop", Duration::from_secs(20), || {
        let first = bytes_len(&ticks);
        std::thread::sleep(Duration::from_millis(600));
        bytes_len(&ticks) == first
    })
    .await;
}
