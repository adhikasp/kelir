//! STEER-7 end-to-end: the session socket's trust boundary, driven against the
//! real binary and the scripted mock.
//!
//! Retro M5-A F8: during dogfooding a test child inside a running session
//! impersonated the shim (43 injected "bg job was lost" messages) and one run
//! cancelled a job belonging to the driver. Reaching the socket used to be the
//! whole of the authorisation. What is proven here, over a raw socket exactly
//! as a hostile script would use it:
//!
//! * a job id this session did not create is refused for `bg`, `cancel` and
//!   `bg-spawn`, and the refusal is a `verb` event marked `rejected`;
//! * a job directory or status/out path outside the session's own `bg/` is
//!   refused (including `..` and, on Windows, another drive or a UNC path);
//! * a request with no token, or with another session's token, is answered and
//!   dropped at the listener, before the session can log or act on it;
//! * a child of one session cannot take down another session's job;
//! * `kelir ctl` keeps working, because it reads `<session dir>/token`;
//! * the token itself never lands in the log, in stdout or in an argv.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::Path;

use crate::support::*;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// One request on a fresh raw connection to the session's socket, as a script
/// that found the pipe would send it: newline-delimited JSON in, one line out.
async fn raw(session: &LiveSession, request: Value) -> Value {
    let stream = kelir::ipc::connect(&session.session_dir, &session.id)
        .await
        .expect("connect to the session socket");
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read);
    let mut line = request.to_string();
    line.push('\n');
    let mut answer = String::new();
    let read = tokio::time::timeout(CEILING, async {
        write.write_all(line.as_bytes()).await.expect("write");
        write.flush().await.expect("flush");
        lines.read_line(&mut answer).await.expect("read the answer")
    })
    .await
    .expect("the session answered within 20 s");
    assert!(read > 0, "the session closed the socket without answering");
    serde_json::from_str(answer.trim())
        .unwrap_or_else(|error| panic!("the answer is not JSON ({error}): {answer}"))
}

/// The session's token, read the way `kelir ctl` reads it.
fn token_of(session: &LiveSession) -> String {
    kelir::trust::read_token(&session.session_dir).expect("the session wrote a token file")
}

/// The `verb` events in the session's log, as strings for a readable assertion.
fn verb_events(session: &LiveSession) -> Vec<Value> {
    session
        .log()
        .into_iter()
        .filter(|event| event["type"] == "verb")
        .collect()
}

/// A status report a *client* would not be allowed to send: an id this session
/// never started, in the shape the shim sends.
fn foreign_report(id: &str, out: &Path) -> Value {
    json!({
        "verb": "bg",
        "from": "job",
        "args": {
            "id": id,
            "state": "running",
            "started": "2026-11-19T00:00:00Z",
            "out": kelir::util::path_string(out),
            "kind": "job",
        },
    })
}

/// F8, first half: a `bg` report for a job this session did not create is
/// refused — even with a perfectly good token — and the attempt is recorded as
/// a `rejected` verb, because the caller is authenticated and the log should
/// say what it tried.
#[tokio::test]
async fn a_foreign_job_report_is_refused_and_recorded() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let token = token_of(&session);
    let outside = session.state.join("elsewhere").join("bg_fake.out");

    let mut request = foreign_report("bg_fake", &outside);
    request["token"] = json!(token);
    let answer = raw(&session, request).await;
    assert_eq!(answer["ok"], false, "{answer}");
    let error = answer["error"].as_str().unwrap_or("");
    assert!(
        error.contains("did not start job `bg_fake`"),
        "the refusal says which id: {answer}"
    );
    assert!(
        !error.contains(&token),
        "the refusal echoed the token: {answer}"
    );

    let verbs = verb_events(&session);
    let refused = verbs
        .iter()
        .find(|event| event["name"] == "bg")
        .unwrap_or_else(|| panic!("no bg verb event: {verbs:?}"));
    assert_eq!(refused["boundary"], "rejected", "{refused}");
    assert_eq!(refused["from"], "job", "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap_or("")
            .contains("did not start job"),
        "{refused}"
    );
    assert!(
        !session
            .log()
            .iter()
            .any(|event| event["type"] == "bg_started"),
        "a fabricated job reached the log"
    );
    let (code, stderr) = session.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// The same for `cancel`: the verb exists now (STEER-7), and it refuses an id
/// that is not this session's before anything is signalled.
#[tokio::test]
async fn a_foreign_cancel_is_refused() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let token = token_of(&session);

    for id in ["bg_other", "../escape", "12-3-4"] {
        let answer = raw(
            &session,
            json!({"verb": "cancel", "from": "ctl", "args": {"id": id}, "token": token}),
        )
        .await;
        assert_eq!(answer["ok"], false, "{id}: {answer}");
        let error = answer["error"].as_str().unwrap_or("");
        assert!(
            error.contains("did not start job") || error.contains("is not a job id"),
            "{id}: {answer}"
        );
    }
    let refused: Vec<Value> = verb_events(&session)
        .into_iter()
        .filter(|event| event["name"] == "cancel")
        .collect();
    assert_eq!(refused.len(), 3, "{refused:?}");
    assert!(refused.iter().all(|event| event["boundary"] == "rejected"));
    let (code, stderr) = session.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// `bg-spawn` is the one verb a job sends on the session's behalf (on Windows a
/// `kelir bg run` inside a call is trapped in the call's Job Object). A spawn
/// aimed anywhere but this session's own `bg/` is refused, and logged.
#[tokio::test]
async fn a_bg_spawn_outside_the_sessions_bg_dir_is_refused() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let token = token_of(&session);
    let other = session.state.join("other-session").join("bg");

    let spawn = json!({
        "verb": "bg-spawn",
        "from": "job",
        "token": token,
        "args": {
            "dir": kelir::util::path_string(&other),
            "id": "bg_1",
            "out": kelir::util::path_string(&other.join("bg_1.out")),
            "cmd": "true",
            "shell": ["bash"],
            "kind": "job",
        },
    });
    let answer = raw(&session, spawn).await;
    assert_eq!(answer["ok"], false, "{answer}");
    assert!(
        answer["error"]
            .as_str()
            .unwrap_or("")
            .contains("not inside this session"),
        "{answer}"
    );
    // Nothing was started: no status file appeared anywhere.
    assert!(!other.join("bg_1.status").exists());
    let refused = verb_events(&session)
        .into_iter()
        .find(|event| event["name"] == "bg-spawn")
        .unwrap_or_else(|| panic!("the refusal was not logged: {:?}", session.log()));
    assert_eq!(refused["boundary"], "rejected", "{refused}");
    let (code, stderr) = session.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// A report for a job of this session's own, whose `out` (or `cmd_path`) points
/// out of the session's directories, is refused: a status path is not a way to
/// make the session read or write anywhere it likes.
#[tokio::test]
async fn a_path_escaping_the_session_is_refused() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let token = token_of(&session);
    // A call's output directory name is one of this session's job ids; this
    // test starts a real job so the id is genuinely the session's own.
    let stolen = session.state.join("stolen.out");

    // Start a real job of this session so its id is genuinely the session's.
    let sandbox = Sandbox::from_live(&session);
    let started = sandbox
        .kelir(
            "http://127.0.0.1:9/v1",
            &["bg", "run", "sleep 5; echo job"],
            &[
                (
                    "KELIR_SESSION_DIR",
                    &kelir::util::path_string(&session.session_dir),
                ),
                (
                    "KELIR_SOCK",
                    &kelir::ipc::value(&session.session_dir, &session.id),
                ),
                ("KELIR_TOKEN", &token),
            ],
        )
        .await;
    assert_eq!(started.code, 0, "stderr: {}", started.stderr);
    let id = started
        .stdout
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    assert!(id.starts_with("bg_"), "{}", started.stdout);

    let mut request = foreign_report(&id, &stolen);
    request["token"] = json!(token);
    let answer = raw(&session, request).await;
    assert_eq!(answer["ok"], false, "{answer}");
    assert!(
        answer["error"]
            .as_str()
            .unwrap_or("")
            .contains("not inside this session's own"),
        "{answer}"
    );
    assert!(!stolen.exists(), "the session wrote outside its own bg/");

    // Clean up the job this test really started (bounded: `sleep 5` anyway).
    let cancelled = session.ctl(&["cancel", &id]).await;
    assert_eq!(
        cancelled.code, 0,
        "{}{}",
        cancelled.stdout, cancelled.stderr
    );
    let (code, stderr) = session.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// B1, the whole point: a tool child holds the session's token, so a report for
/// one of the session's *own* jobs still needs that job's capability. With the
/// token alone it is refused, and nothing it claims reaches the log.
#[tokio::test]
async fn a_report_with_the_token_but_no_capability_is_refused() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let token = token_of(&session);

    // A real job of this session: the same route a bash child takes.
    let sandbox = Sandbox::from_live(&session);
    let started = sandbox
        .kelir(
            "http://127.0.0.1:9/v1",
            &["bg", "run", "sleep 5; echo job"],
            &[
                (
                    "KELIR_SESSION_DIR",
                    &kelir::util::path_string(&session.session_dir),
                ),
                (
                    "KELIR_SOCK",
                    &kelir::ipc::value(&session.session_dir, &session.id),
                ),
                ("KELIR_TOKEN", &token),
            ],
        )
        .await;
    assert_eq!(started.code, 0, "stderr: {}", started.stderr);
    let id = started
        .stdout
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    assert!(id.starts_with("bg_"), "{}", started.stdout);
    let out = session.session_dir.join("bg").join(format!("{id}.out"));

    // The token, a real job id, and a claim that it was lost: exactly what a
    // child can write, without the job's capability.
    for capability in [None, Some("0".repeat(64)), Some("wrong".to_string())] {
        let mut request = json!({
            "verb": "bg",
            "from": "job",
            "token": token,
            "args": {"id": id, "state": "lost", "started": "2026-11-19T00:00:00Z",
                     "out": kelir::util::path_string(&out), "kind": "job",
                     "cmd": "forged"},
        });
        if let Some(capability) = capability {
            request["capability"] = json!(capability);
        }
        let answer = raw(&session, request).await;
        assert_eq!(answer["ok"], false, "{answer}");
        assert!(
            answer["error"]
                .as_str()
                .unwrap_or("")
                .contains("capability"),
            "the refusal names what is missing: {answer}"
        );
    }
    let log = session.log();
    assert!(
        !log.iter()
            .any(|event| event["type"] == "user" && event["from"] == "job"),
        "a forged `lost` report reached the log: {log:?}"
    );
    assert!(
        !log.iter().any(|event| event["type"] == "bg_settled"),
        "a forged report settled the job in the log: {log:?}"
    );

    // Clean up the job this test really started, and its shim.
    let cancelled = session.ctl(&["cancel", &id]).await;
    assert_eq!(
        cancelled.code, 0,
        "{}{}",
        cancelled.stdout, cancelled.stderr
    );
    let (code, stderr) = session.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// F8, second half: reaching the socket is not authorisation. A request with no
/// token is answered and dropped at the listener — the session never sees it, so
/// it cannot log it either.
#[tokio::test]
async fn a_request_without_a_token_never_reaches_the_session() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let before = session.log().len();

    let answer = raw(
        &session,
        json!({"verb": "say", "from": "ctl", "args": {"text": "injected"}}),
    )
    .await;
    assert_eq!(answer["ok"], false, "{answer}");
    assert!(
        answer["error"].as_str().unwrap_or("").contains("no token"),
        "the refusal says what is missing: {answer}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let log = session.log();
    assert_eq!(
        log.len(),
        before,
        "an unauthenticated verb was logged: {log:?}"
    );
    assert!(
        !log.iter().any(|event| event["type"] == "verb"),
        "an unauthenticated verb wrote a verb event: {log:?}"
    );
    // The socket still works for a caller that does carry the token: the
    // listener must not have shut down.
    let token = token_of(&session);
    let answer = raw(
        &session,
        json!({"verb": "say", "from": "ctl", "args": {"text": "legit"}, "token": token}),
    )
    .await;
    assert_eq!(answer["ok"], true, "{answer}");
    session.wait_for("turn_ended").await;
    let (code, stderr) = session.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// The token is per session: another session's token does not open this one,
/// even though both files sit in the same state directory and the same user can
/// read them.
#[tokio::test]
async fn another_sessions_token_does_not_open_this_socket() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("ok", 0))),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    let mut first = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    first.wait_for("turn_ended").await;
    let mut second = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    second.wait_for("turn_ended").await;
    let first_token = token_of(&first);
    let second_token = token_of(&second);
    assert_ne!(first_token, second_token, "two sessions, two tokens");

    let answer = raw(
        &second,
        json!({"verb": "say", "from": "ctl", "args": {"text": "hi"}, "token": first_token}),
    )
    .await;
    assert_eq!(answer["ok"], false, "{answer}");
    assert!(
        answer["error"]
            .as_str()
            .unwrap_or("")
            .contains("not this session's"),
        "{answer}"
    );
    assert!(
        !second.log().iter().any(|event| event["type"] == "verb"),
        "a foreign token got a verb logged"
    );
    for session in [&mut first, &mut second] {
        let answer = session.ctl(&["say", "still fine"]).await;
        assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    }
    first.wait_for("turn_ended").await;
    second.wait_for("turn_ended").await;
    let (code, stderr) = first.end().await;
    assert_eq!(code, 0, "{stderr}");
    let (code, stderr) = second.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// The dogfooding incident itself: a child of one session takes down another
/// session's job. Both halves of the boundary have to hold — the child's token
/// is refused by the other socket, and the child's own session refuses a job id
/// it did not create.
#[tokio::test]
async fn a_child_of_one_session_cannot_cancel_another_sessions_job() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("ok", 0))),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    let mut driver = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    driver.wait_for("turn_ended").await;
    let mut other = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    other.wait_for("turn_ended").await;
    let driver_token = token_of(&driver);
    let other_token = token_of(&other);

    // A real job of the *other* session: it must survive everything below.
    let sandbox = Sandbox::from_live(&other);
    let started = sandbox
        .kelir(
            "http://127.0.0.1:9/v1",
            &["bg", "run", "sleep 8; echo job"],
            &[
                (
                    "KELIR_SESSION_DIR",
                    &kelir::util::path_string(&other.session_dir),
                ),
                (
                    "KELIR_SOCK",
                    &kelir::ipc::value(&other.session_dir, &other.id),
                ),
                ("KELIR_TOKEN", &other_token),
            ],
        )
        .await;
    assert_eq!(started.code, 0, "stderr: {}", started.stderr);
    let id = started
        .stdout
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    assert!(id.starts_with("bg_"), "{}", started.stdout);

    // Half one: the driver's token on the other session's socket.
    let answer = raw(
        &other,
        json!({"verb": "cancel", "from": "ctl", "args": {"id": id}, "token": driver_token}),
    )
    .await;
    assert_eq!(answer["ok"], false, "{answer}");
    assert!(
        answer["error"]
            .as_str()
            .unwrap_or("")
            .contains("not this session's"),
        "{answer}"
    );

    // Half two: the driver's own socket, asked to cancel a foreign job id.
    let answer = raw(
        &driver,
        json!({"verb": "cancel", "from": "ctl", "args": {"id": id}, "token": driver_token}),
    )
    .await;
    assert_eq!(answer["ok"], false, "{answer}");
    assert!(
        answer["error"]
            .as_str()
            .unwrap_or("")
            .contains("did not start job"),
        "{answer}"
    );

    // The job is untouched: its status still says running.
    let status = std::fs::read_to_string(other.session_dir.join("bg").join(format!("{id}.status")))
        .expect("the job's status file");
    let status: Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["state"], "running", "{status}");

    // Clean up by the session that does own it.
    let cancelled = other.ctl(&["cancel", &id]).await;
    assert_eq!(
        cancelled.code, 0,
        "{}{}",
        cancelled.stdout, cancelled.stderr
    );
    let (code, stderr) = driver.end().await;
    assert_eq!(code, 0, "{stderr}");
    let (code, stderr) = other.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// `cancel` as a verb really cancels: the new path in the session (`Verb::Cancel`
/// → `bg::cancel_in`), not just the CLI's `kelir bg cancel`, and the settled
/// report reaches the log through the session's own watcher.
#[tokio::test]
async fn ctl_cancel_kills_an_owned_job_and_the_log_records_it() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let token = token_of(&session);
    let sandbox = Sandbox::from_live(&session);
    let started = sandbox
        .kelir(
            "http://127.0.0.1:9/v1",
            &["bg", "run", "sleep 20; echo never"],
            &[
                (
                    "KELIR_SESSION_DIR",
                    &kelir::util::path_string(&session.session_dir),
                ),
                (
                    "KELIR_SOCK",
                    &kelir::ipc::value(&session.session_dir, &session.id),
                ),
                ("KELIR_TOKEN", &token),
            ],
        )
        .await;
    assert_eq!(started.code, 0, "stderr: {}", started.stderr);
    let id = started
        .stdout
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    assert!(id.starts_with("bg_"), "{}", started.stdout);

    let answer = session.ctl(&["cancel", &id]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    assert_eq!(answer.response()["ok"], true, "{}", answer.stdout);
    let settled = session.wait_for("bg_settled").await;
    assert_eq!(settled["bg_id"], id.as_str(), "{settled}");
    assert_eq!(settled["state"], "cancelled", "{settled}");
    let (code, stderr) = session.end().await;
    assert_eq!(code, 0, "{stderr}");
}

/// `kelir ctl` from a human's terminal keeps working: it reads the token from
/// the session's own directory, and that file exists, is not world-readable, and
/// is the only place the secret lives — never the log, never stdout, never an
/// argv.
#[tokio::test]
async fn ctl_reads_the_token_file_and_the_secret_is_never_logged() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let token = token_of(&session);
    assert_eq!(token.len(), 64, "256 bits, hex: {token}");
    assert!(token.chars().all(|ch| ch.is_ascii_hexdigit()));
    let path = session.session_dir.join("token");
    assert!(
        kelir::trust::owner_only(&path).unwrap(),
        "{}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
    }

    // The human's route: `kelir ctl` needs no environment of its own.
    let answer = session.ctl(&["say", "from a terminal"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    session.wait_for("turn_ended").await;
    let session_dir = session.session_dir.clone();
    let ndjson: String = session
        .events
        .iter()
        .map(|event| event.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    // The log is read before the session ends: ending it drops the test's temp
    // directory, the session dir included.
    let log = std::fs::read_to_string(session_dir.join("events.jsonl")).unwrap();
    let (code, stderr) = session.end().await;
    assert_eq!(code, 0, "{stderr}");

    // The secret is nowhere it could be read: not in the log, not in what the
    // session printed, never in an argv.
    assert!(!log.contains(&token), "the token reached the event log");
    assert!(!ndjson.contains(&token), "the token reached stdout");
    assert!(!stderr.contains(&token), "the token reached stderr");
}

/// The token is exported to the session's own tool children (`KELIR_TOKEN`),
/// together with the socket address, and it is not echoed; the file that holds
/// it does not outlive the session that minted it (STEER-7 review S1).
///
/// A child that holds the token still cannot impersonate a job's shim: that is
/// what the per-job capability is for, and `tests/review_steer7.rs` is the
/// proof of it. This test only pins the export and the retirement.
#[tokio::test]
async fn a_tool_child_gets_the_token_and_the_file_does_not_outlive_the_session() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_env",
            "bash",
            &json!({"command": "if [ -n \"$KELIR_TOKEN\" ]; then echo has-token; else echo no-token; fi; \
                     if [ -n \"$KELIR_SOCK\" ]; then echo has-sock; fi"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "go"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let settled = run.one_event("tool_settled");
    let out_path = settled["out_path"].as_str().expect("the call's out path");
    let out = std::fs::read_to_string(out_path).expect("the call's output file");
    assert!(out.contains("has-token"), "{out}");
    assert!(out.contains("has-sock"), "{out}");
    // The session has ended, and a token nobody serves is only something to
    // find: it is gone from the session directory (review S1).
    assert!(
        !run.session_dir.join("token").exists(),
        "the token file outlived the session"
    );
    // Never echoed: no 64-character hex run in what the session printed or in
    // the call's own output (the token is 32 random bytes, hex).
    for text in [&out, &run.stdout, &run.stderr] {
        let leaked = text.split(|ch: char| !ch.is_ascii_hexdigit()).find(|run| {
            run.len() >= 64
                && run
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        });
        assert!(
            leaked.is_none(),
            "something token-shaped was printed: {text}"
        );
    }
}
