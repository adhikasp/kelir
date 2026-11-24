//! Background jobs: the shim, the status files, and the `kelir bg` CLI.
//! A job is one `kelir bg-shim` process (the same binary) that spawns the command, hands it the
//! output file directly and writes `bg/<id>.status` whenever its state changes.
//! The shim owns the command's process tree: on Unix both are in the shim's process group
//! (`setsid` for a job), on Windows the command is assigned to a named Job Object the shim keeps

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::util::path_string;

use super::*;

// ------------------------------------------------------------- how to launch

/// What the session (or `kelir bg run`) tells the shim.
pub struct Launch<'a> {
    /// The job root (`calls/` or the global `bg/`) the status file lives under.
    pub dir: PathBuf,
    /// The job id (the call's output dir name, or `bg_<ulid>`).
    pub id: String,
    /// The file the command's stdout and stderr go to.
    pub out: PathBuf,
    /// The command, run by `shell` (`shell[0]` plus its args, then the command).
    pub cmd: String,
    /// A file holding the command, when it is too long to pass on a command line.
    pub cmd_file: Option<PathBuf>,
    pub shell: Vec<String>,
    pub kind: &'a str,
    /// The session's pid: if it dies before a promotion, the shim takes the
    /// tree down with it.
    pub owner: Option<u32>,
    pub call: Option<String>,
    /// The Windows job object the session created for this call.
    pub job: Option<String>,
    /// The session socket, exported to the command as `KELIR_SOCK`.
    pub sock: Option<String>,
    /// The owning session dir, when the job root is not under one (a session-less job).
    pub session_dir: Option<PathBuf>,
    /// The session's socket token, exported to the command as `KELIR_TOKEN`
    /// : a job that reports itself over the socket must carry it.
    pub token: Option<String>,
    /// The environment the shim and its command get, when the starter is the session acting for a client:
    /// exactly the client's own (the session's environment is not the client's and must not leak into its
    pub env: Option<Vec<(String, String)>>,
    pub cwd: Option<PathBuf>,
}

/// The command that runs one shim. The caller may add platform grouping flags
/// (`ProcessGroup::configure`) or detach it ([`detach`]).
pub fn shim_command(launch: &Launch) -> Result<tokio::process::Command, String> {
    // Resolve with a PATH fallback: `current_exe` goes stale when the binary is replaced
    // under a live session, and a self-spawn must still find a runnable path.
    let binary = crate::util::self_binary()?;
    let mut command = crate::sys::runner::tokio_command(binary);
    if let Some(env) = &launch.env {
        command.env_clear();
        command.envs(env.iter().map(|(name, value)| (name, value)));
    }
    command
        .arg("bg-shim")
        .arg("--dir")
        .arg(&launch.dir)
        .arg("--id")
        .arg(&launch.id)
        .arg("--out")
        .arg(&launch.out);
    match &launch.cmd_file {
        // The command is a file: nothing long on this arg list either, so the
        // shim can be started whatever the command's size.
        Some(path) => {
            command.arg("--cmd-file").arg(path);
        }
        None => {
            command.arg("--cmd").arg(&launch.cmd);
        }
    }
    command.arg("--kind").arg(launch.kind);
    if let Some(shell) = launch.shell.first() {
        command.arg("--shell").arg(shell);
    }
    for arg in launch.shell.iter().skip(1) {
        command.arg("--shell-arg").arg(arg);
    }
    if let Some(owner) = launch.owner {
        command.arg("--owner").arg(owner.to_string());
    }
    if let Some(call) = &launch.call {
        command.arg("--call").arg(call);
    }
    if let Some(job) = &launch.job {
        command.arg("--job").arg(job);
    }
    if let Some(sock) = &launch.sock {
        command.arg("--sock").arg(sock);
    }
    if let Some(session_dir) = &launch.session_dir {
        command.arg("--session-dir").arg(session_dir);
    }
    // The token goes through the environment, never the argv: a command line is world-readable on Linux
    // (`/proc/<pid>/cmdline`) and a token in it would be a token anyone could read.
    if let Some(token) = &launch.token {
        command.env(crate::sys::trust::TOKEN_ENV, token);
    }
    // Where this session sits in the family of sessions: a command that
    // runs `kelir ls` or `kelir run --child` knows its root, parent and depth.
    for (name, value) in crate::features::children::lineage_env() {
        command.env(name, value);
    }
    // The command inherits this directory; the shim sets nothing of its own.
    if let Some(cwd) = &launch.cwd {
        command.current_dir(cwd);
    }
    // The shim needs no stdio of its own: the command writes straight to the
    // output file, and errors go into the status file.
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    Ok(command)
}

/// Detach a job's shim from this process, this process group and (on Windows) any enclosing
/// Job Object, so it outlives the session that started it.
pub fn detach(command: &mut tokio::process::Command) {
    cfg_select! {
        unix => {
            use std::os::unix::process::CommandExt as _;
            // `setsid` fails if the caller already leads a group; the fork/exec
            // child never does, so this is the spec's `setsid` and nothing else.
            unsafe {
                command.as_std_mut().pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        windows => {
            use std::os::windows::process::CommandExt as _;
            command.as_std_mut().creation_flags(
                crate::sys::proc::win::CREATE_NO_WINDOW
                    | crate::sys::proc::win::CREATE_NEW_PROCESS_GROUP
                    | crate::sys::proc::win::CREATE_BREAKAWAY_FROM_JOB,
            );
        }
        _ => {
            let _ = command;
        }
    }
}

/// What a `kelir bg run` inside a call asks its session to do on Windows: start the shim **outside**
/// the call's Job Object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub dir: String,
    /// The job id, when the caller already has one (a re-spawn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub out: String,
    pub cmd: String,
    pub shell: Vec<String>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sock: Option<String>,
    /// The token the client holds; the session compares it with its own before
    /// it starts anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The directory the client was in: a job started inside a call runs where
    /// the call does, and the session is not there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The client's environment: the job runs in it, not in the session's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
}

impl SpawnRequest {
    pub fn of(launch: &Launch) -> Self {
        Self {
            dir: path_string(&launch.dir),
            id: Some(launch.id.clone()),
            out: path_string(&launch.out),
            cmd: launch.cmd.clone(),
            shell: launch.shell.clone(),
            kind: launch.kind.to_string(),
            call: launch.call.clone(),
            job: launch.job.clone(),
            sock: launch.sock.clone(),
            token: launch.token.clone(),
            cwd: launch.cwd.as_ref().map(|cwd| path_string(cwd)),
            env: crate::envs::all(),
        }
    }

    pub fn pinned(mut self, sock: &str, token: &str, call_is_live: impl Fn(&str) -> bool) -> Self {
        self.kind = KIND_JOB.to_string();
        self.sock = Some(sock.to_string()).filter(|sock| !sock.trim().is_empty());
        self.token = Some(token.to_string()).filter(|token| !token.is_empty());
        self.job = None;
        self.id = None;
        self.call = self
            .call
            .take()
            .map(|call| call.trim().to_string())
            .filter(|call| !call.is_empty() && call_is_live(call));
        self.env = filter_spawn_env(std::mem::take(&mut self.env));
        self
    }

    pub fn launch(&self, id: &str) -> Launch<'_> {
        Launch {
            dir: PathBuf::from(&self.dir),
            id: id.to_string(),
            out: PathBuf::from(&self.out),
            cmd: self.cmd.clone(),
            cmd_file: None,
            shell: self.shell.clone(),
            kind: &self.kind,
            // A job belongs to nobody: it outlives whatever started it.
            owner: None,
            call: self.call.clone(),
            job: self.job.clone(),
            sock: self.sock.clone(),
            session_dir: None,
            token: self.token.clone(),
            env: Some(self.env.clone()),
            cwd: self.cwd.as_ref().map(PathBuf::from),
        }
    }
}

/// How many variables, and how many bytes of them, a spawn request may bring.
const SPAWN_ENV_MAX_VARS: usize = 4096;
const SPAWN_ENV_MAX_BYTES: usize = 512 * 1024;

/// The caller's environment as the session will pass it on: well-formed names
/// only, bounded, and without the variables the session sets itself.
fn filter_spawn_env(env: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut total = 0usize;
    let mut out = Vec::new();
    for (name, value) in env {
        let sound = !name.is_empty()
            && !name.contains(['=', '\0'])
            && !value.contains('\0')
            && !name.eq_ignore_ascii_case(crate::sys::trust::TOKEN_ENV)
            && !name.eq_ignore_ascii_case("KELIR_SOCK");
        total += name.len() + value.len();
        if !sound || out.len() >= SPAWN_ENV_MAX_VARS || total > SPAWN_ENV_MAX_BYTES {
            continue;
        }
        out.push((name, value));
    }
    out
}

/// Start the shim for `kelir bg run`.
pub async fn start_shim(launch: &Launch<'_>) -> Result<(), String> {
    #[cfg(windows)]
    {
        if let Err(denied) = spawn_shim(launch, true) {
            // The one case that can refuse a detached child: an enclosing Job Object without breakaway,
            // which is a call's job. Ask the session, which is outside it.
            if let Some(sock) = launch.sock.clone().filter(|sock| !sock.trim().is_empty()) {
                let args = serde_json::to_value(SpawnRequest::of(launch))
                    .map_err(|error| format!("cannot serialise the spawn request: {error}"))?;
                let request = crate::ipc::Request::new("bg-spawn", args, "job");
                if let Ok(answer) = crate::ipc::request_value(sock.trim(), &request).await
                    && answer.ok
                {
                    return Ok(());
                }
            }
            // No session to help (a bare shell inside some other job): the job runs inside whatever job
            // the caller had, which is the best that can be done — and the caller's job is not a call's.
            spawn_shim(launch, false).map_err(|error| format!("{error} ({denied})"))?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        spawn_shim(launch, true)
    }
}

fn spawn_shim(launch: &Launch<'_>, escape: bool) -> Result<(), String> {
    let mut command = shim_command(launch)?;
    #[cfg(unix)]
    {
        let _ = escape;
        detach(&mut command);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // `CREATE_NO_WINDOW`, not `DETACHED_PROCESS`: the shim is a console program, and a detached one leaves
        // its console-subsystem child (`bash.exe`) to allocate a fresh console — a new `conhost.exe` per job,
        let flags = crate::sys::proc::win::CREATE_NO_WINDOW
            | crate::sys::proc::win::CREATE_NEW_PROCESS_GROUP;
        command.as_std_mut().creation_flags(if escape {
            flags | crate::sys::proc::win::CREATE_BREAKAWAY_FROM_JOB
        } else {
            flags
        });
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = escape;
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("cannot start the shim: {error}"))
}

/// How the session answers a spawn: the id it minted, and nothing else.
#[derive(Debug, Clone, Serialize)]
pub struct Spawned {
    pub id: String,
}

/// The session's half of [`SpawnRequest`]: start the shim for a client and answer at once
/// (`kelir bg run` waits for the status file; the session must not block on a job).
pub fn spawn_shim_for_client(
    trust: &crate::sys::trust::Trust,
    spawn: &SpawnRequest,
) -> Result<Spawned, String> {
    if spawn.id.as_deref().is_some_and(|id| !id.trim().is_empty()) {
        return Err("bg-spawn: the session mints job ids; a request cannot name one".to_string());
    }
    let id = trust.claim(spawn.call.as_deref())?;
    let dir = trust.bg_dir();
    let shell_args: Vec<String> = spawn.shell.iter().skip(1).cloned().collect();
    // A job owns exactly one directory: its script goes there with its status and output.
    let script_dir = job_dir(&dir, &id);
    let cmd_file = match crate::sys::shell::input(&spawn.cmd, &shell_args, &script_dir, &id) {
        Ok(crate::sys::shell::ShellInput::Argument(_)) => None,
        Ok(crate::sys::shell::ShellInput::Script { path, .. }) => Some(path),
        Err(error) => {
            trust.forget(&id);
            return Err(format!("bg-spawn: {error}"));
        }
    };
    let mut launch = spawn.launch(&id);
    launch.out = out_path(&dir, &id);
    launch.dir = dir;
    launch.cmd_file = cmd_file;
    launch.job = Some(job_name("job", &id));
    // The session can itself be inside a job that forbids breakaway (a test harness, a scheduler) while
    // the client is in a call's job.
    let spawned = match spawn_shim(&launch, true) {
        Ok(()) => Ok(()),
        Err(_) => spawn_shim(&launch, false).map_err(|error| format!("bg-spawn: {error}")),
    };
    if let Err(error) = spawned {
        // Nothing was started: do not leave a job nobody can report for.
        trust.forget(&id);
        return Err(error);
    }
    Ok(Spawned { id })
}
