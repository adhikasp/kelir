//! Independent reviewer probes for the M9 fix pass. Read-only on `src/`; every
//! process goes through `Sandbox::guard`, the whole file is spawn-capped, and
//! `clean()` asserts nothing of ours is left (AGENTS.md process rules).

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::time::{Duration, Instant};

use crate::support::*;
use serde_json::{Value, json};

const SPAWN_CAP: usize = 80;
static SPAWNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn count_spawn() {
    let n = SPAWNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    assert!(n <= SPAWN_CAP, "runaway: {n} processes spawned");
}

fn text(answer: &str) -> Reply {
    Reply::Sse(sse(&text_turn(answer, 0)))
}

async fn kelir(sb: &Sandbox, mock: &Mock, args: &[&str], env: &[(&str, &str)]) -> Output {
    count_spawn();
    sb.kelir(&mock.base_url(), args, env).await
}

fn jobs_toml(sb: &Sandbox, body: &str) {
    let dir = sb.state.join("no-config");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("jobs.toml"), body).unwrap();
}

fn jobdir(sb: &Sandbox, job: &str) -> std::path::PathBuf {
    sb.state.join("jobs").join(job)
}

fn status(sb: &Sandbox, job: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(jobdir(sb, job).join("STATUS.json")).unwrap())
        .unwrap()
}

fn completions(mock: &Mock) -> Vec<Recorded> {
    mock.requests()
        .into_iter()
        .filter(|request| request.method == "POST")
        .collect()
}

fn of_type<'a>(log: &'a [Value], kind: &str) -> Vec<&'a Value> {
    log.iter().filter(|e| e["type"] == kind).collect()
}

async fn clean(sb: &Sandbox) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while sb.guard.active() > 0 {
        assert!(
            Instant::now() < deadline,
            "{} of this test's processes are still alive",
            sb.guard.active()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A job with `max_steps = 2` whose second (cap-th) step contains a tool call
/// AND the final answer must pause after the tool result, not between the call
/// and its result, and must not ask for a third step. This is the sharp edge
/// the shipped one-step text test does not cover: the new check sits after
/// `after_step`, so a late boundary must not be able to slip past it.
#[tokio::test]
async fn a_job_cap_th_answer_after_a_tool_pauses_after_the_tool_result() {
    let answer = "late answer on the cap";
    let mut capped = tool_call_chunks("c1", "edit", &json!({"path":"f.txt","old":"","new":"x\n"}));
    capped.push(finish("tool_calls"));
    capped.extend(text_turn(answer, 0));
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "c0",
            "edit",
            &json!({"path":"f0.txt","old":"","new":"x\n"}),
        ))),
        Reply::Sse(sse(&capped)),
        Reply::Sse(sse(&text_turn("never asked", 0))),
    ])
    .await;
    let sb = Sandbox::new("");
    jobs_toml(&sb, "[jobs.late]\ntask = \"t\"\nmax_steps = 2\n");
    let out = kelir(&sb, &mock, &["job", "run", "late"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);

    let now = status(&sb, "late");
    assert_eq!(now["outcome"], "paused", "{now}");
    assert_eq!(now["steps"], 2, "{now}");
    assert_eq!(completions(&mock).len(), 2, "no third request past the cap");
    let id = sb.only_session();
    let log = sb.log(&id);
    let ended = of_type(&log, "turn_ended").pop().expect("turn_ended");
    assert_eq!(ended["outcome"], "paused");
    assert_eq!(ended["reason"], "steps");
    assert_eq!(ended["final_text"], answer, "the cap-th answer is kept");
    let settled = of_type(&log, "tool_settled").len();
    assert_eq!(settled, 2, "both calls were settled before the pause");
    let types: Vec<&str> = log
        .iter()
        .map(|e| e["type"].as_str().unwrap_or(""))
        .collect();
    let last_tool = types.iter().rposition(|t| *t == "tool_settled").unwrap();
    let turn_end = types.iter().rposition(|t| *t == "turn_ended").unwrap();
    assert!(
        last_tool < turn_end,
        "never between a call and its result: {types:?}"
    );
    clean(&sb).await;
}

/// The cap must not be advertised to the model: it is a scheduling rule, not
/// task text. The request's system prompt, source messages and task are
/// byte-identical to an uncapped run of the same job; only STATUS/events say
/// `paused`, and they say `steps` (not an error).
#[tokio::test]
async fn the_job_cap_is_not_in_the_model_prompt_and_is_not_misreported() {
    let sb = Sandbox::new("");
    jobs_toml(&sb, "[jobs.secretcap]\ntask = \"t\"\nmax_steps = 1\n");
    let mock = Mock::start(vec![text("finished")]).await;
    let out = kelir(&sb, &mock, &["job", "run", "secretcap"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);

    // Capture the paused STATUS and log BEFORE the next run overwrites them.
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
            .is_some_and(|e| e.to_lowercase().contains("paused")),
        "STATUS says why it paused: {paused}"
    );
    let paused_events = serde_json::to_string(&sb.log(&sb.only_session())).unwrap();
    assert!(
        !paused_events.contains("job_step_cap"),
        "internal name leaked"
    );
    let a = completions(&mock)[0].messages().to_vec();

    // Lift the cap in the SAME sandbox: same session, system prompt and paths.
    let mock2 = Mock::start(vec![text("finished again")]).await;
    let out2 = kelir(
        &sb,
        &mock2,
        &["job", "run", "secretcap", "--max-steps", "0"],
        &[],
    )
    .await;
    assert_eq!(out2.code, 0, "{}", out2.stderr);
    assert_eq!(status(&sb, "secretcap")["outcome"], "done");
    let b = completions(&mock2)[0].messages().to_vec();
    // The first request of the second run extends the first one's prefix;
    // only job line + task are new, and neither mentions the cap.
    assert_eq!(a[0], b[0], "the system prompt must be identical");
    assert_eq!(&b[..a.len()], &a[..], "cached prefix untouched");
    assert_eq!(a.len(), b.len() - 3, "only the tail messages are new");
    let joined = serde_json::to_string(&a).unwrap();
    assert!(!joined.contains("max_steps"), "{joined}");
    assert!(!joined.contains("step cap"), "{joined}");
    assert!(!joined.contains("job_step_cap"), "{joined}");
    clean(&sb).await;
}

/// A rotating job run must put the carry line BEFORE the job line, both at
/// the tail, and leave the frozen prefix untouched. The shipped e2e checks
/// only the carry line, so it would pass if the order flipped or the prefix
/// changed.
#[tokio::test]
async fn a_rotating_job_adds_carry_then_job_after_the_frozen_prefix() {
    let mock = Mock::start(vec![text("the old session learned X"), text("fresh start")]).await;
    let sb = Sandbox::new("");
    jobs_toml(
        &sb,
        "[jobs.rot2]\ntask = \"go\"\nnew_session_every = \"1s\"\nsession = \"rot2name\"\n",
    );
    assert_eq!(
        kelir(&sb, &mock, &["job", "run", "rot2"], &[]).await.code,
        0
    );
    let old = sb.only_session();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        kelir(&sb, &mock, &["job", "run", "rot2"], &[]).await.code,
        0
    );
    let reqs = completions(&mock);
    // A rotation starts a NEW session: a fresh system prompt is expected,
    // and the carry line is the only bridge. The cached-prefix guarantee is
    // about resumes of the SAME session, checked by e2e_m9's resume test.
    assert_eq!(reqs.len(), 2);
    let second = reqs[1].messages();
    let tail: Vec<&str> = second[second.len() - 3..]
        .iter()
        .map(|m| m["content"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(tail.len(), 3, "carry, job, task: {tail:?}");
    assert!(tail[0].starts_with("[source: carry "), "{tail:?}");
    assert!(
        tail[0].contains("the old session learned X") && tail[0].contains(&old),
        "{tail:?}"
    );
    assert!(tail[1].starts_with("[source: job rot2]"), "{tail:?}");
    assert_eq!(tail[2], "go");
    let log = sb.log(&sb.session_ids()[0]);
    let kinds: Vec<&str> = of_type(&log, "source")
        .iter()
        .map(|source| source["kind"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        kinds,
        vec!["carry", "job"],
        "two tail sources, in order: {kinds:?}"
    );
    clean(&sb).await;
}

/// S2 through the binary: a young lock with no readable record must be
/// respected (never taken over), exit 75, write a `skipped` line that names
/// the reason, and print that reason on stderr — with no pid and no secret.
#[tokio::test]
async fn an_unreadable_young_lock_is_respected_and_the_skip_explains_why() {
    let sb = Sandbox::new("");
    jobs_toml(&sb, "[jobs.unreadable]\ntask = \"t\"\n");
    let dir = jobdir(&sb, "unreadable");
    std::fs::create_dir_all(&dir).unwrap();
    let lock = dir.join("lock");
    std::fs::write(&lock, b"not json at all").unwrap();

    let mock = Mock::start(vec![]).await;
    let out = kelir(&sb, &mock, &["job", "run", "unreadable"], &[]).await;
    assert_eq!(out.code, 75, "{}", out.stderr);
    assert!(
        out.stderr.contains("unreadable"),
        "the skip must say why: {}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("test-openrouter-key") && !out.stderr.contains("test-go-key"),
        "no key in stderr: {}",
        out.stderr
    );
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        "not json at all",
        "a fresh unreadable lock is never taken over"
    );
    let runs: Vec<Value> = std::fs::read_to_string(dir.join("runs.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0]["outcome"], "skipped");
    assert_eq!(runs[0]["exit_code"], 75);
    assert!(runs[0]["holder_pid"].is_null(), "{runs:?}");
    assert!(
        runs[0]["error"]
            .as_str()
            .is_some_and(|e| e.contains("unreadable")),
        "the history line must say why: {runs:?}"
    );
    assert!(mock.requests().is_empty(), "no model turn on a skip");
    clean(&sb).await;
}

/// Pid reuse: a dead, OLD lock whose recorded pid is now alive (pid 1) must
/// be taken over — the start-time check is what makes this safe. Guards
/// against a fix that simply refuses all locks whose pid is alive.
#[tokio::test]
async fn an_old_lock_with_a_recycled_pid_is_taken_over() {
    let sb = Sandbox::new("");
    jobs_toml(&sb, "[jobs.recycled]\ntask = \"t\"\n");
    let dir = jobdir(&sb, "recycled");
    std::fs::create_dir_all(&dir).unwrap();
    let lock = dir.join("lock");
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
    let out = kelir(&sb, &mock, &["job", "run", "recycled"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(status(&sb, "recycled")["outcome"], "done");
    assert_eq!(completions(&mock).len(), 1);
    assert!(!lock.exists(), "the taken-over lock was released");
    let runs: Vec<Value> = std::fs::read_to_string(dir.join("runs.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["outcome"], "done");
    clean(&sb).await;
}

/// Interactive `--max-steps` on the cap-th answer stays `done` (STEER-4)
/// even while a job's Plan is installed in ANOTHER process: the Plan is
/// process-local, so an interactive run must not inherit the job's rule.
#[tokio::test]
async fn interactive_max_steps_keeps_the_cap_th_answer_done() {
    use std::process::Stdio;
    let mock = Mock::start(vec![text("finished on the cap"), text("never asked")]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(&config, test_config("")).unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_kelir"));
    scrub_std(&mut command);
    command
        .args(["run", "--json", "--max-steps", "1", "task"])
        .current_dir(&cwd)
        .env("KELIR_CONFIG", &config)
        .env("KELIR_CONFIG_DIR", temp.path().join("no-config"))
        .env("KELIR_STATE_DIR", temp.path().join("state"))
        .env("KELIR_BASE_URL", mock.base_url())
        .env("KELIR_HOME", temp.path().join("home"))
        .env("OPENROUTER_API_KEY", "test-openrouter-key")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    count_spawn();
    let out = tokio::process::Command::from(command)
        .output()
        .await
        .expect("run kelir");
    assert_eq!(out.status.code(), Some(0));
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let result = lines.iter().find(|line| line["type"] == "result").unwrap();
    assert_eq!(result["outcome"], "done", "STEER-4 cap-th answer");
    assert_eq!(completions(&mock).len(), 1);
}
