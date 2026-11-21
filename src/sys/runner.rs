//! The one child-process runner for gates, hooks, credential helpers and MCP servers.
//! A Spec says what to start, what it may inherit, how long it may run and how much output is
//! kept; the tree is always grouped (Unix process group, Windows job object) and killed on
//! timeout, drop or `kill`.
//! The bg shim keeps its own launch (it must outlive the session), documented in `sys/mod.rs`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::proc::ProcessGroup;

/// Variables a scrubbed child inherits, matched without regard to case (Windows has none).
/// Nothing here is a provider key, `KELIR_TOKEN`, `KELIR_SOCK` or a `key_env` name.
const INHERITED: &[&str] = &[
    "PATH",
    "PATHEXT",
    "HOME",
    "USER",
    "USERNAME",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "TERM",
    "TMPDIR",
    "TMP",
    "TEMP",
    "TZ",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "COMMONPROGRAMFILES",
    "COMMONPROGRAMFILES(X86)",
    "HOMEDRIVE",
    "HOMEPATH",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_RUNTIME_DIR",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "SSH_AUTH_SOCK",
];

/// The one environment allow-list. `LC_*` is allowed for locale; anything else outside the
/// list is withheld. Callers add their own variables through `Spec::env` / [`scrubbed_env`].
pub fn allowed_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.starts_with("LC_") || INHERITED.contains(&upper.as_str())
}

/// The allow-listed parent environment plus the caller's own variables. Gates, hooks, credential
/// helpers and MCP servers all get exactly this, never the whole parent environment.
pub fn scrubbed_env(extra: impl IntoIterator<Item = (String, String)>) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = crate::envs::all_os()
        .filter(|(key, _)| allowed_env(&key.to_string_lossy()))
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect();
    env.extend(extra);
    env
}

/// What a child inherits: a scrubbed allow-list (almost all callers) or the parent environment
/// (only for a credential helper whose `key_env` names a variable the user chose).
#[derive(Debug, Clone)]
pub enum Env {
    Scrubbed { extra: BTreeMap<String, String> },
    Inherit,
}

/// A child's stdin: nothing, a buffer written once, or a pipe the caller holds.
#[derive(Debug, Clone)]
pub enum Stdin {
    Null,
    Bytes(Vec<u8>),
    Piped,
}

/// One child process, described without naming a caller.
#[derive(Debug, Clone)]
pub struct Spec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Env,
    pub stdin: Stdin,
    pub timeout: Option<Duration>,
    pub output_cap: usize,
    pub stderr_cap: usize,
    pub no_window: bool,
    /// Windows job object name; a unique name per concurrent run.
    pub job_name: Option<String>,
    /// Unix: an `sh -c <guard> <program> <args…>` watcher that kills the process group if the
    /// parent dies. MCP servers use it; one-shot callers do not need it.
    pub unix_guard: Option<&'static str>,
}

impl Spec {
    /// A short-lived command: no timeout by default, 1 MiB of stdout, 8 KiB of stderr.
    pub fn new(program: impl Into<String>, args: impl IntoIterator<Item = String>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().collect(),
            cwd: None,
            env: Env::Scrubbed {
                extra: BTreeMap::new(),
            },
            stdin: Stdin::Null,
            timeout: None,
            output_cap: 1024 * 1024,
            stderr_cap: 8 * 1024,
            no_window: false,
            job_name: None,
            unix_guard: None,
        }
    }
}

/// A finished one-shot run. Output is capped: the rest was counted, not stored.
#[derive(Debug)]
pub struct Finished {
    pub status: Option<std::process::ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_over: bool,
    pub stderr_over: bool,
    pub timed_out: bool,
}

/// Apply a Spec's environment and stdio to a command. One place for the allow-list.
fn configure_env(command: &mut Command, env: &Env) {
    match env {
        Env::Scrubbed { extra } => {
            command.env_clear();
            for (key, value) in scrubbed_env(extra.iter().map(|(k, v)| (k.clone(), v.clone()))) {
                command.env(key, value);
            }
        }
        Env::Inherit => {}
    }
}

/// Build a command from a Spec. Only this function and `spawn` build one in `src/`.
pub fn command(spec: &Spec) -> Result<(Command, ProcessGroup), String> {
    let (program, args) = match spec.unix_guard {
        #[cfg(unix)]
        Some(guard) => (
            "sh".to_string(),
            std::iter::once("-c".to_string())
                .chain(std::iter::once(guard.to_string()))
                .chain(std::iter::once("kelir-child".to_string()))
                .chain(std::iter::once(spec.program.clone()))
                .chain(spec.args.iter().cloned())
                .collect::<Vec<_>>(),
        ),
        #[cfg(not(unix))]
        Some(_) => (spec.program.clone(), spec.args.clone()),
        None => (spec.program.clone(), spec.args.clone()),
    };
    let mut command = Command::new(program);
    command.args(&args);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    configure_env(&mut command, &spec.env);
    match spec.stdin {
        Stdin::Null => {
            command.stdin(Stdio::null());
        }
        Stdin::Bytes(_) | Stdin::Piped => {
            command.stdin(Stdio::piped());
        }
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    if spec.no_window {
        // A console-less parent's child would otherwise get its own visible console.
        command.creation_flags(crate::proc::win::CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = spec.no_window;
    let group = ProcessGroup::new();
    #[cfg(windows)]
    let group = match spec.job_name.as_deref() {
        Some(name) => {
            let job = crate::proc::win::Job::create_named(name, true)
                .ok_or_else(|| "cannot create a job object".to_string())?;
            group.with_job(job)
        }
        None => group,
    };
    #[cfg(not(windows))]
    let _ = &spec.job_name;
    group.configure(&mut command);
    Ok((command, group))
}

/// Spawn a grouped child and bind the group to it. Every caller starts its tree here.
pub fn start(command: &mut Command, group: &ProcessGroup) -> Result<tokio::process::Child, String> {
    let mut child = command
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("cannot start the process: {error}"))?;
    let pid = child
        .id()
        .ok_or_else(|| "the process exited at once".to_string())?;
    if let Err(error) = group.spawned(pid) {
        let _ = child.start_kill();
        return Err(error);
    }
    Ok(child)
}

/// Read to the end, keeping at most `max` bytes; the rest is read and thrown away so a chatty
/// child never blocks on a full pipe. The flag says it ran over.
pub async fn read_capped<R: AsyncRead + Unpin>(mut reader: R, max: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut over = false;
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if kept.len() < max {
                    let room = (max - kept.len()).min(n);
                    kept.extend_from_slice(&chunk[..room]);
                    over |= room < n;
                } else {
                    over = true;
                }
            }
        }
    }
    (kept, over)
}

/// Write a Spec's `Stdin::Bytes` to a started child in its own task; a child that never reads
/// stdin must not block the write. `Null`/`Piped` need nothing here.
pub fn feed_stdin(spec: &Spec, child: &mut tokio::process::Child) {
    if let Stdin::Bytes(bytes) = &spec.stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        let body = bytes.clone();
        tokio::spawn(async move {
            let _ = pipe.write_all(&body).await;
            let _ = pipe.shutdown().await;
        });
    } else {
        drop(child.stdin.take());
    }
}

/// Run one short-lived child, cap its output, and kill its tree on timeout or drop.
pub async fn run(spec: Spec) -> Result<Finished, String> {
    let (mut command, group) = command(&spec)?;
    let mut child = start(&mut command, &group)?;
    crate::sys::runner::feed_stdin(&spec, &mut child);
    let stdout = child.stdout.take().ok_or("no stdout pipe")?;
    let stderr = child.stderr.take().ok_or("no stderr pipe")?;
    let out_task = tokio::spawn(read_capped(stdout, spec.output_cap));
    let err_task = tokio::spawn(read_capped(stderr, spec.stderr_cap));
    let waited = match spec.timeout {
        Some(limit) => tokio::time::timeout(limit, child.wait()).await.ok(),
        None => Some(child.wait().await),
    };
    // The tree dies with the child whatever happened: a child that left a background process
    // behind leaves nothing.
    group.kill();
    let waited = match waited {
        Some(Ok(status)) => Some(status),
        Some(Err(error)) => return Err(format!("cannot wait for the process: {error}")),
        None => {
            out_task.abort();
            err_task.abort();
            return Ok(Finished {
                status: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
                stdout_over: false,
                stderr_over: false,
                timed_out: true,
            });
        }
    };
    let (stdout, stdout_over) = out_task.await.unwrap_or_default();
    let (stderr, stderr_over) = err_task.await.unwrap_or_default();
    Ok(Finished {
        status: waited,
        stdout,
        stderr,
        stdout_over,
        stderr_over,
        timed_out: false,
    })
}

/// A long-lived child with piped stdio. The tree is killed when the client drops it.
#[derive(Debug)]
pub struct Running {
    pub child: tokio::process::Child,
    pub stdin: tokio::process::ChildStdin,
    pub stdout: tokio::process::ChildStdout,
    pub stderr: tokio::process::ChildStderr,
    pub group: ProcessGroup,
}

impl Running {
    pub fn kill(&self) {
        self.group.kill();
    }
}

/// Spawn a long-lived child (an MCP server): stdin/stdout/stderr stay piped.
pub fn spawn(spec: Spec) -> Result<Running, String> {
    let (mut command, group) = command(&spec)?;
    let mut child = start(&mut command, &group)?;
    let stdin = child.stdin.take().ok_or("no stdin pipe")?;
    let stdout = child.stdout.take().ok_or("no stdout pipe")?;
    let stderr = child.stderr.take().ok_or("no stderr pipe")?;
    Ok(Running {
        child,
        stdin,
        stdout,
        stderr,
        group,
    })
}

/// A synchronous `Command` for a program: `.cmd`/`.bat` shims on Windows run through `cmd.exe /C`;
/// everything else starts directly. Only this module builds a process command in `src/`.
pub fn std_command(path: &std::path::Path) -> std::process::Command {
    #[cfg(windows)]
    {
        let extension = path
            .extension()
            .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if extension == "cmd" || extension == "bat" {
            use std::os::windows::process::CommandExt as _;
            let shell = crate::envs::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into());
            let mut command = std::process::Command::new(shell);
            command.arg("/C");
            // `raw_arg` so the program's own quoting survives.
            command.raw_arg(format!("\"{}\"", path.display()));
            return command;
        }
    }
    std::process::Command::new(path)
}

/// A synchronous command by program name (identity probes).
pub fn std_command_name(program: &str) -> std::process::Command {
    std::process::Command::new(program)
}
pub fn start_std(
    command: &mut std::process::Command,
    group: &crate::proc::StdGroup,
) -> Result<std::process::Child, String> {
    let child = command
        .spawn()
        .map_err(|error| format!("cannot start the process: {error}"))?;
    group.spawned(child.id());
    Ok(child)
}

/// A `tokio` command from a program name. The only place a tokio child command is built.
pub fn tokio_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    Command::new(program)
}

/// A fresh named job object for a child tree (Windows); a no-op elsewhere.
pub fn job(name: &str) -> Result<ProcessGroup, String> {
    let group = ProcessGroup::new();
    #[cfg(windows)]
    {
        let handle = crate::proc::win::Job::create_named(name, true)
            .ok_or_else(|| "cannot create a job object".to_string())?;
        Ok(group.with_job(handle))
    }
    #[cfg(not(windows))]
    {
        let _ = name;
        Ok(group)
    }
}

/// Configure a caller-built command with the tree discipline and start it.
pub fn start_with_group(
    command: &mut Command,
    group: &ProcessGroup,
) -> Result<tokio::process::Child, String> {
    group.configure(command);
    start(command, group)
}

/// Start a detached gate guard: a short-lived `kelir gate-guard` that outlives a killed session.
#[cfg(unix)]
pub fn start_guard(args: &[String]) -> Option<std::process::Child> {
    let exe = crate::util::self_binary().ok()?;
    let mut command = std::process::Command::new(exe);
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
    command.spawn().ok()
}

/// A synchronous command by program name (identity probes).
/// The caller configures the command's grouping flags itself.
pub fn start_grouped(command: &mut Command) -> Result<tokio::process::Child, String> {
    let child = command
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("cannot start the process: {error}"))?;
    child
        .id()
        .ok_or_else(|| "the process exited at once".to_string())?;
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn one_allow_list_keeps_provider_keys_and_tokens_out() {
        for secret in [
            "OPENROUTER_API_KEY",
            "KELIR_GO_API_KEY",
            "KELIR_TOKEN",
            "KELIR_SOCK",
            "MY_CUSTOM_KEY_ENV",
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
        ] {
            assert!(!allowed_env(secret), "{secret} must not reach a child");
        }
        for ordinary in [
            "PATH",
            "HOME",
            "TEMP",
            "SystemRoot",
            "SSH_AUTH_SOCK",
            "LC_ALL",
        ] {
            assert!(allowed_env(ordinary), "{ordinary}");
        }
        let env = scrubbed_env([("KELIR_GATE_EVENT".into(), "before_tool".into())]);
        assert_eq!(
            env.get("KELIR_GATE_EVENT").map(String::as_str),
            Some("before_tool")
        );
        for secret in ["KELIR_TOKEN", "KELIR_SOCK", "OPENROUTER_API_KEY"] {
            assert!(
                !env.contains_key(secret),
                "{secret} reached a scrubbed child"
            );
        }
    }

    #[tokio::test]
    async fn a_timed_out_child_leaves_no_tree_and_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        let flag = dir.path().join("flag");
        #[cfg(unix)]
        let script = format!("(sleep 2; touch '{}') & sleep 30", flag.to_string_lossy());
        #[cfg(windows)]
        let script = format!(
            "cmd /C \"ping -n 30 127.0.0.1 > NUL & echo alive > '{}'\"",
            flag.to_string_lossy()
        );
        let spec = Spec {
            timeout: Some(Duration::from_millis(400)),
            ..Spec::new(
                if cfg!(windows) { "cmd" } else { "sh" },
                if cfg!(windows) {
                    vec!["/C".into(), script]
                } else {
                    vec!["-c".into(), script]
                },
            )
        };
        let started = Instant::now();
        let finished = run(spec).await.unwrap();
        assert!(finished.timed_out, "the child did not time out");
        assert!(started.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!flag.exists(), "the child's tree outlived its timeout");
    }
}
