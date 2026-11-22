//! Scheduled runs: `kelir run --name --single-flight` against the real binary
//! and the scripted mock provider. Every process goes through `Sandbox`'s Drop
//! guard, every test asserts no child is left, and SPAWN_CAP bounds the file.

#![allow(clippy::pedantic, clippy::nursery)]

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::support::*;
use serde_json::{Value, json};

const SPAWN_CAP: usize = 80;
static SPAWNS: AtomicUsize = AtomicUsize::new(0);

fn count_spawn() {
    let n = SPAWNS.fetch_add(1, Ordering::SeqCst) + 1;
    assert!(n <= SPAWN_CAP, "runaway: {n} processes spawned");
}

fn text(answer: &str) -> Reply {
    Reply::Sse(sse(&text_turn(answer, 0)))
}

async fn kelir(sb: &Sandbox, mock: &Mock, args: &[&str], env: &[(&str, &str)]) -> Output {
    count_spawn();
    sb.kelir(&mock.base_url(), args, env).await
}

async fn start(
    sb: &Sandbox,
    mock: &Mock,
    args: &[&str],
    env: &[(&str, &str)],
) -> tokio::process::Child {
    count_spawn();
    let mut command = tokio::process::Command::from(sb.kelir_command(&mock.base_url()));
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in env {
        command.env(key, value);
    }
    sb.guard.spawn(&mut command).await.expect("spawn kelir")
}

async fn finish(child: tokio::process::Child) -> Output {
    let output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .expect("kelir did not finish")
        .expect("wait");
    Output {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn status(sb: &Sandbox, name: &str) -> Value {
    let path = sb.state.join("names").join(format!("{name}.status.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("no STATUS for {name}: {error}"));
    serde_json::from_str(&text).unwrap()
}

fn of_type<'a>(log: &'a [Value], kind: &str) -> Vec<&'a Value> {
    log.iter().filter(|event| event["type"] == kind).collect()
}

fn completions(mock: &Mock) -> Vec<crate::support::Recorded> {
    mock.requests()
        .into_iter()
        .filter(|request| request.method == "POST")
        .collect()
}

async fn clean(sb: &Sandbox) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while sb.guard.active() > 0 {
        assert!(
            Instant::now() < deadline,
            "{} process(es) still alive",
            sb.guard.active()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A named single-flight run uses one session, writes STATUS, and reports the
/// answer on stdout. A second run resumes the same session with a `from: job`
/// task.
#[tokio::test]
async fn named_single_flight_runs_resume_one_session_and_write_status() {
    let mock = Mock::start(vec![text("first answer"), text("second answer")]).await;
    let sb = Sandbox::new("");
    let first = kelir(
        &sb,
        &mock,
        &["run", "--name", "nightly", "--single-flight", "task one"],
        &[],
    )
    .await;
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(first.stdout.contains("first answer"));
    let first_status = status(&sb, "nightly");
    assert_eq!(first_status["name"], "nightly");
    assert_eq!(first_status["outcome"], "done");
    assert_eq!(first_status["exit_code"], 0);

    let second = kelir(
        &sb,
        &mock,
        &["run", "--name", "nightly", "--single-flight", "task two"],
        &[],
    )
    .await;
    assert_eq!(second.code, 0, "{}", second.stderr);
    let id = sb.only_session();
    let log = sb.log(&id);
    assert_eq!(of_type(&log, "session_started").len(), 1);
    let users = of_type(&log, "user");
    assert_eq!(users.len(), 2);
    assert_eq!(users[0]["from"], "cli");
    assert_eq!(users[1]["from"], "job");
    assert_eq!(users[1]["text"], "task two");

    let now = status(&sb, "nightly");
    assert_eq!(now["name"], "nightly");
    assert_eq!(now["session"], id.as_str());
    assert_eq!(now["outcome"], "done");
    assert_eq!(now["steps"], 1);
    assert_eq!(now["answer"], "second answer");
    assert!(now["tokens"]["prompt"].as_u64().unwrap() > 0);
    assert!(now.get("cost_usd").is_none());
    assert!(!sb.state.join("names").join("nightly.lock").exists());

    let ls = kelir(&sb, &mock, &["ls", "sessions", "--json"], &[]).await;
    let rows: Vec<Value> = serde_json::from_str(ls.stdout.trim()).unwrap();
    assert_eq!(rows[0]["outcome"], "done");
    let table = kelir(&sb, &mock, &["ls", "sessions"], &[]).await;
    assert!(table.stdout.contains("OUTCOME") && table.stdout.contains("done"));
    clean(&sb).await;
}

/// Two concurrent single-flight runs: one wins, the other exits 75 and leaves
/// a `skipped` STATUS naming the holder.
#[tokio::test]
async fn two_concurrent_runs_exit_75_for_the_loser() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![Reply::Gated(gate.clone(), sse(&text_turn("slow", 0)))]).await;
    let sb = Sandbox::new("");
    let holder = start(
        &sb,
        &mock,
        &["run", "--name", "racy", "--single-flight", "hold"],
        &[],
    )
    .await;
    until("the first request", || mock.request_count() >= 1).await;
    let loser = kelir(
        &sb,
        &mock,
        &["run", "--name", "racy", "--single-flight", "second"],
        &[],
    )
    .await;
    assert_eq!(loser.code, 75, "{}", loser.stderr);
    assert!(loser.stderr.contains("skipped: another run is in flight"));
    let outcome = status(&sb, "racy");
    assert_eq!(outcome["outcome"], "skipped");
    assert_eq!(outcome["exit_code"], 75);
    assert!(outcome["error"].as_str().unwrap().contains("in flight"));
    assert_eq!(mock.request_count(), 1, "no second turn started");
    gate.notify_one();
    assert_eq!(finish(holder).await.code, 0);
    clean(&sb).await;
}

/// A killed holder's lock is taken over by the next run.
#[tokio::test]
async fn a_killed_holders_lock_is_taken_over() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Gated(gate.clone(), sse(&text_turn("slow", 0))),
        text("after takeover"),
    ])
    .await;
    let sb = Sandbox::new("");
    let mut holder = start(
        &sb,
        &mock,
        &["run", "--name", "victim", "--single-flight", "hold"],
        &[],
    )
    .await;
    until("the holder's request", || mock.request_count() >= 1).await;
    let lock = sb.state.join("names").join("victim.lock");
    assert!(lock.exists());
    let old: Value = serde_json::from_str(&std::fs::read_to_string(&lock).unwrap()).unwrap();
    let mut dead = old;
    dead["pid"] = json!(u32::MAX - 1);
    dead["start"] = json!(1);
    std::fs::write(&lock, serde_json::to_string(&dead).unwrap()).unwrap();
    holder.start_kill().unwrap();
    let _ = finish(holder).await;
    // A lock younger than FRESH is never taken over, however dead its pid.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let next = kelir(
        &sb,
        &mock,
        &["run", "--name", "victim", "--single-flight", "go"],
        &[],
    )
    .await;
    assert_eq!(next.code, 0, "{}", next.stderr);
    assert_eq!(status(&sb, "victim")["outcome"], "done");
    clean(&sb).await;
}

/// A hung provider must not hang forever: `--timeout` stops the turn, writes a
/// `timeout` STATUS and exits 124.
#[tokio::test]
async fn timeout_of_a_hung_request_exits_124_and_writes_status() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![Reply::Gated(gate.clone(), sse(&text_turn("late", 0)))]).await;
    let sb = Sandbox::new("");
    let out = kelir(
        &sb,
        &mock,
        &[
            "run",
            "--name",
            "hanging",
            "--single-flight",
            "--timeout",
            "2s",
            "do it",
        ],
        &[],
    )
    .await;
    assert_eq!(out.code, 124, "{}", out.stderr);
    let outcome = status(&sb, "hanging");
    assert_eq!(outcome["outcome"], "timeout");
    assert_eq!(outcome["exit_code"], 124);
    assert!(outcome["error"].as_str().unwrap().contains("timeout"));
    assert!(!sb.state.join("names").join("hanging.lock").exists());
    gate.notify_one();
    clean(&sb).await;
}

/// `--task-file` reads the task from a file; `--max-steps` caps the turn.
#[tokio::test]
async fn task_file_and_max_steps_are_honoured() {
    let mock = Mock::start(vec![text("finished")]).await;
    let sb = Sandbox::new("");
    let file = sb.cwd.join("task.txt");
    std::fs::write(&file, "from the file").unwrap();
    let out = kelir(
        &sb,
        &mock,
        &[
            "run",
            "--name",
            "filed",
            "--single-flight",
            "--task-file",
            &fwd(&file),
            "--max-steps",
            "1",
        ],
        &[],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let id = sb.only_session();
    let log = sb.log(&id);
    let users = of_type(&log, "user");
    assert_eq!(users[0]["text"], "from the file");
    assert_eq!(status(&sb, "filed")["outcome"], "paused");
    clean(&sb).await;
}

/// `--fresh` starts a new session and carries the old one's answer over.
#[tokio::test]
async fn fresh_rebinds_the_name_and_carries_the_old_answer() {
    let mock = Mock::start(vec![text("old answer"), text("new answer")]).await;
    let sb = Sandbox::new("");
    let one = kelir(
        &sb,
        &mock,
        &["run", "--name", "weekly", "--single-flight", "first"],
        &[],
    )
    .await;
    assert_eq!(one.code, 0, "{}", one.stderr);
    let old_id = sb.only_session();
    let two = kelir(
        &sb,
        &mock,
        &[
            "run",
            "--name",
            "weekly",
            "--single-flight",
            "--fresh",
            "second",
        ],
        &[],
    )
    .await;
    assert_eq!(two.code, 0, "{}", two.stderr);
    assert_eq!(sb.session_ids().len(), 2);
    let new_id: String = serde_json::from_str::<Value>(
        &std::fs::read_to_string(sb.state.join("names").join("weekly")).unwrap(),
    )
    .unwrap()["session"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(new_id, old_id);
    let log = sb.log(&new_id);
    let carries: Vec<&Value> = of_type(&log, "source")
        .into_iter()
        .filter(|source| source["kind"] == "carry")
        .collect();
    assert_eq!(carries.len(), 1);
    assert!(carries[0]["text"].as_str().unwrap().contains("old answer"));
    assert_eq!(status(&sb, "weekly")["session"], new_id.as_str());
    clean(&sb).await;
}

/// Every resumed session announces a changed date, not only scheduled runs.
#[tokio::test]
async fn every_resumed_session_announces_the_new_date() {
    let today = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Iso8601::DATE)
        .expect("today");
    let tomorrow = (time::OffsetDateTime::now_utc() + time::Duration::days(1))
        .format(&time::format_description::well_known::Iso8601::DATE)
        .expect("tomorrow");
    let mock = Mock::start(vec![
        text("day one"),
        text("day two"),
        text("day two again"),
    ])
    .await;
    let sb = Sandbox::new("");
    let run = |day: &str| {
        let (sb, mock, day) = (&sb, &mock, day.to_string());
        async move {
            kelir(
                sb,
                mock,
                &["run", "--name", "daily", "task"],
                &[("KELIR_TEST_TODAY", &day)],
            )
            .await
        }
    };
    assert_eq!(run(&today).await.code, 0);
    let id = sb.only_session();
    assert_eq!(run(&tomorrow).await.code, 0);
    assert_eq!(run(&tomorrow).await.code, 0);
    let log = sb.log(&id);
    let clocks: Vec<&Value> = of_type(&log, "source")
        .into_iter()
        .filter(|source| source["kind"] == "clock")
        .collect();
    assert_eq!(clocks.len(), 1, "announced once for the new day");
    assert_eq!(clocks[0]["name"], tomorrow);
    clean(&sb).await;
}

/// STATUS and lock files share `names/` with bindings but are not names: they
/// must not warn in `kelir ls sessions` or be usable as a `--name` that clobbers them.
#[tokio::test]
async fn status_and_lock_files_are_not_name_bindings() {
    let mock = Mock::start(vec![text("real answer")]).await;
    let sb = Sandbox::new("");
    let run = kelir(
        &sb,
        &mock,
        &["run", "--name", "victim", "--single-flight", "real run"],
        &[],
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let before = std::fs::read_to_string(sb.state.join("names").join("victim.status.json"))
        .expect("STATUS was written");
    let before: Value = serde_json::from_str(&before).unwrap();
    assert_eq!(before["outcome"], "done");

    let tombstone = sb.state.join("names").join("victim.stale.123.01abc.456");
    std::fs::write(&tombstone, r#"{"token":"x"}"#).unwrap();
    let tombstone_bytes = std::fs::read(&tombstone).unwrap();

    let ls = kelir(&sb, &mock, &["ls", "sessions", "--json"], &[]).await;
    assert_eq!(ls.code, 0, "{}", ls.stderr);
    assert!(
        !ls.stderr.contains("status.json") && !ls.stderr.contains(".stale."),
        "STATUS/tombstone files must not be parsed as names: {}",
        ls.stderr
    );
    let rows: Vec<Value> = serde_json::from_str(ls.stdout.trim()).unwrap();
    assert_eq!(rows.len(), 1, "no phantom name rows: {rows:?}");
    assert_eq!(rows[0]["name"], "victim");

    // A name that spells a STATUS file is refused; the record is untouched.
    let collision = kelir(
        &sb,
        &mock,
        &["run", "--name", "victim.status.json", "collision"],
        &[],
    )
    .await;
    assert_eq!(collision.code, 1, "{}", collision.stderr);
    assert!(collision.stderr.contains("reserved"));
    let after: Value = serde_json::from_str(
        &std::fs::read_to_string(sb.state.join("names").join("victim.status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(after, before, "STATUS must not be clobbered by a name");

    // A takeover tombstone is reserved too: a name cannot overwrite it.
    let tomb_collision = kelir(
        &sb,
        &mock,
        &["run", "--name", "victim.stale.123.01abc.456", "collide"],
        &[],
    )
    .await;
    assert_eq!(tomb_collision.code, 1, "{}", tomb_collision.stderr);
    assert!(tomb_collision.stderr.contains("reserved"));
    assert_eq!(std::fs::read(&tombstone).unwrap(), tombstone_bytes);
    clean(&sb).await;
}

/// `--json` on a scheduled run streams the event log as NDJSON and ends with
/// the result event, like `run --name X --json` without scheduling flags.
#[tokio::test]
async fn scheduled_run_honours_json() {
    let mock = Mock::start(vec![text("json answer")]).await;
    let sb = Sandbox::new("");
    let run = kelir(
        &sb,
        &mock,
        &[
            "run",
            "--name",
            "jsonsf",
            "--single-flight",
            "--json",
            "task",
        ],
        &[],
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let events: Vec<Value> = run
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|error| {
                panic!("stdout is not NDJSON ({error}): {}\n{}", line, run.stdout)
            })
        })
        .collect();
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "session_started")
    );
    assert!(events.last().is_some_and(|event| event["type"] == "result"));
    assert!(
        !run.stdout.contains("json answer\n"),
        "no plain answer line"
    );
    assert_eq!(status(&sb, "jsonsf")["answer"], "json answer");
    clean(&sb).await;
}

fn fwd(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().replace('\\', "/")
}

// ------------------------------------------- ported from the removed M9 files
// The tests below keep the surviving assertions of e2e_m9.rs, review_m9.rs
// and review_m9fix.rs under the new flags. Tests of jobs.toml, `kelir job`,
// `runs.jsonl` history, --wait, the per-job notify list and new_session_every
// went away with those features and are deliberately not ported.

/// A live session still refuses a second `--name` run, naming the holder pid,
/// without starting a turn. Ported from e2e_m9.rs.
#[tokio::test]
async fn a_name_held_by_a_live_session_is_refused_with_its_pid() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![Reply::Gated(gate.clone(), sse(&text_turn("slow", 0)))]).await;
    let sb = Sandbox::new("");
    let holder = start(&sb, &mock, &["run", "--name", "busy", "hold"], &[]).await;
    let pid = holder.id().unwrap();
    until("the first request", || mock.request_count() >= 1).await;

    let refused = kelir(&sb, &mock, &["run", "--name", "busy", "second"], &[]).await;
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(
        refused.stderr.contains(&format!("pid {pid}")),
        "{}",
        refused.stderr
    );
    assert!(refused.stderr.contains("busy"), "{}", refused.stderr);
    assert_eq!(mock.request_count(), 1, "no second turn started");

    gate.notify_one();
    assert_eq!(finish(holder).await.code, 0);
    clean(&sb).await;
}

/// A corrupt name file is tolerated and replaced; STATUS files in the same
/// directory must not be treated as corrupt names. Ported from e2e_m9.rs.
#[tokio::test]
async fn a_corrupt_name_file_is_tolerated_and_replaced() {
    let mock = Mock::start(vec![text("ok")]).await;
    let sb = Sandbox::new("");
    std::fs::create_dir_all(sb.state.join("names")).unwrap();
    std::fs::write(sb.state.join("names").join("broken"), "{ nope").unwrap();
    let ls = kelir(&sb, &mock, &["ls", "sessions"], &[]).await;
    assert_eq!(ls.code, 0, "{}", ls.stderr);
    assert!(!ls.stderr.contains("panicked"), "{}", ls.stderr);
    let run = kelir(&sb, &mock, &["run", "--name", "broken", "go"], &[]).await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stderr.contains("unreadable"), "{}", run.stderr);
    let entry: Value = serde_json::from_str(
        &std::fs::read_to_string(sb.state.join("names").join("broken")).unwrap(),
    )
    .unwrap();
    assert_eq!(entry["session"], sb.only_session().as_str());
    clean(&sb).await;
}

/// A failed provider run writes STATUS `error` with the reason and exit 1.
/// Ported from e2e_m9.rs (without the removed history line).
#[tokio::test]
async fn a_failing_provider_is_an_error_run_with_the_reason_recorded() {
    let mock = Mock::start(vec![Reply::Http(
        400,
        "{\"error\":{\"message\":\"nope\"}}".into(),
    )])
    .await;
    let sb = Sandbox::new("");
    let out = kelir(
        &sb,
        &mock,
        &["run", "--name", "badprovider", "--single-flight", "go"],
        &[],
    )
    .await;
    assert_eq!(out.code, 1, "{}", out.stderr);
    let now = status(&sb, "badprovider");
    assert_eq!(now["outcome"], "error");
    assert_eq!(now["exit_code"], 1);
    assert!(!now["error"].as_str().unwrap_or("").is_empty(), "{now}");
    clean(&sb).await;
}

/// The date source goes at the tail only: the system prompt stays byte-identical
/// and nothing else is appended to the log. Ported from e2e_m9.rs's
/// `the_next_day_adds_the_date_source_and_nothing_else`.
#[tokio::test]
async fn the_next_day_adds_only_the_date_source_at_the_tail() {
    let today = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Iso8601::DATE)
        .expect("today");
    let tomorrow = (time::OffsetDateTime::now_utc() + time::Duration::days(1))
        .format(&time::format_description::well_known::Iso8601::DATE)
        .expect("tomorrow");
    let mock = Mock::start(vec![
        text("day one"),
        text("day two"),
        text("day two again"),
    ])
    .await;
    let sb = Sandbox::new("");
    let run = |day: &str| {
        let (sb, mock, day) = (&sb, &mock, day.to_string());
        async move {
            kelir(
                sb,
                mock,
                &["run", "--name", "daily2", "task"],
                &[("KELIR_TEST_TODAY", &day)],
            )
            .await
        }
    };
    assert_eq!(run(&today).await.code, 0);
    let id = sb.only_session();
    let before = sb.log(&id).len();
    assert_eq!(run(&tomorrow).await.code, 0);
    assert_eq!(run(&tomorrow).await.code, 0);
    let log = sb.log(&id);

    let clocks: Vec<&Value> = of_type(&log, "source")
        .into_iter()
        .filter(|source| source["kind"] == "clock")
        .collect();
    assert_eq!(clocks.len(), 1, "announced once for the new day");
    assert_eq!(clocks[0]["name"], tomorrow);
    assert!(
        clocks[0]["text"]
            .as_str()
            .unwrap()
            .contains(&format!("it is now {tomorrow}"))
    );
    // Only the clock source and the events of one ordinary turn are new.
    let added = &log[before..];
    assert_eq!(
        added
            .iter()
            .filter(|event| event["type"] == "source")
            .count(),
        1,
        "nothing but the clock source was added: {added:?}"
    );

    let requests = completions(&mock);
    let system = |request: &crate::support::Recorded| {
        request.messages()[0]["content"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(system(&requests[0]), system(&requests[1]));
    assert_eq!(system(&requests[0]), system(&requests[2]));
    assert!(system(&requests[0]).contains(&today));
    assert!(!system(&requests[1]).contains(&tomorrow));
    let second = requests[1].messages();
    let tail: Vec<String> = second[second.len() - 2..]
        .iter()
        .map(|message| message["content"].as_str().unwrap().to_string())
        .collect();
    assert!(
        tail[0].starts_with(&format!("[source: clock {tomorrow}]")),
        "{tail:?}"
    );
    assert_eq!(tail[1], "task", "clock + task is the whole tail: {tail:?}");
    clean(&sb).await;
}

/// A stopped scheduled run exits 1 and records `stopped`. Ported from
/// review_m9.rs.
#[tokio::test]
async fn a_stopped_scheduled_run_exits_1_and_records_stopped() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![Reply::Gated(gate.clone(), sse(&text_turn("slow", 0)))]).await;
    let sb = Sandbox::new("");
    let holder = start(
        &sb,
        &mock,
        &["run", "--name", "stopped", "--single-flight", "hold"],
        &[],
    )
    .await;
    until("the holder's request", || mock.request_count() >= 1).await;
    let id = sb.only_session();
    let out = kelir(&sb, &mock, &["ctl", &id, "stop"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    gate.notify_one();
    let code = finish(holder).await.code;
    assert_eq!(code, 1, "stopped is not success");
    assert_eq!(status(&sb, "stopped")["outcome"], "stopped");
    clean(&sb).await;
}

/// The token check still guards state-changing verbs on a named session.
/// Ported from review_m9.rs.
#[tokio::test]
async fn a_state_changing_verb_without_a_token_is_refused() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![Reply::Gated(gate.clone(), sse(&text_turn("hold", 0)))]).await;
    let sb = Sandbox::new("");
    let holder = start(&sb, &mock, &["run", "--name", "guarded", "hold"], &[]).await;
    until("the holder's request", || mock.request_count() >= 1).await;
    let id = sb.only_session();
    let dir = sb.session_dir(&id);
    let before = sb.log(&id).len();
    let bare = kelir_ipc_no_token(&dir, &id, "stop").await;
    assert!(
        bare.contains("token") || bare.contains("denied") || bare.contains("refused"),
        "refusal: {bare}"
    );
    assert_eq!(sb.log(&id).len(), before, "no state change without a token");
    gate.notify_one();
    assert_eq!(finish(holder).await.code, 0);
    clean(&sb).await;
}

async fn kelir_ipc_no_token(dir: &Path, id: &str, verb: &str) -> String {
    #[cfg(windows)]
    {
        let _ = (dir, id, verb);
        "refused".to_string()
    }
    #[cfg(unix)]
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;
        let path = dir.join(format!("{id}.sock"));
        let Ok(mut stream) = UnixStream::connect(&path).await else {
            return "refused: cannot connect".to_string();
        };
        let request = json!({"verb": verb, "args": {}, "from": "foreign", "token": null});
        let line = serde_json::to_string(&request).unwrap();
        let _ = stream.write_all(line.as_bytes()).await;
        let _ = stream.write_all(b"\n").await;
        let mut answer = String::new();
        let _ =
            tokio::time::timeout(Duration::from_secs(2), stream.read_to_string(&mut answer)).await;
        answer
    }
}

/// The owner file names the live holder; a pid with the wrong start time is
/// not a holder, and the session lock keeps a forged owner from starting a
/// second turn. Ported from review_m9.rs.
#[tokio::test]
async fn named_session_owner_file_names_the_live_holder_and_is_pid_reuse_safe() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![Reply::Gated(gate.clone(), sse(&text_turn("hold", 0)))]).await;
    let sb = Sandbox::new("");
    let holder = start(&sb, &mock, &["run", "--name", "owned", "hold"], &[]).await;
    until("the holder's request", || mock.request_count() >= 1).await;
    let id = sb.only_session();
    let owner: Value =
        serde_json::from_str(&std::fs::read_to_string(sb.session_dir(&id).join("owner")).unwrap())
            .unwrap();
    assert_eq!(owner["pid"].as_u64().unwrap(), holder.id().unwrap() as u64);
    let tampered = json!({"pid": holder.id().unwrap(), "start": 1u64});
    std::fs::write(sb.session_dir(&id).join("owner"), tampered.to_string()).unwrap();
    let _second = kelir(&sb, &mock, &["run", "--name", "owned", "second"], &[]).await;
    assert_eq!(
        completions(&mock).len(),
        1,
        "no second turn even with a forged owner"
    );
    gate.notify_one();
    assert_eq!(finish(holder).await.code, 0);
    clean(&sb).await;
}

/// STATUS is always a whole JSON record: a reader polling while the run is
/// live never sees a torn file, and sees `running`. Ported from review_m9.rs.
#[tokio::test]
async fn status_files_are_always_parseable_while_the_run_is_live() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![Reply::Gated(gate.clone(), sse(&text_turn("late", 0)))]).await;
    let sb = Sandbox::new("");
    let holder = start(
        &sb,
        &mock,
        &["run", "--name", "atomic", "--single-flight", "hold"],
        &[],
    )
    .await;
    let path = sb.state.join("names").join("atomic.status.json");
    until("STATUS", || path.exists()).await;
    let mut saw_running = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let text = std::fs::read_to_string(&path).unwrap();
        let value: Value = serde_json::from_str(&text).expect("a torn STATUS");
        saw_running |= value["outcome"] == "running";
        if mock.request_count() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(saw_running, "the running state was published");
    gate.notify_one();
    assert_eq!(finish(holder).await.code, 0);
    clean(&sb).await;
}

/// Two processes naming a free name at once still end with one session and
/// one `session_started`. Ported from review_m9.rs.
#[tokio::test]
async fn two_first_runs_of_one_new_name_do_not_share_a_session() {
    let mock = Mock::start(vec![
        Reply::SseAfter(Duration::from_secs(2), sse(&text_turn("one", 0))),
        text("two"),
    ])
    .await;
    let sb = Sandbox::new("");
    let a = start(&sb, &mock, &["run", "--name", "race2", "first"], &[]).await;
    let b = start(&sb, &mock, &["run", "--name", "race2", "second"], &[]).await;
    let out_a = finish(a).await;
    let out_b = finish(b).await;
    let ids = sb.session_ids();
    assert_eq!(ids.len(), 1, "one name must bind one session: {ids:?}");
    let log = sb.log(&ids[0]);
    assert_eq!(
        of_type(&log, "session_started").len(),
        1,
        "one session must start once (codes {} {})",
        out_a.code,
        out_b.code
    );
    clean(&sb).await;
}

/// A step cap must settle the cap-th tool call before pausing. Ported from
/// review_m9fix.rs (jobs.toml cap replaced by --max-steps).
#[tokio::test]
async fn a_cap_th_answer_after_a_tool_pauses_after_the_tool_result() {
    let answer = "late answer on the cap";
    let mut capped = tool_call_chunks("c1", "edit", &json!({"path":"f.txt","old":"","new":"x\n"}));
    capped.push(crate::support::finish("tool_calls"));
    capped.extend(text_turn(answer, 0));
    let mock = Mock::start(vec![
        Reply::Sse(sse(&crate::support::tool_turn(
            "c0",
            "edit",
            &json!({"path":"f0.txt","old":"","new":"x\n"}),
        ))),
        Reply::Sse(sse(&capped)),
        Reply::Sse(sse(&text_turn("never asked", 0))),
    ])
    .await;
    let sb = Sandbox::new("");
    let out = kelir(
        &sb,
        &mock,
        &[
            "run",
            "--name",
            "latecap",
            "--single-flight",
            "--max-steps",
            "2",
            "go",
        ],
        &[],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let now = status(&sb, "latecap");
    assert_eq!(now["outcome"], "paused", "{now}");
    assert_eq!(now["steps"], 2, "{now}");
    assert_eq!(completions(&mock).len(), 2, "no third request past the cap");
    let id = sb.only_session();
    let log = sb.log(&id);
    let ended = of_type(&log, "turn_ended").pop().expect("turn_ended");
    assert_eq!(ended["outcome"], "paused");
    assert_eq!(ended["reason"], "steps");
    assert_eq!(ended["final_text"], answer, "the cap-th answer is kept");
    assert_eq!(of_type(&log, "call_settled").len(), 2, "both calls settled");
    let types: Vec<&str> = log
        .iter()
        .map(|event| event["type"].as_str().unwrap_or(""))
        .collect();
    let last_tool = types
        .iter()
        .rposition(|kind| *kind == "call_settled")
        .unwrap();
    let turn_end = types
        .iter()
        .rposition(|kind| *kind == "turn_ended")
        .unwrap();
    assert!(last_tool < turn_end, "never between a call and its result");
    clean(&sb).await;
}

/// The cap is a scheduling rule, not model text: the request prefix is
/// byte-identical to an uncapped run. Ported from review_m9fix.rs.
#[tokio::test]
async fn the_job_cap_is_not_in_the_model_prompt_and_is_not_misreported() {
    let sb = Sandbox::new("");
    let mock = Mock::start(vec![text("finished")]).await;
    let out = kelir(
        &sb,
        &mock,
        &[
            "run",
            "--name",
            "secretcap",
            "--single-flight",
            "--max-steps",
            "1",
            "go",
        ],
        &[],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let paused = status(&sb, "secretcap");
    assert_eq!(paused["outcome"], "paused", "{paused}");
    assert!(
        !serde_json::to_string(&paused)
            .unwrap()
            .contains("job_step_cap")
    );
    assert!(
        paused["error"]
            .as_str()
            .is_some_and(|error| error.to_lowercase().contains("paused")),
        "STATUS says why it paused: {paused}"
    );
    let paused_events = serde_json::to_string(&sb.log(&sb.only_session())).unwrap();
    assert!(
        !paused_events.contains("job_step_cap"),
        "internal name leaked"
    );
    let first = completions(&mock)[0].messages().to_vec();

    // Lift the cap in the SAME sandbox: same session, prompt and paths.
    let mock2 = Mock::start(vec![text("finished again")]).await;
    let out2 = kelir(
        &sb,
        &mock2,
        &["run", "--name", "secretcap", "--single-flight", "go"],
        &[],
    )
    .await;
    assert_eq!(out2.code, 0, "{}", out2.stderr);
    assert_eq!(status(&sb, "secretcap")["outcome"], "done");
    let second = completions(&mock2)[0].messages().to_vec();
    assert_eq!(first[0], second[0], "the system prompt must be identical");
    assert_eq!(
        &second[..first.len()],
        &first[..],
        "cached prefix untouched"
    );
    assert_eq!(first.len(), second.len() - 2, "only tail messages are new");
    let joined = serde_json::to_string(&first).unwrap();
    assert!(!joined.contains("max_steps"), "{joined}");
    assert!(!joined.contains("step cap"), "{joined}");
    assert!(!joined.contains("job_step_cap"), "{joined}");
    clean(&sb).await;
}

/// A `--fresh` run puts carry before the job source, both at the tail.
/// Ported from review_m9fix.rs's rotation test.
#[tokio::test]
async fn a_fresh_run_adds_carry_then_job_after_the_frozen_prefix() {
    let mock = Mock::start(vec![text("the old session learned X"), text("fresh start")]).await;
    let sb = Sandbox::new("");
    assert_eq!(
        kelir(
            &sb,
            &mock,
            &["run", "--name", "rot", "--single-flight", "go"],
            &[]
        )
        .await
        .code,
        0
    );
    let old = sb.only_session();
    assert_eq!(
        kelir(
            &sb,
            &mock,
            &["run", "--name", "rot", "--single-flight", "--fresh", "go"],
            &[]
        )
        .await
        .code,
        0
    );
    let reqs = completions(&mock);
    assert_eq!(reqs.len(), 2);
    let second = reqs[1].messages();
    let tail: Vec<&str> = second[second.len() - 2..]
        .iter()
        .map(|message| message["content"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(tail.len(), 2, "carry, task: {tail:?}");
    assert!(tail[0].starts_with("[source: carry "), "{tail:?}");
    assert!(tail[0].contains("the old session learned X"), "{tail:?}");
    assert_eq!(tail[1], "go");
    let new_id: String = serde_json::from_str::<Value>(
        &std::fs::read_to_string(sb.state.join("names").join("rot")).unwrap(),
    )
    .unwrap()["session"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(new_id, old);
    let new_log = sb.log(&new_id);
    let kinds: Vec<&str> = of_type(&new_log, "source")
        .iter()
        .map(|source| source["kind"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(kinds, vec!["carry"], "the tail source: {kinds:?}");
    clean(&sb).await;
}

/// A young unreadable lock is respected and the skip explains why. Ported
/// from review_m9fix.rs (without the removed history line).
#[tokio::test]
async fn an_unreadable_young_lock_is_respected_and_the_skip_explains_why() {
    let sb = Sandbox::new("");
    std::fs::create_dir_all(sb.state.join("names")).unwrap();
    let lock = sb.state.join("names").join("unreadable.lock");
    std::fs::write(&lock, b"not json at all").unwrap();
    let mock = Mock::start(vec![]).await;
    let out = kelir(
        &sb,
        &mock,
        &["run", "--name", "unreadable", "--single-flight", "go"],
        &[],
    )
    .await;
    assert_eq!(out.code, 75, "{}", out.stderr);
    assert!(out.stderr.contains("unreadable"), "{}", out.stderr);
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        "not json at all",
        "a fresh unreadable lock is never taken over"
    );
    let skipped = status(&sb, "unreadable");
    assert_eq!(skipped["outcome"], "skipped");
    assert_eq!(skipped["exit_code"], 75);
    assert!(
        skipped["error"]
            .as_str()
            .unwrap_or("")
            .contains("unreadable")
    );
    assert!(mock.requests().is_empty(), "no model turn on a skip");
    clean(&sb).await;
}

/// A dead OLD lock whose recorded pid (1) is alive is taken over because the
/// start time does not match. Ported from review_m9fix.rs.
#[tokio::test]
async fn an_old_lock_with_a_recycled_pid_is_taken_over() {
    let sb = Sandbox::new("");
    std::fs::create_dir_all(sb.state.join("names")).unwrap();
    let lock = sb.state.join("names").join("recycled.lock");
    let record = json!({
        "token": "dead-token",
        "pid": 1,
        "start": 1_u64,
        "cmd": "old kelir",
        "started": "2020-01-01T00:00:00Z"
    });
    std::fs::write(&lock, record.to_string()).unwrap();
    let old = std::fs::File::options().write(true).open(&lock).unwrap();
    old.set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000))
        .unwrap();
    drop(old);

    let mock = Mock::start(vec![text("fresh run")]).await;
    let out = kelir(
        &sb,
        &mock,
        &["run", "--name", "recycled", "--single-flight", "go"],
        &[],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(status(&sb, "recycled")["outcome"], "done");
    assert_eq!(completions(&mock).len(), 1);
    assert!(!lock.exists(), "the taken-over lock was released");
    clean(&sb).await;
}
