//! M9 end-to-end: named sessions, jobs, single-flight and STATUS, against the
//! real binary and a scripted mock provider.
//!
//! Every process goes through the `Sandbox` Drop-guard (a Job Object on
//! Windows), every test ends with `clean()` (nothing of ours is left), and the
//! whole file is capped at `SPAWN_CAP` spawned processes by a counter: an
//! incident (about 1000 runaway `sh.exe` from a sibling job's tests) is why.

mod support;

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::*;

const SPAWN_CAP: usize = 120;
static SPAWNS: AtomicUsize = AtomicUsize::new(0);

fn count_spawn() {
    let n = SPAWNS.fetch_add(1, Ordering::SeqCst) + 1;
    assert!(n <= SPAWN_CAP, "this file spawned {n} processes: a runaway");
}

fn fwd(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().replace('\\', "/")
}

fn text(answer: &str) -> Reply {
    Reply::Sse(sse(&text_turn(answer, 0)))
}

fn jobs_toml(sb: &Sandbox, body: &str) {
    let dir = sb.state.join("no-config");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("jobs.toml"), body).unwrap();
}

async fn kelir(sb: &Sandbox, mock: &Mock, args: &[&str], env: &[(&str, &str)]) -> Output {
    count_spawn();
    sb.kelir(&mock.base_url(), args, env).await
}

/// A kelir that keeps running while the test looks at it.
async fn start(
    sb: &Sandbox,
    mock: &Mock,
    args: &[&str],
    env: &[(&str, &str)],
) -> tokio::process::Child {
    count_spawn();
    let mut std_cmd = sb.kelir_command(&mock.base_url());
    std_cmd
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        std_cmd.env(key, value);
    }
    let mut command = tokio::process::Command::from(std_cmd);
    command.kill_on_drop(true);
    sb.guard.spawn(&mut command).await.expect("spawn kelir")
}

async fn finish(child: tokio::process::Child) -> Output {
    let output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
        .await
        .expect("kelir did not finish within 60 s")
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

fn job_dir(sb: &Sandbox, job: &str) -> std::path::PathBuf {
    sb.state.join("jobs").join(job)
}

fn status(sb: &Sandbox, job: &str) -> Value {
    let text = std::fs::read_to_string(job_dir(sb, job).join("STATUS.json"))
        .unwrap_or_else(|error| panic!("no STATUS.json for {job}: {error}"));
    serde_json::from_str(&text).unwrap()
}

fn runs(sb: &Sandbox, job: &str) -> Vec<Value> {
    std::fs::read_to_string(job_dir(sb, job).join("runs.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn completions(mock: &Mock) -> Vec<Recorded> {
    mock.requests()
        .into_iter()
        .filter(|request| request.method == "POST")
        .collect()
}

fn of_type<'a>(log: &'a [Value], kind: &str) -> Vec<&'a Value> {
    log.iter().filter(|event| event["type"] == kind).collect()
}

/// Nothing of this test is left running.
async fn clean(sb: &Sandbox) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while sb.guard.active() > 0 {
        assert!(
            Instant::now() < deadline,
            "{} process(es) of the test are still alive",
            sb.guard.active()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

const NIGHTLY: &str =
    "[jobs.nightly]\ntask = \"do the nightly thing\"\nschedule = \"every night\"\n";

// ------------------------------------------------------------ named sessions

#[tokio::test]
async fn a_name_creates_a_session_then_resumes_it_with_the_task_from_a_job() {
    let mock = Mock::start(vec![text("first answer"), text("second answer")]).await;
    let sb = Sandbox::new("");
    let one = kelir(&sb, &mock, &["run", "--name", "daily", "task one"], &[]).await;
    assert_eq!(one.code, 0, "{}", one.stderr);
    let two = kelir(&sb, &mock, &["run", "--name", "daily", "task two"], &[]).await;
    assert_eq!(two.code, 0, "{}", two.stderr);

    let id = sb.only_session();
    let entry: Value = serde_json::from_str(
        &std::fs::read_to_string(sb.state.join("names").join("daily")).unwrap(),
    )
    .unwrap();
    assert_eq!(entry["session"], id.as_str());
    let log = sb.log(&id);
    let users = of_type(&log, "user");
    assert_eq!(users.len(), 2);
    assert_eq!(
        users[0]["from"], "cli",
        "the first task creates the session"
    );
    assert_eq!(users[1]["from"], "job", "a resumed task is a job's");
    assert_eq!(users[1]["text"], "task two");

    // The second request keeps the first one's whole prefix (the cache).
    let requests = completions(&mock);
    assert_eq!(requests.len(), 2);
    let (first, second) = (requests[0].messages(), requests[1].messages());
    assert_eq!(&second[..first.len()], first);
    assert_eq!(second[first.len()]["role"], "assistant");
    assert_eq!(second.last().unwrap()["content"], "task two");
    clean(&sb).await;
}

#[tokio::test]
async fn sessions_ls_shows_names_and_rename_and_forget_keep_the_session() {
    let mock = Mock::start(vec![text("a"), text("b")]).await;
    let sb = Sandbox::new("");
    assert_eq!(
        kelir(&sb, &mock, &["run", "--name", "alpha", "x"], &[])
            .await
            .code,
        0
    );
    let id = sb.only_session();

    let ls = kelir(&sb, &mock, &["sessions", "ls", "--json"], &[]).await;
    assert_eq!(ls.code, 0, "{}", ls.stderr);
    let rows: Vec<Value> = serde_json::from_str(ls.stdout.trim()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "alpha");
    assert_eq!(rows[0]["session"], id.as_str());
    assert_eq!(rows[0]["state"], "done");
    assert!(rows[0]["last_activity"].as_str().unwrap().starts_with("20"));
    assert!(rows[0].get("cost_usd").is_some());
    let table = kelir(&sb, &mock, &["sessions", "ls"], &[]).await;
    assert!(table.stdout.contains("alpha") && table.stdout.contains("NAME"));

    assert_eq!(
        kelir(&sb, &mock, &["sessions", "rename", "alpha", "beta"], &[])
            .await
            .code,
        0
    );
    assert!(!sb.state.join("names").join("alpha").exists());
    // The new name resumes the same session.
    assert_eq!(
        kelir(&sb, &mock, &["run", "--name", "beta", "again"], &[])
            .await
            .code,
        0
    );
    assert_eq!(sb.session_ids().len(), 1);
    // A taken name, a missing one and a bad one are refused.
    assert_eq!(
        kelir(&sb, &mock, &["sessions", "rename", "beta", "beta2"], &[])
            .await
            .code,
        0
    );
    assert_eq!(
        kelir(&sb, &mock, &["sessions", "rename", "nope", "x"], &[])
            .await
            .code,
        1
    );
    assert_eq!(
        kelir(
            &sb,
            &mock,
            &["sessions", "rename", "beta2", "Bad Name"],
            &[]
        )
        .await
        .code,
        1
    );

    assert_eq!(
        kelir(&sb, &mock, &["sessions", "forget", "beta2"], &[])
            .await
            .code,
        0
    );
    assert_eq!(
        kelir(&sb, &mock, &["sessions", "forget", "beta2"], &[])
            .await
            .code,
        1
    );
    assert_eq!(sb.session_ids().len(), 1, "the session and its log stay");
    let ls = kelir(&sb, &mock, &["sessions", "ls", "--json"], &[]).await;
    let rows: Vec<Value> = serde_json::from_str(ls.stdout.trim()).unwrap();
    assert_eq!(rows[0]["name"], Value::Null);
    clean(&sb).await;
}

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
    assert_eq!(completions(&mock).len(), 1, "no second turn started");

    gate.notify_one();
    assert_eq!(finish(holder).await.code, 0);
    clean(&sb).await;
}

#[tokio::test]
async fn a_corrupt_name_file_is_tolerated_and_replaced() {
    let mock = Mock::start(vec![text("ok")]).await;
    let sb = Sandbox::new("");
    std::fs::create_dir_all(sb.state.join("names")).unwrap();
    std::fs::write(sb.state.join("names").join("broken"), "{ nope").unwrap();
    let ls = kelir(&sb, &mock, &["sessions", "ls"], &[]).await;
    assert_eq!(ls.code, 0, "{}", ls.stderr);
    assert!(!ls.stderr.contains("panicked"), "{}", ls.stderr);
    let run = kelir(&sb, &mock, &["run", "--name", "broken", "go"], &[]).await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stderr.contains("unreadable"),
        "a warning, not silence: {}",
        run.stderr
    );
    let entry: Value = serde_json::from_str(
        &std::fs::read_to_string(sb.state.join("names").join("broken")).unwrap(),
    )
    .unwrap();
    assert_eq!(entry["session"], sb.only_session().as_str());
    clean(&sb).await;
}

// -------------------------------------------------------------------- jobs

#[tokio::test]
async fn a_job_run_twice_resumes_one_session_and_status_describes_each_run() {
    let mock = Mock::start(vec![text("all quiet"), text("still quiet")]).await;
    let sb = Sandbox::new("");
    jobs_toml(&sb, NIGHTLY);
    let first = kelir(&sb, &mock, &["job", "run", "nightly"], &[]).await;
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(
        first.stdout.contains("all quiet"),
        "the answer is stdout: {}",
        first.stdout
    );
    let first_status = status(&sb, "nightly");
    let second = kelir(&sb, &mock, &["job", "run", "nightly"], &[]).await;
    assert_eq!(second.code, 0, "{}", second.stderr);

    // One session, resumed.
    let id = sb.only_session();
    let log = sb.log(&id);
    assert_eq!(of_type(&log, "session_started").len(), 1);
    assert_eq!(of_type(&log, "resumed").len(), 1);
    let users = of_type(&log, "user");
    assert_eq!(users.len(), 2);
    assert!(users.iter().all(|user| user["from"] == "job"));
    assert_eq!(users[1]["text"], "do the nightly thing");

    // Prefix-stable: the second request extends the first.
    let requests = completions(&mock);
    let (a, b) = (requests[0].messages(), requests[1].messages());
    assert_eq!(&b[..a.len()], a);
    // One job line per run, a source, never a rewrite of the base prompt.
    let job_lines: Vec<&Value> = of_type(&log, "source")
        .into_iter()
        .filter(|source| source["kind"] == "job")
        .collect();
    assert_eq!(job_lines.len(), 2);
    let line = job_lines[1]["text"].as_str().unwrap();
    assert!(
        line.contains("STATUS.json") && line.contains("notes"),
        "{line}"
    );

    // STATUS: machine and human, atomic, complete.
    let now = status(&sb, "nightly");
    assert_eq!(now["job"], "nightly");
    assert_eq!(now["outcome"], "done");
    assert_eq!(now["exit_code"], 0);
    assert_eq!(now["session"], id.as_str());
    assert_eq!(now["session_name"], "nightly");
    assert_eq!(now["schedule"], "every night");
    assert_eq!(now["steps"], 1);
    assert_eq!(now["answer"], "still quiet");
    assert_ne!(now["run"], first_status["run"]);
    assert!(now["tokens"]["prompt"].as_u64().unwrap() > 0);
    assert!(now["cache_hit_ratio"].is_number() || now["cache_hit_ratio"].is_null());
    assert!(now["started"].as_str().unwrap() <= now["finished"].as_str().unwrap());
    let md = std::fs::read_to_string(job_dir(&sb, "nightly").join("STATUS.md")).unwrap();
    assert!(
        md.contains("**done**") && md.contains("still quiet"),
        "{md}"
    );
    assert!(job_dir(&sb, "nightly").join("notes").is_dir());

    // History is append-only: two runs, in order.
    let history = runs(&sb, "nightly");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["run"], first_status["run"]);
    assert_eq!(history[1]["outcome"], "done");

    // The CLI reads them back.
    let shown = kelir(&sb, &mock, &["job", "status", "nightly", "--json"], &[]).await;
    assert_eq!(shown.code, 0, "{}", shown.stderr);
    let shown: Value = serde_json::from_str(shown.stdout.trim()).unwrap();
    assert_eq!(shown["outcome"], "done");
    assert_eq!(shown["live"], false);
    assert!(shown["paths"]["notes"].as_str().unwrap().ends_with("notes"));
    let all = kelir(&sb, &mock, &["job", "status"], &[]).await;
    assert!(
        all.stdout.contains("nightly") && all.stdout.contains("done"),
        "{}",
        all.stdout
    );
    let hist = kelir(
        &sb,
        &mock,
        &["job", "history", "nightly", "-n", "1", "--json"],
        &[],
    )
    .await;
    let hist: Vec<Value> = serde_json::from_str(hist.stdout.trim()).unwrap();
    assert_eq!(hist.len(), 1);
    assert_eq!(hist[0]["run"], now["run"]);
    clean(&sb).await;
}

#[tokio::test]
async fn the_next_day_adds_the_date_source_and_nothing_else() {
    let mock = Mock::start(vec![
        text("day one"),
        text("day two"),
        text("day two again"),
    ])
    .await;
    let sb = Sandbox::new("");
    jobs_toml(&sb, NIGHTLY);
    let run = |day: &'static str| {
        let (sb, mock) = (&sb, &mock);
        async move {
            kelir(
                sb,
                mock,
                &["job", "run", "nightly"],
                &[("KELIR_TODAY", day)],
            )
            .await
        }
    };
    assert_eq!(run("2026-11-20").await.code, 0);
    let id = sb.only_session();
    let before = sb.log(&id).len();
    assert_eq!(run("2026-11-21").await.code, 0);
    assert_eq!(run("2026-11-21").await.code, 0);
    let log = sb.log(&id);

    let clocks: Vec<&Value> = of_type(&log, "source")
        .into_iter()
        .filter(|source| source["kind"] == "clock")
        .collect();
    assert_eq!(
        clocks.len(),
        1,
        "announced once for the new day, not again the same day"
    );
    assert_eq!(clocks[0]["name"], "2026-11-21");
    assert!(
        clocks[0]["text"]
            .as_str()
            .unwrap()
            .contains("it is now 2026-11-21")
    );

    // The frozen system prompt is untouched and carries the first day.
    let requests = completions(&mock);
    let system = |request: &Recorded| {
        request.messages()[0]["content"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(system(&requests[0]), system(&requests[1]));
    assert_eq!(system(&requests[0]), system(&requests[2]));
    assert!(system(&requests[0]).contains("2026-11-20"));
    assert!(!system(&requests[1]).contains("2026-11-21"));
    // The tail of the second day's request is: the date, the job line, the task.
    let second = requests[1].messages();
    let tail: Vec<String> = second[second.len() - 3..]
        .iter()
        .map(|message| message["content"].as_str().unwrap().to_string())
        .collect();
    assert!(
        tail[0].starts_with("[source: clock 2026-11-21]"),
        "{tail:?}"
    );
    assert!(tail[1].starts_with("[source: job nightly]"), "{tail:?}");
    assert_eq!(tail[2], "do the nightly thing");
    // The first request is a strict prefix of the second.
    let first = requests[0].messages();
    assert_eq!(&second[..first.len()], first);
    // Same day again: only the job line and the task are new.
    let third = requests[2].messages();
    assert!(
        third[third.len() - 2]["content"]
            .as_str()
            .unwrap()
            .starts_with("[source: job")
    );
    assert!(log.len() > before);
    clean(&sb).await;
}

#[tokio::test]
async fn a_session_older_than_new_session_every_is_replaced_and_carries_a_summary() {
    let mock = Mock::start(vec![text("the first run learned X"), text("fresh start")]).await;
    let sb = Sandbox::new("");
    jobs_toml(
        &sb,
        "[jobs.rot]\ntask = \"go\"\nnew_session_every = \"1s\"\nsession = \"rotating\"\n",
    );
    assert_eq!(kelir(&sb, &mock, &["job", "run", "rot"], &[]).await.code, 0);
    let old = sb.only_session();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(kelir(&sb, &mock, &["job", "run", "rot"], &[]).await.code, 0);

    let ids = sb.session_ids();
    assert_eq!(ids.len(), 2, "{ids:?}");
    let entry: Value = serde_json::from_str(
        &std::fs::read_to_string(sb.state.join("names").join("rotating")).unwrap(),
    )
    .unwrap();
    let fresh = entry["session"].as_str().unwrap().to_string();
    assert_ne!(fresh, old);
    assert_eq!(status(&sb, "rot")["session"], fresh.as_str());
    // The new session starts with the old one's last word as a source.
    let second = completions(&mock)[1].messages().to_vec();
    let carried = second
        .iter()
        .filter_map(|message| message["content"].as_str())
        .find(|content| content.starts_with("[source: carry"))
        .expect("a carry source");
    assert!(
        carried.contains("the first run learned X") && carried.contains(&old),
        "{carried}"
    );
    clean(&sb).await;
}

#[tokio::test]
async fn a_failing_provider_is_an_error_run_with_the_reason_recorded() {
    let mock = Mock::start(vec![Reply::Http(
        400,
        "{\"error\":{\"message\":\"nope\"}}".into(),
    )])
    .await;
    let sb = Sandbox::new("");
    jobs_toml(&sb, NIGHTLY);
    let out = kelir(&sb, &mock, &["job", "run", "nightly"], &[]).await;
    assert_eq!(out.code, 1, "{}", out.stderr);
    let now = status(&sb, "nightly");
    assert_eq!(now["outcome"], "error");
    assert_eq!(now["exit_code"], 1);
    assert!(!now["error"].as_str().unwrap_or("").is_empty(), "{now}");
    assert_eq!(runs(&sb, "nightly")[0]["outcome"], "error");
    clean(&sb).await;
}

#[tokio::test]
async fn usage_errors_exit_2_and_leave_no_lock_behind() {
    let mock = Mock::start(vec![]).await;
    let sb = Sandbox::new("");
    // No jobs.toml at all, then an unknown job, then a broken file.
    let none = kelir(&sb, &mock, &["job", "run", "ghost"], &[]).await;
    assert_eq!(none.code, 2);
    assert!(
        none.stderr.contains("no job named `ghost`"),
        "{}",
        none.stderr
    );
    jobs_toml(&sb, NIGHTLY);
    assert_eq!(
        kelir(&sb, &mock, &["job", "run", "ghost"], &[]).await.code,
        2
    );
    assert_eq!(
        kelir(&sb, &mock, &["job", "run", "Bad Name"], &[])
            .await
            .code,
        2
    );
    jobs_toml(&sb, "[jobs.x]\ntask = \"a\"\ntypo = 1\n");
    let broken = kelir(&sb, &mock, &["job", "run", "x"], &[]).await;
    assert_eq!(broken.code, 2);
    assert!(broken.stderr.contains("typo"), "{}", broken.stderr);
    assert!(!sb.state.join("jobs").join("x").join("lock").exists());
    assert_eq!(completions(&mock).len(), 0);
    clean(&sb).await;
}

// ------------------------------------------------------------ single-flight

#[tokio::test]
async fn two_job_runs_at_once_run_exactly_one_turn_and_the_other_exits_75() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![Reply::Gated(gate.clone(), sse(&text_turn("slow", 0)))]).await;
    let sb = Sandbox::new("");
    jobs_toml(&sb, NIGHTLY);
    let holder = start(&sb, &mock, &["job", "run", "nightly"], &[]).await;
    until("the holder's request", || mock.request_count() >= 1).await;
    assert_eq!(status(&sb, "nightly")["outcome"], "running");

    let began = Instant::now();
    let skipped = kelir(&sb, &mock, &["job", "run", "nightly"], &[]).await;
    assert_eq!(skipped.code, 75, "{}", skipped.stderr);
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "it must not wait"
    );
    assert!(
        skipped.stderr.contains("already running"),
        "{}",
        skipped.stderr
    );

    // The holder's STATUS was not clobbered, and the skip is in the history.
    assert_eq!(status(&sb, "nightly")["outcome"], "running");
    let history = runs(&sb, "nightly");
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0]["outcome"], "skipped");
    assert_eq!(history[0]["exit_code"], 75);
    assert!(history[0]["holder_pid"].is_number());
    assert_eq!(
        completions(&mock).len(),
        1,
        "never a second concurrent turn"
    );

    gate.notify_one();
    assert_eq!(finish(holder).await.code, 0);
    assert_eq!(status(&sb, "nightly")["outcome"], "done");
    assert_eq!(runs(&sb, "nightly").len(), 2);
    assert_eq!(mock.max_concurrent(), 1);
    clean(&sb).await;
}

#[tokio::test]
async fn wait_queues_behind_the_holder_instead_of_skipping() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Gated(gate.clone(), sse(&text_turn("first", 0))),
        text("second"),
    ])
    .await;
    let sb = Sandbox::new("");
    jobs_toml(&sb, NIGHTLY);
    let holder = start(&sb, &mock, &["job", "run", "nightly"], &[]).await;
    until("the holder's request", || mock.request_count() >= 1).await;
    let waiter = start(&sb, &mock, &["job", "run", "nightly", "--wait", "40"], &[]).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        completions(&mock).len(),
        1,
        "the waiter has not started a turn"
    );
    gate.notify_one();
    assert_eq!(finish(holder).await.code, 0);
    assert_eq!(finish(waiter).await.code, 0);
    assert_eq!(completions(&mock).len(), 2);
    assert_eq!(mock.max_concurrent(), 1, "never two turns at once");
    let outcomes: Vec<String> = runs(&sb, "nightly")
        .iter()
        .map(|run| run["outcome"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(outcomes, vec!["done", "done"]);
    clean(&sb).await;
}

#[tokio::test]
async fn kill_9_of_the_holder_frees_the_lock_and_the_dead_run_is_recorded() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Gated(gate.clone(), sse(&text_turn("never", 0))),
        text("recovered"),
    ])
    .await;
    let sb = Sandbox::new("");
    jobs_toml(&sb, NIGHTLY);
    let mut holder = start(&sb, &mock, &["job", "run", "nightly"], &[]).await;
    until("the holder's request", || mock.request_count() >= 1).await;
    holder.kill().await.unwrap();
    let _ = holder.wait().await;

    // The corpse's STATUS still says running, and `status` says it is not.
    assert_eq!(status(&sb, "nightly")["outcome"], "running");
    let shown = kelir(&sb, &mock, &["job", "status", "nightly", "--json"], &[]).await;
    let shown: Value = serde_json::from_str(shown.stdout.trim()).unwrap();
    assert_eq!(shown["live"], false);
    assert_eq!(shown["stale"], true);

    tokio::time::sleep(Duration::from_millis(1200)).await; // the lock is no longer young
    let again = kelir(&sb, &mock, &["job", "run", "nightly"], &[]).await;
    assert_eq!(
        again.code, 0,
        "the dead holder's lock must be taken over: {}",
        again.stderr
    );
    assert_eq!(status(&sb, "nightly")["outcome"], "done");
    let history = runs(&sb, "nightly");
    assert!(
        history.iter().any(|run| run["outcome"] == "error"
            && run["error"].as_str().unwrap_or("").contains("disappeared")),
        "{history:?}"
    );
    gate.notify_one();
    clean(&sb).await;
}

// ------------------------------------------------------------------ timeout

#[tokio::test]
async fn the_timeout_stops_the_turn_exits_124_and_kills_the_tools_tree() {
    let marker = tempfile::tempdir().unwrap();
    let pid_file = marker.path().join("pid");
    let command = format!("echo $$ > \"{}\"; sleep 300", fwd(&pid_file));
    let mock = Mock::start(vec![Reply::Sse(sse(&tool_turn(
        "c1",
        "bash",
        &json!({"command": command}),
    )))])
    .await;
    let sb = Sandbox::new("");
    jobs_toml(&sb, "[jobs.slow]\ntask = \"sleep\"\ntimeout = \"3s\"\n");
    let began = Instant::now();
    let out = kelir(&sb, &mock, &["job", "run", "slow"], &[]).await;
    assert_eq!(out.code, 124, "stdout {} stderr {}", out.stdout, out.stderr);
    assert!(
        began.elapsed() < Duration::from_secs(45),
        "{:?}",
        began.elapsed()
    );

    let now = status(&sb, "slow");
    assert_eq!(now["outcome"], "timeout");
    assert_eq!(now["exit_code"], 124);
    assert!(now["error"].as_str().unwrap().contains("timeout"), "{now}");
    assert_eq!(runs(&sb, "slow").last().unwrap()["outcome"], "timeout");
    assert!(
        !job_dir(&sb, "slow").join("lock").exists(),
        "the lock is released"
    );

    // The tool call's tree is gone: nothing of the test is alive, and the
    // shell that wrote its pid is not either.
    clean(&sb).await;
    #[cfg(unix)]
    if let Some(pid) = std::fs::read_to_string(&pid_file)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
    {
        assert!(
            !kelir::child::process_alive(pid, None),
            "pid {pid} survived the timeout"
        );
    }
    #[cfg(not(unix))]
    let _ = &pid_file;
}

// ------------------------------------------------------------------- notify

#[tokio::test]
async fn the_notify_hook_runs_with_a_scrubbed_environment_and_its_failure_changes_nothing() {
    let out_dir = tempfile::tempdir().unwrap();
    let env_file = out_dir.path().join("env.txt");
    let mock = Mock::start(vec![text("done one"), text("done two")]).await;
    let sb = Sandbox::new("");
    jobs_toml(
        &sb,
        &format!(
            "[jobs.hooked]\ntask = \"t\"\nnotify = \"env > '{}'\"\n\
             [jobs.badhook]\ntask = \"t\"\nnotify = [\"exit 3\"]\n",
            fwd(&env_file)
        ),
    );
    let ran = kelir(&sb, &mock, &["job", "run", "hooked"], &[]).await;
    assert_eq!(ran.code, 0, "{}", ran.stderr);
    let env = std::fs::read_to_string(&env_file).expect("the hook ran");
    assert!(env.contains("KELIR_JOB=hooked"), "{env}");
    assert!(env.contains("KELIR_JOB_OUTCOME=done"), "{env}");
    assert!(env.contains("KELIR_JOB_STATUS="), "{env}");
    assert!(env.contains("KELIR_JOB_RUN=run_"), "{env}");
    for secret in [
        "OPENROUTER_API_KEY",
        "KELIR_GO_API_KEY",
        "KELIR_TOKEN",
        "KELIR_SOCK",
        "test-openrouter-key",
        "test-go-key",
    ] {
        assert!(!env.contains(secret), "the hook saw {secret}");
    }

    let bad = kelir(&sb, &mock, &["job", "run", "badhook"], &[]).await;
    assert_eq!(
        bad.code, 0,
        "a failing hook never changes the exit code: {}",
        bad.stderr
    );
    let now = status(&sb, "badhook");
    assert_eq!(now["outcome"], "done");
    assert!(
        now["notify_errors"][0]
            .as_str()
            .unwrap()
            .contains("exited with 3"),
        "{now}"
    );
    clean(&sb).await;
}

#[tokio::test]
async fn a_hook_that_hangs_is_killed_at_its_timeout_and_the_job_still_exits_normally() {
    let mock = Mock::start(vec![text("ok")]).await;
    let sb = Sandbox::new("");
    jobs_toml(&sb, "[jobs.hang]\ntask = \"t\"\nnotify = \"sleep 120\"\n");
    let began = Instant::now();
    let out = kelir(
        &sb,
        &mock,
        &["job", "run", "hang"],
        &[("KELIR_NOTIFY_TIMEOUT_MS", "700")],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(began.elapsed() < Duration::from_secs(40));
    let now = status(&sb, "hang");
    assert!(
        now["notify_errors"][0]
            .as_str()
            .unwrap()
            .contains("timed out"),
        "{now}"
    );
    clean(&sb).await;
}

#[tokio::test]
async fn a_projects_notify_hook_does_not_run_until_the_project_is_trusted() {
    let out_dir = tempfile::tempdir().unwrap();
    let flag = out_dir.path().join("ran");
    let mock = Mock::start(vec![text("a"), text("b")]).await;
    let sb = Sandbox::new("");
    let project = sb.cwd.join(".kelir");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("jobs.toml"),
        format!(
            "[jobs.proj]\ntask = \"t\"\nnotify = \"echo x > '{}'\"\n",
            fwd(&flag)
        ),
    )
    .unwrap();
    let out = kelir(&sb, &mock, &["job", "run", "proj"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stderr.contains("not trusted"), "{}", out.stderr);
    assert!(!flag.exists(), "an untrusted project's hook ran");
    let out = kelir(
        &sb,
        &mock,
        &["job", "run", "proj"],
        &[("KELIR_TRUST_PROJECT", "1")],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(flag.exists(), "a trusted project's hook did not run");
    clean(&sb).await;
}

// ------------------------------------------------------- trust and tolerance

fn walk(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, found);
        } else {
            found.push(path);
        }
    }
}

#[tokio::test]
async fn no_secret_reaches_names_jobs_or_the_history_and_the_directories_are_the_users() {
    let mock = Mock::start(vec![text("answer")]).await;
    let sb = Sandbox::new("");
    jobs_toml(&sb, "[jobs.canary]\ntask = \"t\"\nnotify = \"true\"\n");
    assert_eq!(
        kelir(&sb, &mock, &["job", "run", "canary"], &[]).await.code,
        0
    );
    assert_eq!(
        kelir(&sb, &mock, &["sessions", "ls", "--json"], &[])
            .await
            .code,
        0
    );
    let token = std::fs::read_to_string(
        sb.state
            .join("sessions")
            .join(sb.only_session())
            .join("token"),
    )
    .ok()
    .map(|t| t.trim().to_string())
    .filter(|t| !t.is_empty());
    let mut files = Vec::new();
    walk(&sb.state.join("jobs"), &mut files);
    walk(&sb.state.join("names"), &mut files);
    assert!(files.len() >= 4, "{files:?}");
    for file in files {
        let body = std::fs::read_to_string(&file).unwrap_or_default();
        for secret in ["test-openrouter-key", "test-go-key"] {
            assert!(!body.contains(secret), "{} holds a key", file.display());
        }
        if let Some(token) = &token {
            assert!(
                !body.contains(token.as_str()),
                "{} holds the session token",
                file.display()
            );
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for dir in [
            sb.state.join("names"),
            sb.state.join("jobs"),
            sb.state.join("jobs").join("canary"),
        ] {
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{} is {mode:o}", dir.display());
        }
    }
    clean(&sb).await;
}

#[tokio::test]
async fn corrupt_status_and_history_are_tolerated_and_the_next_run_repairs_them() {
    let mock = Mock::start(vec![text("fine")]).await;
    let sb = Sandbox::new("");
    jobs_toml(&sb, NIGHTLY);
    let dir = job_dir(&sb, "nightly");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("STATUS.json"), "{ not json").unwrap();
    std::fs::write(
        dir.join("runs.jsonl"),
        "garbage\n{\"run\":\"run_ok\",\"outcome\":\"done\"}\n{\"torn",
    )
    .unwrap();

    let shown = kelir(&sb, &mock, &["job", "status", "nightly"], &[]).await;
    assert_eq!(shown.code, 1);
    assert!(
        shown.stderr.contains("unreadable") && !shown.stderr.contains("panicked"),
        "{}",
        shown.stderr
    );
    let all = kelir(&sb, &mock, &["job", "status"], &[]).await;
    assert_eq!(all.code, 0);
    assert!(all.stdout.contains("unreadable"), "{}", all.stdout);
    let hist = kelir(&sb, &mock, &["job", "history", "nightly", "--json"], &[]).await;
    assert_eq!(hist.code, 0);
    let hist: Vec<Value> = serde_json::from_str(hist.stdout.trim()).unwrap();
    assert_eq!(hist.len(), 1);

    let run = kelir(&sb, &mock, &["job", "run", "nightly"], &[]).await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(status(&sb, "nightly")["outcome"], "done");
    let hist = kelir(&sb, &mock, &["job", "history", "nightly", "--json"], &[]).await;
    let hist: Vec<Value> = serde_json::from_str(hist.stdout.trim()).unwrap();
    assert_eq!(
        hist.len(),
        2,
        "the new record is not glued to the torn line: {hist:?}"
    );
    assert_eq!(hist[1]["outcome"], "done");
    clean(&sb).await;
}
