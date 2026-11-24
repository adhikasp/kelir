//! Test support: a scripted, in-process OpenAI-compatible SSE server and a
//! harness that drives the real `kelir` binary against it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncWriteExt;

mod harness;
pub use harness::*;
mod mock;
pub use mock::*;
mod ui;
pub use ui::{ApplyEv, Ev};

/// How long any wait in the harness may take before it calls the thing hung
/// (STEER-9). It bounds a hang, never a normal run: every wait below returns the
/// moment its event, file or exit shows up, so the ceiling only has to be larger
/// than the worst scheduling stall on a box running several workers and builds.
pub const CEILING: Duration = Duration::from_secs(90);

/// A bash command that blocks until the test calls [`LiveSession::release`]
/// (STEER-9). The tests used `sleep 1` to mean "a call that is still running when
/// the verb arrives", which holds on a quiet box and not on one where the `ctl`
/// process takes two seconds to start. Held by the test, the call is running for
/// exactly as long as the test needs.
pub const HOLD: &str = "while [ ! -e kelir-release ]; do sleep 0.05; done";

/// [`HOLD`], then `rest`.
pub fn hold_then(rest: &str) -> String {
    format!("{HOLD}; {rest}")
}

/// A session-less `bg run` job's root, `<state>/bg/`.
pub fn global_job_root(state: &Path) -> PathBuf {
    state.join("calls")
}

/// A session job's status file: the job owns `<session>/calls/<id>/`.
pub fn job_status_path(session_dir: &Path, id: &str) -> PathBuf {
    kelir::features::bg::status_path(&kelir::paths::calls_dir(session_dir), id)
}

/// A session job's output file.
pub fn job_out_path(session_dir: &Path, id: &str) -> PathBuf {
    kelir::features::bg::out_path(&kelir::paths::calls_dir(session_dir), id)
}

/// A session job's root, `<session>/calls/`.
pub fn job_root(session_dir: &Path) -> PathBuf {
    kelir::paths::calls_dir(session_dir)
}

/// Every status file under a job root. Each job owns `<root>/<id>/<id>.status`.
pub fn status_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let file = path.join(format!("{name}.status"));
        if file.is_file() {
            out.push(file);
        }
    }
    out
}

/// Poll `check` (cheap, synchronous) until it returns true; panic with `what`,
/// the ceiling and the `diagnostics()` text when it never does. The one
/// replacement for "sleep N and hope": a wait that ends on the condition.
pub async fn eventually(what: &str, check: impl FnMut() -> bool, diagnostics: impl Fn() -> String) {
    if !eventually_within(CEILING, check).await {
        panic!(
            "timed out after {CEILING:?} waiting for {what}
--- diagnostics ---
{}",
            diagnostics()
        );
    }
}

/// [`eventually`] without the panic: true if `check` held within `limit`. For
/// a caller that has its own assertion message.
pub async fn eventually_within(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + limit;
    loop {
        if check() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Poll `count` until it reads zero (a process tree that must be gone) and
/// return the last value read. The caller keeps its own assertion on the number,
/// so nothing is weakened: the only change from `sleep(3s); count()` is that a
/// slow teardown on a loaded box gets until the ceiling instead of a guess.
pub async fn settle_to_zero(mut count: impl FnMut() -> usize) -> usize {
    let mut last = count();
    let deadline = std::time::Instant::now() + CEILING;
    while last > 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        last = count();
    }
    last
}
// ------------------------------------------------------------------- harness

pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub events: Vec<Value>,
    pub session_dir: PathBuf,
    pub cwd: PathBuf,
    _temp: tempfile::TempDir,
}

impl Run {
    /// The NDJSON lines the binary wrote to stdout.
    pub fn json_lines(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|_| panic!("stdout line is not JSON: {line}"))
            })
            .collect()
    }

    pub fn event_types(&self) -> Vec<String> {
        self.events
            .iter()
            .map(|event| event["type"].as_str().unwrap_or("").to_string())
            .collect()
    }

    pub fn events_of(&self, event_type: &str) -> Vec<&Value> {
        self.events
            .iter()
            .filter(|event| event["type"] == event_type)
            .collect()
    }

    pub fn one_call(&self, event_type: &str, kind: &str) -> &Value {
        let found: Vec<_> = self
            .events_of(event_type)
            .into_iter()
            .filter(|e| e["kind"] == kind)
            .collect();
        assert_eq!(found.len(), 1, "stderr: {}", self.stderr);
        found[0]
    }

    pub fn one_event(&self, event_type: &str) -> &Value {
        let found = self.events_of(event_type);
        assert_eq!(
            found.len(),
            1,
            "expected exactly one {event_type} event, got {}; stderr: {}",
            found.len(),
            self.stderr
        );
        found[0]
    }

    pub fn result(&self) -> Value {
        self.json_lines()
            .into_iter()
            .find(|line| line["type"] == "result")
            .expect("a result line on stdout")
    }

    pub fn command_out(&self, call_id: &str) -> String {
        let path = self.session_dir.join("calls").join(call_id).join("out");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
    }
}

pub async fn run_task(
    mock: &Mock,
    args: &[&str],
    env: &[(&str, &str)],
    stdin: Option<&str>,
    extra_config: &str,
) -> Run {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(&config, test_config(extra_config)).unwrap();

    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_kelir"));
    // Inherited `KELIR_*`/`TERM_PROGRAM` first, the test's own environment
    // second: the scrub must never undo what the test deliberately set.
    scrub_session_env(&mut command);
    command
        .args(args)
        .current_dir(&cwd)
        .env("KELIR_CONFIG", &config)
        .env("KELIR_CONFIG_DIR", temp.path().join("no-config"))
        .env("KELIR_STATE_DIR", temp.path().join("state"))
        .env("KELIR_BASE_URL", mock.base_url())
        // No real home: a run's sources are its own temp dirs, never this
        // machine's `~/.claude/skills` (a test must not depend on a desktop).
        .env("KELIR_HOME", temp.path().join("home"))
        .env("OPENROUTER_API_KEY", "test-openrouter-key")
        .env("KELIR_GO_API_KEY", "test-go-key")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }

    let mut child = command.spawn().expect("spawn kelir");
    if let Some(input) = stdin {
        let mut handle = child.stdin.take().expect("stdin");
        handle.write_all(input.as_bytes()).await.unwrap();
        drop(handle);
    }
    let output = tokio::time::timeout(CEILING, child.wait_with_output())
        .await
        .expect("kelir did not finish within the ceiling")
        .expect("wait for kelir");

    let sessions = temp.path().join("state").join("sessions");
    let session_dir = std::fs::read_dir(&sessions)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| path.is_dir())
        .unwrap_or_else(|| sessions.clone());
    let events_text = std::fs::read_to_string(session_dir.join("events.jsonl")).unwrap_or_default();
    let events = events_text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("bad event line ({error}): {line}"))
        })
        .collect();

    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        events,
        session_dir,
        cwd,
        _temp: temp,
    }
}

// --------------------------------------------------- a running session + ctl

/// A `kelir run` (usually `--keep`) that stays up while the test drives it, and
/// the `kelir ctl` surface that talks to it.
pub struct LiveSession {
    child: tokio::process::Child,
    stdout: tokio::io::BufReader<tokio::process::ChildStdout>,
    stderr: Arc<Mutex<String>>,
    stderr_task: tokio::task::JoinHandle<()>,
    _temp: tempfile::TempDir,
    pub cwd: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub id: String,
    pub session_dir: PathBuf,
    /// Every NDJSON line read from the session so far.
    pub events: Vec<Value>,
    /// The test's process guard: the session and everything it starts die with
    /// the test (AGENTS.md hard rule).
    pub guard: Arc<TreeGuard>,
}

pub struct Ctl {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Ctl {
    /// The one JSON response line the session wrote back.
    pub fn response(&self) -> Value {
        self.stdout
            .lines()
            .rfind(|line| line.trim_start().starts_with('{'))
            .unwrap_or_else(|| panic!("no JSON answer; stderr: {}", self.stderr))
            .parse()
            .unwrap_or_else(|error| panic!("answer is not JSON ({error}); stderr: {}", self.stderr))
    }
}

/// Is `pid` still a running process? (The support module's own answer: the e2e
/// tests carry their own copy; this one is for a diagnostic message.)
fn pid_alive_here(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let tasklist = std::path::Path::new(&system_root)
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

impl LiveSession {
    pub async fn start(mock: &Mock, args: &[&str], extra_config: &str) -> LiveSession {
        Self::start_with_flags(mock, args, extra_config, 0).await
    }

    /// A session on a state directory a previous one already used: the second
    /// start of a box whose metadata cache is warm (STEER-1 review).
    pub async fn start_reusing(
        state: PathBuf,
        mock: &Mock,
        args: &[&str],
        extra_config: &str,
    ) -> LiveSession {
        Self::start_in(Some(state), mock, args, extra_config, 0).await
    }

    /// A session on a state dir of the caller's own and a base URL that is not
    /// the mock's (an unreachable endpoint, a second gateway), so one box's
    /// metadata cache can be tested against another endpoint (STEER-1 S1).
    pub async fn start_with_env(
        state: PathBuf,
        base_url: &str,
        args: &[&str],
        extra_config: &str,
    ) -> LiveSession {
        Self::start_in_base(Some(state), base_url, args, extra_config, 0).await
    }

    /// The same, with creation flags of the caller's own: a test that needs a
    /// session without a console (a tool shell, a service) passes
    /// `DETACHED_PROCESS` (M4 review round 2, G3).
    pub async fn start_with_flags(
        mock: &Mock,
        args: &[&str],
        extra_config: &str,
        extra_flags: u32,
    ) -> LiveSession {
        Self::start_in(None, mock, args, extra_config, extra_flags).await
    }

    async fn start_in(
        reuse_state: Option<PathBuf>,
        mock: &Mock,
        args: &[&str],
        extra_config: &str,
        extra_flags: u32,
    ) -> LiveSession {
        let base = mock.base_url();
        Self::start_in_base(reuse_state, &base, args, extra_config, extra_flags).await
    }

    /// A session whose project has this `.kelir/config.toml` (M6).
    pub async fn start_with_mcp(
        mock: &Mock,
        args: &[&str],
        extra_config: &str,
        mcp_toml: &str,
    ) -> LiveSession {
        let base = mock.base_url();
        Self::start_in_base_mcp(None, &base, args, extra_config, 0, Some(mcp_toml)).await
    }

    async fn start_in_base(
        reuse_state: Option<PathBuf>,
        base_url: &str,
        args: &[&str],
        extra_config: &str,
        extra_flags: u32,
    ) -> LiveSession {
        Self::start_in_base_mcp(reuse_state, base_url, args, extra_config, extra_flags, None).await
    }

    async fn start_in_base_mcp(
        reuse_state: Option<PathBuf>,
        base_url: &str,
        args: &[&str],
        extra_config: &str,
        extra_flags: u32,
        mcp_toml: Option<&str>,
    ) -> LiveSession {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        if let Some(text) = mcp_toml {
            std::fs::create_dir_all(cwd.join(".kelir")).unwrap();
            std::fs::write(cwd.join(".kelir").join("config.toml"), text).unwrap();
        }
        let config = temp.path().join("config.toml");
        std::fs::write(&config, test_config(extra_config)).unwrap();
        let state = reuse_state.unwrap_or_else(|| temp.path().join("state"));

        let mut command = live_command(&cwd, &config, &state, base_url);
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let guard = Arc::new(TreeGuard::new("live session"));
        let mut child = guard
            .spawn_with(&mut command, extra_flags)
            .await
            .expect("spawn kelir");
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
        let buffer = Arc::new(Mutex::new(String::new()));
        let task = tokio::spawn(drain(stderr, buffer.clone()));

        let mut session = LiveSession {
            child,
            stdout: tokio::io::BufReader::new(stdout),
            stderr: buffer,
            stderr_task: task,
            _temp: temp,
            cwd,
            config,
            state,
            id: String::new(),
            session_dir: PathBuf::new(),
            events: Vec::new(),
            guard,
        };
        let first = session.next_event().await;
        // A fresh session starts with `session_started`; a resume continues
        // its log directly, and its id is the one it was given.
        match first["type"].as_str() {
            Some("session_started") => {
                session.id = first["id"].as_str().expect("session id").to_string();
            }
            _ => {
                let sessions =
                    kelir::paths::sessions_newest_first_in(&session.state.join("sessions"));
                assert_eq!(
                    sessions.len(),
                    1,
                    "a resume of one session, got {sessions:?}"
                );
                session.id = sessions[0].clone();
            }
        }
        session.session_dir = session.state.join("sessions").join(&session.id);
        session
    }

    /// The next NDJSON event the session wrote.
    pub async fn next_event(&mut self) -> Value {
        let pid = self.pid();
        let started = std::time::Instant::now();
        let mut line = String::new();
        let read = tokio::time::timeout(CEILING, async {
            use tokio::io::AsyncBufReadExt as _;
            self.stdout.read_line(&mut line).await
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "no event within {CEILING:?} (session pid {pid} alive={} after {:?}, seen so far: {:?}, stderr: {})",
                pid_alive_here(pid),
                started.elapsed(),
                self.events
                    .iter()
                    .map(|event| event["type"].clone())
                    .collect::<Vec<_>>(),
                self.stderr()
            )
        })
        .expect("read the session output");
        assert!(
            read > 0,
            "the session closed its output; stderr: {}",
            self.stderr()
        );
        let value: Value = serde_json::from_str(line.trim())
            .unwrap_or_else(|error| panic!("bad event line ({error}): {line}"));
        self.events.push(value.clone());
        value
    }

    /// Read until an event of this type arrives (never looks backwards).
    /// Wait for a call event of a given kind (`tool`, `bg`, `mcp`, `child`).
    pub async fn wait_for_call(&mut self, event_type: &str, kind: &str) -> Value {
        loop {
            let event = self.wait_for(event_type).await;
            if event["kind"] == kind {
                return event;
            }
        }
    }

    pub async fn wait_for(&mut self, event_type: &str) -> Value {
        loop {
            let event = self.next_event().await;
            assert_ne!(
                event["type"], "result",
                "the session ended before a {event_type} event"
            );
            if event["type"] == event_type {
                return event;
            }
        }
    }

    /// `kelir ctl <this session> …`.
    pub async fn ctl(&self, args: &[&str]) -> Ctl {
        let mut full = vec![self.id.as_str()];
        full.extend_from_slice(args);
        ctl_in(&self.guard, &self.state, &self.cwd, &self.config, &full).await
    }

    /// `kelir ctl latest …`: exercises session resolution.
    pub async fn ctl_latest(&self, args: &[&str]) -> Ctl {
        let mut full = vec!["latest"];
        full.extend_from_slice(args);
        ctl_in(&self.guard, &self.state, &self.cwd, &self.config, &full).await
    }

    pub fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    /// Let every call that is blocked on [`HOLD`] go on.
    pub fn release(&self) {
        std::fs::write(self.cwd.join("kelir-release"), b"go").expect("write the release file");
    }

    /// What to print when a wait times out: the pid and whether it is alive,
    /// the event types seen (stream and log), the last log lines and stderr.
    pub fn dump(&self) -> String {
        let pid = self.pid();
        let log = self.log();
        let tail: Vec<String> = log
            .iter()
            .rev()
            .take(12)
            .rev()
            .map(|event| event.to_string())
            .collect();
        format!(
            "session pid {pid} alive={}; streamed: {:?}; log has {} events, last:
{}
stderr: {}",
            pid_alive_here(pid),
            self.events
                .iter()
                .map(|event| event["type"].clone())
                .collect::<Vec<_>>(),
            log.len(),
            tail.join(
                "
"
            ),
            self.stderr()
        )
    }

    /// Poll the on-disk log until `check` holds over it; the panic carries
    /// [`LiveSession::dump`]. Use this instead of sleeping and then looking.
    pub async fn log_eventually(&self, what: &str, check: impl Fn(&[Value]) -> bool) {
        eventually(what, || check(&self.log()), || self.dump()).await;
    }

    /// The session process's pid, for a test that has to say whether it is
    /// still alive when an event is late.
    pub fn pid(&self) -> u32 {
        self.child.id().unwrap_or(0)
    }

    /// The session log as it is on disk right now.
    pub fn log(&self) -> Vec<Value> {
        std::fs::read_to_string(self.session_dir.join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect()
    }

    /// Ask the session to end and wait for it to exit.
    pub async fn end(self) -> (i32, String) {
        let answer = self.ctl(&["end"]).await;
        // The `end` ack and the process exiting race: the session answers from
        // its connection task, and on a busy runner the process is gone first,
        // so the socket that was accepted closes without an answer. The server
        // exits on `end` by design, so that one signature means the `end` *was*
        // honoured; every other refusal is still a failure.
        let already_gone = answer.code != 0
            && answer
                .stderr
                .contains("closed the connection without answering");
        assert!(
            answer.code == 0 || already_gone,
            "end was refused: stdout {:?} stderr {:?}",
            answer.stdout,
            answer.stderr
        );
        // A clean end may leave a promoted job running on purpose (that is the
        // M4 guarantee), and the test may still look at it. When something is
        // still alive, the guard is parked until this test *process* exits,
        // where the OS closes the job handle and kill-on-close reaps it; a test
        // that leaves nothing running is still terminated and asserted here.
        // A test that cancels its jobs must wait for their processes to exit
        // before `end()`, or nextest's leak check flags the parked guard (STEER-9).
        let parked = self.guard.active() > 0;
        let guard = self.guard.clone();
        let result = self.finish().await;
        if parked {
            std::mem::forget(guard);
        }
        result
    }

    /// Kill the session the way a crash or a `taskkill` would: the socket dies
    /// and the state dir stays behind, so a later `ctl` can be tried against it.
    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }

    /// Send the session a real interrupt, Unix only: `SIGINT` to the session's
    /// own pid.
    ///
    /// Windows is deliberately not here. A console control event cannot be
    /// aimed at one process: `GenerateConsoleCtrlEvent` addresses a *process
    /// group*, and a child started without `CREATE_NEW_PROCESS_GROUP` shares
    /// the test runner's group — sending Ctrl-Break would broadcast to every
    /// process on that console, the test harness and the shell that started it
    /// included (it did exactly that on 2026-11-19). The Windows side of the
    /// signal path is the handler registration itself, covered by the unit test
    /// in `src/signals.rs`; the wait that consumes it is covered here, since the
    /// `stop` verb takes the identical branch.
    #[cfg(unix)]
    pub async fn interrupt(&self) {
        let pid = self.pid();
        let seen = std::process::Command::new("kill")
            .args(["-INT", &pid.to_string()])
            .status()
            .expect("run kill");
        assert!(seen.success(), "kill -INT {pid} failed: {seen:?}");
    }

    /// Wait for the process to exit on its own.
    pub async fn finish(mut self) -> (i32, String) {
        let deadline = tokio::time::Instant::now() + CEILING;
        match tokio::time::timeout_at(deadline, self.child.wait()).await {
            Ok(Ok(status)) => {
                let code = status.code().unwrap_or(-1);
                let buffer = self.stderr.clone();
                let _ = self.stderr_task.await;
                (code, buffer.lock().unwrap().clone())
            }
            Ok(Err(error)) => panic!("wait for kelir: {error}"),
            Err(_) => {
                let _ = self.child.kill().await;
                panic!("kelir did not exit within {CEILING:?}; {}", self.dump());
            }
        }
    }
}

// ------------------------------------------------------- shared test helpers

/// The Python interpreter used by the scripted MCP / channel fixtures.
pub fn py() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

/// Count `sh` processes, on either OS, for the no-leak assertions.
pub fn shell_count() -> usize {
    #[cfg(windows)]
    let output = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq sh.exe", "/NH", "/FO", "CSV"])
        .output();
    #[cfg(not(windows))]
    let output = std::process::Command::new("ps").arg("-e").output();
    output
        .map(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            text.lines()
                .filter(|line| {
                    line.to_ascii_lowercase().contains("sh.exe") || line.contains(" sh ")
                })
                .count()
        })
        .unwrap_or(0)
}

/// Parse a config document through the real loader and return its error, if any.
pub fn config_err(text: &str) -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let path = dir.path().join("config.toml");
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    kelir::config::load(Some(&path))
        .map(|_| ())
        .map_err(|e| e.to_string())
}
