//! Sandbox, process tree guard and environment scrubbing for the integration tests.
#![allow(dead_code)]

use super::*;

// ------------------------------------------------------- a reusable sandbox

/// One state dir, cwd and config shared by several `kelir` invocations, so a
/// test can crash a run and resume the same session.
pub struct Sandbox {
    _temp: Option<tempfile::TempDir>,
    pub cwd: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    /// Every process this test starts lives in one Job Object, so a failed or
    /// panicking test cannot leave a tree behind (AGENTS.md hard rule).
    pub guard: std::sync::Arc<TreeGuard>,
}

/// Holds a test's process trees so they cannot outlive it.
///
/// The dev box is someone's desktop: a leaked `kelir`/`bash` (or a leaked
/// console window) is a real mess, and an earlier round leaked hundreds
/// (M4 review round 2, N8). On Windows every child the harness starts is
/// created suspended, put in one kill-on-close Job Object and resumed; dropping
/// the guard terminates the job and asserts nothing survived. On Unix the guard
/// carries the same contract and relies on kelir's own tree rules (tests there
/// spawn far fewer processes and no consoles).
pub struct TreeGuard {
    label: String,
    #[cfg(windows)]
    job: kelir::sys::proc::win::Job,
    #[cfg(not(windows))]
    _private: (),
}

impl TreeGuard {
    pub fn new(label: &str) -> TreeGuard {
        #[cfg(windows)]
        {
            let name = format!(
                "kelir-test-{}-{}",
                std::process::id(),
                kelir::features::bg::new_id()
            );
            let job = kelir::sys::proc::win::Job::create_named(&name, true)
                .unwrap_or_else(|| panic!("cannot create the test's job object {name}"));
            TreeGuard {
                label: label.to_string(),
                job,
            }
        }
        #[cfg(not(windows))]
        {
            TreeGuard {
                label: label.to_string(),
                _private: (),
            }
        }
    }

    /// Spawn a command whose whole tree stays in the guard.
    pub async fn spawn(
        &self,
        command: &mut tokio::process::Command,
    ) -> std::io::Result<tokio::process::Child> {
        self.spawn_with(command, 0).await
    }

    /// The same, with creation flags of the caller's own (`extra` is OR-ed into
    /// the guard's `CREATE_SUSPENDED`).
    pub async fn spawn_with(
        &self,
        command: &mut tokio::process::Command,
        extra: u32,
    ) -> std::io::Result<tokio::process::Child> {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command
                .as_std_mut()
                .creation_flags(extra | kelir::sys::proc::win::CREATE_SUSPENDED);
            let child = command.spawn()?;
            if let Some(pid) = child.id() {
                self.adopt(pid);
            }
            Ok(child)
        }
        #[cfg(not(windows))]
        {
            let _ = extra;
            command.spawn()
        }
    }

    /// Spawn a synchronous command whose whole tree stays in the guard.
    pub fn spawn_sync(
        &self,
        command: &mut std::process::Command,
    ) -> std::io::Result<std::process::Child> {
        self.spawn_sync_with(command, 0)
    }

    /// The same, with creation flags of the caller's own (`extra` is OR-ed into
    /// the guard's `CREATE_SUSPENDED`).
    pub fn spawn_sync_with(
        &self,
        command: &mut std::process::Command,
        extra: u32,
    ) -> std::io::Result<std::process::Child> {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(extra | kelir::sys::proc::win::CREATE_SUSPENDED);
            let child = command.spawn()?;
            self.adopt(child.id());
            Ok(child)
        }
        #[cfg(not(windows))]
        {
            let _ = extra;
            command.spawn()
        }
    }

    /// How many processes are still alive in the guard.
    pub fn active(&self) -> u32 {
        #[cfg(windows)]
        {
            self.job.active_processes().unwrap_or(0)
        }
        #[cfg(not(windows))]
        {
            0
        }
    }

    #[cfg(windows)]
    fn adopt(&self, pid: u32) {
        // A suspended child that cannot enter the job must not be left
        // suspended: say so loudly, then let it run outside the guard.
        match self
            .job
            .assign(pid)
            .and_then(|()| kelir::sys::proc::win::resume(pid).map_err(|error| error.to_string()))
        {
            Ok(()) => {}
            Err(error) => {
                eprintln!(
                    "test harness: cannot contain pid {pid} in {}: {error}",
                    self.label
                );
                let _ = kelir::sys::proc::win::resume(pid);
            }
        }
    }
}

impl Drop for TreeGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            self.job.terminate_job();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while let Some(active) = self.job.active_processes() {
                if active == 0 {
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    let message = format!(
                        "the test leaked {active} process(es) in {} — kill them by pid",
                        self.label
                    );
                    if std::thread::panicking() {
                        eprintln!("LEAK: {message}");
                    } else {
                        panic!("{message}");
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

#[derive(Debug)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
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

    pub fn result(&self) -> Option<Value> {
        self.json_lines()
            .into_iter()
            .find(|line| line["type"] == "result")
    }
}

impl Sandbox {
    pub fn new(extra_config: &str) -> Sandbox {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        let config = temp.path().join("config.toml");
        std::fs::write(&config, test_config(extra_config)).unwrap();
        let state = temp.path().join("state");
        Sandbox {
            _temp: Some(temp),
            cwd,
            config,
            state,
            guard: std::sync::Arc::new(TreeGuard::new("sandbox")),
        }
    }

    /// The same dirs as a running [`LiveSession`], for a second process (a
    /// resume) on its log.
    pub fn from_live(live: &LiveSession) -> Sandbox {
        Sandbox {
            _temp: None,
            cwd: live.cwd.clone(),
            config: live.config.clone(),
            state: live.state.clone(),
            guard: live.guard.clone(),
        }
    }

    /// Run the real binary against `base_url`, capturing stdout and stderr.
    pub async fn kelir(&self, base_url: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.kelir_with_ceiling(base_url, args, env, CEILING).await
    }

    /// The same as [`Self::kelir`] with an explicit ceiling, for stress tests
    /// whose work legitimately outgrows the shared 90 s one.
    pub async fn kelir_with_ceiling(
        &self,
        base_url: &str,
        args: &[&str],
        env: &[(&str, &str)],
        ceiling: Duration,
    ) -> Output {
        let mut command = live_command(&self.cwd, &self.config, &self.state, base_url);
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let child = self.guard.spawn(&mut command).await.expect("run kelir");
        let output = tokio::time::timeout(ceiling, child.wait_with_output())
            .await
            .expect("kelir did not finish within the ceiling")
            .expect("run kelir");
        Output {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// A synchronous command with this sandbox's environment: for a test that
    /// has to create the process with flags of its own (a console-less parent).
    pub fn kelir_command(&self, base_url: &str) -> std::process::Command {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_kelir"));
        scrub_session_env_std(&mut command);
        command
            .current_dir(&self.cwd)
            .env("KELIR_CONFIG", &self.config)
            .env("KELIR_CONFIG_DIR", self.state.join("no-config"))
            .env("KELIR_STATE_DIR", &self.state)
            .env("KELIR_BASE_URL", base_url)
            .env("KELIR_HOME", self.state.join("home"))
            .env("OPENROUTER_API_KEY", "test-openrouter-key")
            .env("KELIR_GO_API_KEY", "test-go-key");
        command
    }

    /// Cancel a job from synchronous code — a test's `Drop` — so a failing
    /// test cannot leak an infinite job and its shim (M4 review round 2, N8).
    pub fn cancel_now(&self, id: &str) {
        let mut command = self.kelir_command("http://127.0.0.1:9/v1");
        let _ = self
            .guard
            .spawn_sync(
                command
                    .args(["bg", "cancel", id])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .and_then(|mut child| child.wait());
    }

    pub fn session_ids(&self) -> Vec<String> {
        kelir::paths::sessions_newest_first_in(&self.state.join("sessions"))
    }

    pub fn only_session(&self) -> String {
        let ids = self.session_ids();
        assert_eq!(ids.len(), 1, "expected one session, got {ids:?}");
        ids[0].clone()
    }

    pub fn session_dir(&self, id: &str) -> PathBuf {
        self.state.join("sessions").join(id)
    }

    /// The log as it is on disk, skipping a line a crash left torn (a resume
    /// reports those; the test still wants the rest).
    pub fn log(&self, id: &str) -> Vec<Value> {
        std::fs::read_to_string(self.session_dir(id).join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// Wait until every process the session `id` recorded for its calls (the
    /// shim and the command, from each job's `<id>.status`) is gone. A crashed session's
    /// shim notices its dead owner and takes the tree down, but on a loaded box
    /// that takes longer than any fixed sleep the tests used to guess (STEER-9).
    /// Returns false if something recorded is still alive at the ceiling (a
    /// recycled pid, say); the caller's own assertions then speak.
    pub async fn trees_gone(&self, id: &str) -> bool {
        let bg = kelir::paths::calls_dir(&self.session_dir(id));
        let pids = || -> Vec<u32> {
            let mut out = Vec::new();
            for entry in status_files(&bg) {
                let Ok(text) = std::fs::read_to_string(&entry) else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                for key in ["shim", "pid"] {
                    if let Some(pid) = value[key].as_u64() {
                        out.push(pid as u32);
                    }
                }
            }
            out
        };
        // Nothing recorded yet: the crash may have come before the shim wrote
        // its status, and then there is no pid to wait for. That one case keeps
        // a short grace (the old fixed wait); it cannot be event-driven.
        if pids().is_empty() {
            tokio::time::sleep(Duration::from_millis(750)).await;
        }
        eventually_within(CEILING, || !pids().into_iter().any(pid_alive_here)).await
    }

    /// The lines of a file the tools wrote, 0 when it does not exist.
    pub fn lines_in(&self, name: &str) -> usize {
        std::fs::read_to_string(self.cwd.join(name))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }
}

/// The variables a child kelir must not inherit from the kelir session (or the
/// shell) that runs the suite.
///
/// `KELIR_SESSION_DIR`/`KELIR_SOCK`/`KELIR_CALL_ID`/`KELIR_BIN` are what kelir
/// exports to a bash child: kelir dogfoods itself, so a suite run from inside a
/// kelir session would otherwise hand its own `bg/` directory and socket to the
/// child, and `bg out` would read *that* session's jobs (M4 review).
/// `KELIR_PROMOTE_AFTER` would silently change promotion timing, `KELIR_HERDR`
/// and `TERM_PROGRAM` would make the binary think it is in a pane, and
/// `KELIR_TEST_CRASH_AT`/`KELIR_TEST_PANIC` are test hooks (M5-A fixes N2). The rest
/// of `KELIR_*` is removed as well, by prefix, so a new variable cannot quietly
/// leak in. `HC_TERMINAL_ID` (the Herdr marker) goes too.
fn scrubbed_keys() -> Vec<String> {
    let mut keys: Vec<String> = std::env::vars_os()
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .filter(|key| key.starts_with("KELIR_"))
        .collect();
    for key in ["TERM_PROGRAM", "HC_TERMINAL_ID"] {
        keys.push(key.to_string());
    }
    keys.sort();
    keys.dedup();
    keys
}

pub fn scrub_session_env(command: &mut tokio::process::Command) {
    for key in scrubbed_keys() {
        command.env_remove(key);
    }
}

fn scrub_session_env_std(command: &mut std::process::Command) {
    for key in scrubbed_keys() {
        command.env_remove(key);
    }
}

/// The same scrub for a command a test builds itself (either flavour).
pub fn scrub_std(command: &mut std::process::Command) {
    scrub_session_env_std(command);
}

pub fn scrub_tokio(command: &mut tokio::process::Command) {
    scrub_session_env(command);
}

/// The `portable_pty` flavour of the same scrub, for the TUI tests (which
/// spawn through a pty rather than a `Command`).
pub fn scrub_pty(command: &mut portable_pty::CommandBuilder) {
    for key in scrubbed_keys() {
        command.env_remove(key);
    }
}

pub(super) fn live_command(
    cwd: &PathBuf,
    config: &PathBuf,
    state: &PathBuf,
    base_url: &str,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_kelir"));
    scrub_session_env(&mut command);
    command
        .current_dir(cwd)
        .env("KELIR_CONFIG", config)
        .env("KELIR_CONFIG_DIR", state.join("no-config"))
        .env("KELIR_STATE_DIR", state)
        .env("KELIR_BASE_URL", base_url)
        .env("KELIR_HOME", state.join("home"))
        .env("OPENROUTER_API_KEY", "test-openrouter-key")
        .env("KELIR_GO_API_KEY", "test-go-key");
    command
}

/// `kelir ctl` against a state dir, with no live session in the test harness.
pub async fn ctl_in(
    guard: &TreeGuard,
    state: &Path,
    cwd: &Path,
    config: &Path,
    args: &[&str],
) -> Ctl {
    let mut command = live_command(
        &cwd.to_path_buf(),
        &config.to_path_buf(),
        &state.to_path_buf(),
        "http://127.0.0.1:9/v1",
    );
    command.arg("ctl");
    for arg in args {
        command.arg(arg);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = guard.spawn(&mut command).await.expect("spawn ctl");
    let output = tokio::time::timeout(CEILING, child.wait_with_output())
        .await
        .expect("ctl did not finish within the ceiling")
        .expect("run ctl");
    Ctl {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

pub(super) async fn drain(mut stderr: tokio::process::ChildStderr, buffer: Arc<Mutex<String>>) {
    use tokio::io::AsyncReadExt as _;
    let mut text = Vec::new();
    let _ = stderr.read_to_end(&mut text).await;
    buffer
        .lock()
        .unwrap()
        .push_str(&String::from_utf8_lossy(&text));
}

/// Request snapshots: refactor guards, not a compatibility promise.
///
/// Each guarded test calls `assert_requests_snapshot` with a stable name. The
/// first run writes `tests/it/snapshots/<name>.jsonl`; a later run compares the
/// exact request bodies (one JSON value per line) and fails with a diff.
/// `UPDATE_SNAPSHOTS=1` deliberately regenerates them (format-changing PRs do
/// that and say so in their description).
fn snapshot_value(value: &mut serde_json::Value) {
    // Volatile: temp dirs and generated ids. Everything else (system text,
    // tool schemas, message order, cache markers) is compared byte for byte.
    fn visit(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(text) => {
                // Temp paths differ by OS: /tmp/.tmpXXXXXX on Linux,
                // /var/folders/.../T/.tmpXXXXXX on macOS, a per-user
                // AppData\Local\Temp\.tmpXXXXXX on Windows. Replace the
                // whole OS-specific root + random component with one token,
                // preserving any `cwd: ` or `- ` prefix on the same line.
                let mut cursor = 0usize;
                while let Some(offset) = text[cursor..].find(".tmp") {
                    let start = cursor + offset;
                    let after = start + ".tmp".len();
                    let end = text[after..]
                        .find(['/', '\\', '\n', ' ', '"'])
                        .map(|n| after + n)
                        .unwrap_or(text.len());
                    let random = &text[after..end];
                    if random.len() < 4 || !random.chars().all(|c| c.is_ascii_alphanumeric()) {
                        cursor = after;
                        continue;
                    }
                    let before = &text[..start];
                    let line_start = before.rfind('\n').map(|n| n + 1).unwrap_or(0);
                    let line = &text[line_start..start];
                    let prefix = if let Some(pos) = line.find("cwd: ") {
                        line_start + pos + "cwd: ".len()
                    } else if let Some(pos) = line.rfind("- ") {
                        line_start + pos + 2
                    } else if let Some(pos) = line.find(": ") {
                        line_start + pos + 2
                    } else {
                        line_start
                    };
                    text.replace_range(prefix..end, "<TEMP>");
                    cursor = prefix + "<TEMP>".len();
                }
                // Platform facts and today's date are not request-format
                // changes: normalise them away so one snapshot works on all
                // three CI legs and on any calendar day.
                for os in ["linux", "windows", "macos", "darwin"] {
                    let needle = format!("os: {os}");
                    while let Some(offset) = text.find(&needle) {
                        text.replace_range(offset..offset + needle.len(), "os: <OS>");
                    }
                }
                if let Some(pos) = text.find("date: ") {
                    let start = pos + "date: ".len();
                    let end = text[start..]
                        .find('\n')
                        .map(|n| start + n)
                        .unwrap_or(text.len());
                    if end - start == 10 {
                        text.replace_range(start..end, "<DATE>");
                    }
                }
                // Generated child ids are `chd_` plus hex; normalise them the
                // same way as session ids (they appear in tool results and
                // wake messages).
                let mut cursor = 0usize;
                while let Some(offset) = text[cursor..].find("chd_") {
                    let start = cursor + offset;
                    let end = text[start..]
                        .find(|c: char| {
                            !(c.is_ascii_alphanumeric() || c == '_' || c == '<' || c == '>')
                        })
                        .map(|n| start + n)
                        .unwrap_or(text.len());
                    if &text[start..end] == "chd_<ID>" {
                        cursor = end;
                        continue;
                    }
                    text.replace_range(start..end, "chd_<ID>");
                    cursor = start + "chd_<ID>".len();
                }
                // Generated session ids are ses_ plus alphanumerics; keep any
                // surrounding punctuation (quotes, separators) verbatim.
                let mut cursor = 0usize;
                while let Some(offset) = text[cursor..].find("ses_") {
                    let start = cursor + offset;
                    let end = text[start..]
                        .find(|c: char| {
                            !(c.is_ascii_alphanumeric() || c == '_' || c == '<' || c == '>')
                        })
                        .map(|n| start + n)
                        .unwrap_or(text.len());
                    if &text[start..end] == "ses_<ID>" {
                        cursor = end;
                        continue;
                    }
                    text.replace_range(start..end, "ses_<ID>");
                    cursor = start + "ses_<ID>".len();
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(visit),
            serde_json::Value::Object(map) => map.values_mut().for_each(visit),
            _ => {}
        }
    }
    visit(value);
}

pub fn assert_requests_snapshot(name: &str, requests: &[Recorded]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/it/snapshots")
        .join(format!("{name}.jsonl"));
    let actual = requests
        .iter()
        .map(|request| {
            let mut body = request.body.clone();
            snapshot_value(&mut body);
            serde_json::to_string(&body).expect("serialize request")
        })
        .collect::<Vec<_>>()
        .join("\n");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().expect("snapshot directory")).expect("snapshot dir");
        std::fs::write(&path, format!("{actual}\n")).expect("write snapshot");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing snapshot {} (run UPDATE_SNAPSHOTS=1)",
            path.display()
        )
    });
    // The committed snapshots were generated on Linux. Re-run the same
    // normaliser on their parsed JSON so a field that was volatile when the
    // snapshot was written (or a platform the generator never saw) cannot
    // produce a cross-OS mismatch.
    let expected = expected
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut value: serde_json::Value =
                serde_json::from_str(line).expect("snapshot line is JSON");
            snapshot_value(&mut value);
            serde_json::to_string(&value).expect("serialize snapshot")
        })
        .collect::<Vec<_>>()
        .join("\n");
    if expected != actual {
        let mut message = String::from("request snapshot changed: ");
        message.push_str(&path.to_string_lossy());
        let expected_lines: Vec<&str> = expected.lines().collect();
        let actual_lines: Vec<&str> = actual.lines().collect();
        for (index, (old, new)) in expected_lines.iter().zip(actual_lines.iter()).enumerate() {
            if old != new {
                let at = (0..old.len().min(new.len()))
                    .find(|&i| old.as_bytes()[i] != new.as_bytes()[i])
                    .unwrap_or(old.len().min(new.len()));
                let from = at.saturating_sub(40);
                message.push_str(&format!(
                    " request{}@{}:[-{}|{}]{}",
                    index + 1,
                    at,
                    &old[from..(at + 80).min(old.len())],
                    &new[from..(at + 80).min(new.len())],
                    if old.len() != new.len() { " LEN" } else { "" }
                ));
                break;
            }
        }
        if expected_lines.len() != actual_lines.len() {
            message.push_str(&format!(
                " line-count {}->{}",
                expected_lines.len(),
                actual_lines.len()
            ));
        }
        panic!("{message}");
    }
}

#[cfg(test)]
mod snapshot_normalisation_tests {
    use super::snapshot_value;
    use serde_json::json;

    fn normalise(text: &str) -> String {
        let mut value = json!({"text": text});
        snapshot_value(&mut value);
        value["text"].as_str().expect("string").to_string()
    }

    #[test]
    fn temp_roots_and_os_lines_are_stable_across_platforms() {
        let cases = [
            (
                "- /tmp/.tmpAbCd12/work/.claude/skills\nos: linux",
                "- <TEMP>/work/.claude/skills\nos: <OS>",
            ),
            (
                "- /var/folders/ab/cd/T/.tmpXyZ987/state/home/.claude/skills\nos: macos",
                "- <TEMP>/state/home/.claude/skills\nos: <OS>",
            ),
            (
                "- C:/Users/RUNNER~1/AppData/Local/Temp/.tmpQwEr45/work/.kelir/skills\nos: windows",
                "- <TEMP>/work/.kelir/skills\nos: <OS>",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(normalise(input), expected, "input: {input}");
        }
        assert_eq!(
            normalise("cwd: C:/Users/RUNNER~1/AppData/Local/Temp/.tmpQwEr45/work"),
            "cwd: <TEMP>/work"
        );
        assert_eq!(
            normalise("os: linux\ndate: 2026-11-21"),
            "os: <OS>\ndate: <DATE>"
        );
    }

    #[test]
    fn windows_and_linux_prompts_normalise_to_the_same_snapshot() {
        let linux = "- Skills are **pull**: read a `SKILL.md`. Directories:   - /tmp/.tmpAbCd12/state/home/.claude/skills\n  - /tmp/.tmpAbCd12/work/.claude/skills\n/tmp/.tmpAbCd12/work\nos: linux";
        let windows = "- Skills are **pull**: read a `SKILL.md`. Directories:   - C:/Users/RUNNER~1/AppData/Local/Temp/.tmpQwEr45/state/home/.claude/skills\n  - C:/Users/RUNNER~1/AppData/Local/Temp/.tmpQwEr45/work/.claude/skills\nC:/Users/RUNNER~1/AppData/Local/Temp/.tmpQwEr45/work\nos: windows";
        assert_eq!(normalise(linux), normalise(windows));
    }

    #[test]
    fn generated_ids_are_normalised_but_user_text_is_left_alone() {
        let text = "child ses_abc123 done; chd_deadbeef settled; user said ses_my_literal";
        assert_eq!(
            normalise(text),
            "child ses_<ID> done; chd_<ID> settled; user said ses_<ID>"
        );
    }
}
