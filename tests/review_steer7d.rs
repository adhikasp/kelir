//! STEER-7 round 4 (steer7d review): what is left after "no secret ever leaves the session or the
//! job's own shim".
//!
//! `bg-spawn` still takes the caller's `kind`, `sock`, `token` and whole `env`. These tests ask what a
//! token-holding tool child gets out of that. Each one is red while the weakness is open.

mod support;

use serde_json::{Value, json};
use support::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn raw(session: &LiveSession, request: Value) -> Value {
    let stream = kelir::ipc::connect(&session.session_dir, &session.id)
        .await
        .expect("connect");
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read);
    let mut line = request.to_string();
    line.push('\n');
    let mut answer = String::new();
    write.write_all(line.as_bytes()).await.unwrap();
    write.flush().await.unwrap();
    lines.read_line(&mut answer).await.unwrap();
    serde_json::from_str(answer.trim()).unwrap_or(json!({"unparsed": answer}))
}

fn session_env(session: &LiveSession, token: &str) -> Vec<(String, String)> {
    vec![
        (
            "KELIR_SESSION_DIR".into(),
            kelir::util::path_string(&session.session_dir),
        ),
        (
            "KELIR_SOCK".into(),
            kelir::ipc::value(&session.session_dir, &session.id),
        ),
        ("KELIR_TOKEN".into(), token.to_string()),
    ]
}

async fn start_job(session: &LiveSession, command: &str) -> String {
    let token = kelir::trust::read_token(&session.session_dir).unwrap();
    let sandbox = Sandbox::from_live(session);
    let env = session_env(session, &token);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let started = sandbox
        .kelir("http://127.0.0.1:9/v1", &["bg", "run", command], &env)
        .await;
    assert_eq!(started.code, 0, "stderr: {}", started.stderr);
    started
        .stdout
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn status_path(session: &LiveSession, id: &str) -> std::path::PathBuf {
    session.session_dir.join("bg").join(format!("{id}.status"))
}

fn full_env(session: &LiveSession, token: &str) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.extend(session_env(session, token));
    env
}

fn long_command() -> &'static str {
    if cfg!(windows) {
        "ping -n 40 127.0.0.1 > nul"
    } else {
        "sleep 40"
    }
}

/// The shell argv a `kelir bg run` would send (the config's shell: Git bash on Windows).
fn shell_of(_session: &LiveSession, _id: &str) -> Value {
    if cfg!(windows) {
        json!(["C:/Program Files/Git/bin/bash.exe", "-c"])
    } else {
        json!(["bash", "-c"])
    }
}

fn spawn_args(session: &LiveSession, shell: &Value, cmd: &str, extra: Value) -> Value {
    let bg = session.session_dir.join("bg");
    let mut args = json!({"dir": kelir::util::path_string(&bg), "out": "", "cmd": cmd,
                          "shell": shell, "kind": "job"});
    for (key, value) in extra.as_object().unwrap() {
        args[key] = value.clone();
    }
    args
}

/// A refused `bg-spawn` is written to the log with its arguments, and the arguments carry the
/// caller's whole environment and the session token (`SpawnRequest.env`, `.token`).
#[tokio::test]
async fn a_refused_spawn_does_not_write_the_callers_environment_or_token_into_the_log() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let token = kelir::trust::read_token(&session.session_dir).unwrap();
    let canary = "CANARY-ENV-SECRET-7d-4f1a9";
    let args = spawn_args(
        &session,
        &json!([]),
        "echo hi",
        json!({"token": token, "env": [["SOME_API_KEY", canary], ["KELIR_TOKEN", token]],
               "cwd": "this-directory-does-not-exist-7d"}),
    );
    let answer = raw(
        &session,
        json!({"verb": "bg-spawn", "from": "job", "token": token, "args": args}),
    )
    .await;
    assert_eq!(answer["ok"], json!(false), "{answer}");
    let log = std::fs::read_to_string(session.session_dir.join("events.jsonl")).unwrap();
    let _ = session.end().await;
    assert!(
        !log.contains(canary) && !log.contains(&token),
        "the refused spawn's environment ({}) / session token ({}) is in events.jsonl",
        log.contains(canary),
        log.contains(&token)
    );
}

/// `bg-spawn` copies `sock` from the request into the shim, and the shim sends its **capability** to
/// that address when the job starts. A tool child names an endpoint it listens on and reads the
/// capability of the job it just made.
#[tokio::test]
async fn a_spawn_cannot_send_the_jobs_capability_to_an_address_the_caller_chose() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let token = kelir::trust::read_token(&session.session_dir).unwrap();
    let seed = start_job(&session, long_command()).await;
    let shell = shell_of(&session, &seed);

    let evil_dir = std::env::temp_dir().join(format!("k7d-evil-{}", std::process::id()));
    std::fs::create_dir_all(&evil_dir).unwrap();
    let evil_id = format!("evil{}", std::process::id());
    let listener = kelir::ipc::bind(&evil_dir, &evil_id).expect("bind the caller's own endpoint");
    let heard = tokio::spawn(async move {
        use interprocess::local_socket::tokio::prelude::*;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut lines_seen = Vec::new();
        while let Ok(Ok(accepted)) = tokio::time::timeout_at(deadline, listener.accept()).await {
            let mut lines = BufReader::new(accepted);
            let mut line = String::new();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), lines.read_line(&mut line)).await;
            if !line.trim().is_empty() {
                lines_seen.push(line);
                break;
            }
        }
        lines_seen.pop()
    });
    let args = spawn_args(
        &session,
        &shell,
        long_command(),
        json!({"token": token, "sock": kelir::ipc::value(&evil_dir, &evil_id),
               "env": full_env(&session, &token)}),
    );
    let answer = raw(
        &session,
        json!({"verb": "bg-spawn", "from": "job", "token": token, "args": args}),
    )
    .await;
    let heard = heard.await.unwrap();
    eprintln!("sock-redirect: spawn answer ok={} heard={}", answer["ok"], heard.is_some());
    let ids: Vec<String> = session
        .ctl(&["bg", "ls"])
        .await
        .stdout
        .split_whitespace()
        .filter(|word| word.starts_with("bg_"))
        .map(str::to_string)
        .collect();
    for id in ids {
        let _ = session.ctl(&["cancel", &id]).await;
    }
    let _ = session.end().await;
    let _ = std::fs::remove_dir_all(&evil_dir);
    if let Some(line) = heard.as_deref().and_then(|l| serde_json::from_str::<Value>(l.trim()).ok()) {
        eprintln!(
            "heard verb={} keys={:?} capability_field={}",
            line["verb"],
            line.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()),
            line["capability"].is_string()
        );
    }
    eprintln!("heard line (scrubbed): {:?}", heard.as_deref().map(|l| {
        let mut out = String::new(); let mut run = 0;
        for c in l.chars().take(400) { if c.is_ascii_hexdigit() { run += 1; if run > 20 { continue; } } else { run = 0; } out.push(c); }
        out
    }));
    let leaked = heard
        .as_deref()
        .and_then(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .and_then(|request| request["capability"].as_str().map(str::len))
        .unwrap_or(0);
    assert_eq!(answer["ok"], json!(true), "the spawn was refused, so this proved nothing: {answer}");
    assert!(heard.is_some(), "nothing connected to the caller's endpoint: the test is vacuous");
    assert!(
        leaked == 0,
        "the shim sent a {leaked}-char capability to the caller's own listener"
    );
}

/// The shim hardens itself against same-user memory reads only when it believes it is a job, and
/// "kind" is the caller's word in a `bg-spawn`. A spawn that says `kind: "x"` gets a shim that holds the
/// signing key of its (session-minted) job in readable memory.
#[cfg(windows)]
#[tokio::test]
async fn a_spawn_with_another_kind_still_gets_a_shim_closed_to_memory_reads() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let token = kelir::trust::read_token(&session.session_dir).unwrap();
    let seed = start_job(&session, long_command()).await;
    let shell = shell_of(&session, &seed);
    let args = spawn_args(
        &session,
        &shell,
        long_command(),
        json!({"token": token, "kind": "x", "env": full_env(&session, &token),
               "sock": kelir::ipc::value(&session.session_dir, &session.id)}),
    );
    let answer = raw(
        &session,
        json!({"verb": "bg-spawn", "from": "job", "token": token, "args": args}),
    )
    .await;
    let id = answer["state"]["id"].as_str().map(str::to_string);
    eprintln!("kind=x spawn: {answer}");
    let mut outcome = String::from("spawn refused");
    let mut found = false;
    if let Some(id) = &id {
        let mut shim = None;
        for _ in 0..50 {
            if let Ok(text) = std::fs::read_to_string(status_path(&session, id))
                && let Ok(status) = serde_json::from_str::<Value>(&text)
                && let Some(pid) = status["shim"].as_u64()
            {
                shim = Some(pid);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        if let Some(pid) = shim {
            let keyfile = session
                .session_dir
                .join("..")
                .join(format!("stolen7d-{id}.key"));
            let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts/live/steer7c_memscan.py");
            let scan = std::process::Command::new("python")
                .arg("-I")
                .arg(&script)
                .arg(pid.to_string())
                .arg(&keyfile)
                .output()
                .unwrap();
            outcome = String::from_utf8_lossy(&scan.stdout).trim().to_string();
            eprintln!("kind=x status: {}", std::fs::read_to_string(status_path(&session, id)).unwrap_or_default().chars().take(700).collect::<String>());
            found = keyfile.exists();
            let _ = std::fs::remove_file(&keyfile);
        } else {
            outcome = "no shim pid".into();
        }
    }
    let ids: Vec<String> = session
        .ctl(&["bg", "ls"])
        .await
        .stdout
        .split_whitespace()
        .filter(|word| word.starts_with("bg_"))
        .map(str::to_string)
        .collect();
    for id in ids {
        let _ = session.ctl(&["cancel", &id]).await;
    }
    eprintln!("kind=x scan outcome: {outcome} found={found}");
    let _ = session.end().await;
    assert!(id.is_some(), "the spawn was refused: this test proved nothing");
    assert!(
        !found,
        "memory scan of a kind=x shim: {outcome} (the key is readable)"
    );
}

/// Unix: the same, observed through /proc ownership (a non-dumpable process's /proc files belong to root).
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_spawn_with_another_kind_still_gets_a_non_dumpable_shim() {
    use std::os::unix::fs::MetadataExt as _;
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let token = kelir::trust::read_token(&session.session_dir).unwrap();
    let seed = start_job(&session, long_command()).await;
    let shell = shell_of(&session, &seed);
    let genuine: Value =
        serde_json::from_str(&std::fs::read_to_string(status_path(&session, &seed)).unwrap())
            .unwrap();
    let control_uid = std::fs::metadata(format!("/proc/{}/environ", genuine["shim"]))
        .map(|meta| meta.uid())
        .ok();
    let args = spawn_args(
        &session,
        &shell,
        long_command(),
        json!({"token": token, "kind": "x", "env": full_env(&session, &token),
               "sock": kelir::ipc::value(&session.session_dir, &session.id)}),
    );
    let answer = raw(
        &session,
        json!({"verb": "bg-spawn", "from": "job", "token": token, "args": args}),
    )
    .await;
    let id = answer["state"]["id"].as_str().map(str::to_string);
    let mut owner = None;
    if let Some(id) = &id {
        for _ in 0..50 {
            if let Ok(text) = std::fs::read_to_string(status_path(&session, id))
                && let Ok(status) = serde_json::from_str::<Value>(&text)
                && let Some(pid) = status["shim"].as_u64()
            {
                owner = std::fs::metadata(format!("/proc/{pid}/environ"))
                    .map(|meta| meta.uid())
                    .ok();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }
    let ids: Vec<String> = session
        .ctl(&["bg", "ls"])
        .await
        .stdout
        .split_whitespace()
        .filter(|word| word.starts_with("bg_"))
        .map(str::to_string)
        .collect();
    for id in ids {
        let _ = session.ctl(&["cancel", &id]).await;
    }
    let _ = session.end().await;
    assert_eq!(
        control_uid,
        Some(0),
        "positive control: a kind=job shim must be non-dumpable"
    );
    assert!(
        id.is_none() || owner == Some(0),
        "a kind=x shim is dumpable (its /proc files belong to uid {owner:?})"
    );
}

/// Unix: `.final` is read by the session for every job it owns. A FIFO planted there (any file
/// type a same-user child can create) must not block the session.
#[cfg(unix)]
#[tokio::test]
async fn a_fifo_planted_as_the_final_sidecar_does_not_hang_the_session() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let id = start_job(&session, long_command()).await;
    let fifo = session
        .session_dir
        .join("bg")
        .join(format!("{id}.final"));
    let made = std::process::Command::new("mkfifo").arg(&fifo).status();
    let answered = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        session.ctl(&["tools", "--disable", "bash"]),
    )
    .await;
    // Open the fifo for writing so a blocked reader can be released and the session can end.
    let _ = std::fs::OpenOptions::new().write(true).open(&fifo);
    let _ = std::fs::remove_file(&fifo);
    let _ = session.ctl(&["cancel", &id]).await;
    let _ = session.end().await;
    assert!(made.is_ok_and(|s| s.success()), "mkfifo");
    assert!(
        answered.is_ok(),
        "the session did not answer `bg ls` within 20 s (`tools`) with a FIFO at <id>.final"
    );
}

/// The session process holds the private key of **every** job it minted (to hand it to the shim, and to
/// sign `cancel`), as a hex string for its whole life. Hardening only the shim leaves the larger store
/// open: any same-user tool child reads the session's memory instead of the shim's.
#[cfg(windows)]
#[tokio::test]
async fn a_same_user_child_cannot_read_job_keys_from_the_sessions_memory() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let id = start_job(&session, long_command()).await;
    let keyfile = session
        .session_dir
        .join("..")
        .join(format!("stolen7d-session-{id}.key"));
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/live/steer7c_memscan.py");
    let scan = std::process::Command::new("python")
        .arg("-I")
        .arg(&script)
        .arg(session.pid().to_string())
        .arg(&keyfile)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&scan.stdout).trim().to_string();
    let found = keyfile.exists();
    let _ = std::fs::remove_file(&keyfile);
    let _ = session.ctl(&["cancel", &id]).await;
    let _ = session.end().await;
    assert!(!found, "scan of the session process: {said}");
}

/// Linux: the session is dumpable, so with `kernel.yama.ptrace_scope=0` (or any ptrace-capable peer)
/// its `/proc/<pid>/mem` is readable by the user's other processes; a non-dumpable process's /proc
/// files belong to root.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_session_process_is_not_dumpable_while_it_holds_job_keys() {
    use std::os::unix::fs::MetadataExt as _;
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let id = start_job(&session, long_command()).await;
    let owner = std::fs::metadata(format!("/proc/{}/environ", session.pid()))
        .map(|meta| meta.uid())
        .ok();
    let scope = std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope").unwrap_or_default();
    let _ = session.ctl(&["cancel", &id]).await;
    let _ = session.end().await;
    assert_eq!(
        owner,
        Some(0),
        "the session (holding every job's signing key) is dumpable; ptrace_scope={}",
        scope.trim()
    );
}
