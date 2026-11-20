//! Job notify hooks (M9): a command run when a job ends.
//!
//! Gate-like: the command runs through the configured shell with a scrubbed
//! environment (the platform allow-list, never a provider key or a session
//! token), at most [`TIMEOUT`] seconds, its whole tree killed when it ends, and
//! whatever it does or fails to do never changes the job's exit code. A hook
//! that fails is one line in STATUS (`notify_errors`).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::proc::ProcessGroup;

/// How long a hook may run.
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// How much of a failing hook's stderr is kept.
const STDERR_TAIL: usize = 200;

fn timeout() -> Duration {
    crate::envs::var("KELIR_NOTIFY_TIMEOUT_MS")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(TIMEOUT)
}

/// The hook's environment: the allow-list, then the job's own variables.
pub fn scrubbed(command: &mut Command, job_env: &[(&str, &str)]) {
    command.env_clear();
    for (key, value) in crate::envs::all_os() {
        if crate::gates::allowed_env(&key.to_string_lossy()) {
            command.env(key, value);
        }
    }
    for (key, value) in job_env {
        command.env(key, value);
    }
}

/// Run every hook, in order. Returns one line per failure.
pub async fn run_hooks(
    hooks: &[String],
    shell: &[String],
    cwd: &Path,
    dir: &Path,
    job_env: &[(&str, &str)],
) -> Vec<String> {
    let mut errors = Vec::new();
    for (index, hook) in hooks.iter().enumerate() {
        if let Err(why) = run_one(hook, shell, cwd, dir, index, job_env).await {
            eprintln!("kelir: job: notify hook {} failed: {why}", index + 1);
            errors.push(format!("hook {}: {why}", index + 1));
        }
    }
    errors
}

async fn run_one(
    hook: &str,
    shell: &[String],
    cwd: &Path,
    dir: &Path,
    index: usize,
    job_env: &[(&str, &str)],
) -> Result<(), String> {
    let Some(program) = shell.first() else {
        return Err("no shell configured".into());
    };
    let shell_args: Vec<String> = shell.iter().skip(1).cloned().collect();
    let line = crate::gates::adapt_for_shell(hook, program);
    let scripts = dir.join("notify");
    std::fs::create_dir_all(&scripts).map_err(|error| error.to_string())?;
    let input = crate::shellcmd::input(&line, &shell_args, &scripts, &format!("n{index}"))?;
    let mut command = Command::new(program);
    match &input {
        crate::shellcmd::ShellInput::Argument(text) => {
            command.args(&shell_args).arg(text);
        }
        crate::shellcmd::ShellInput::Script { path, keep_switch } => {
            command.args(crate::shellcmd::script_args(
                &shell_args,
                keep_switch.as_deref(),
                path,
            ));
        }
    }
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    scrubbed(&mut command, job_env);

    let group = ProcessGroup::new();
    #[cfg(windows)]
    let group = {
        let name = crate::bg::job_name(
            "notify",
            &format!(
                "{}-{}",
                std::process::id(),
                ulid::Ulid::new().to_string().to_lowercase()
            ),
        );
        let job = crate::proc::win::Job::create_named(&name, true)
            .ok_or_else(|| "cannot create a job object".to_string())?;
        group.with_job(job)
    };
    group.configure(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start it: {error}"))?;
    let Some(pid) = child.id() else {
        return Err("it exited at once".into());
    };
    if let Err(error) = group.spawned(pid) {
        let _ = child.kill().await;
        return Err(error);
    }
    let stderr = child.stderr.take();
    let reader = tokio::spawn(async move {
        use tokio::io::AsyncReadExt as _;
        let mut kept = Vec::new();
        if let Some(mut stderr) = stderr {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                if kept.len() < 4096 {
                    kept.extend_from_slice(&chunk[..n.min(4096 - kept.len())]);
                }
            }
        }
        kept
    });
    let limit = timeout();
    let waited = tokio::time::timeout(limit, child.wait()).await;
    // The tree goes with the hook whatever happened.
    group.kill();
    let status = match waited {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => return Err(format!("cannot wait for it: {error}")),
        Err(_) => {
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            reader.abort();
            return Err(format!("timed out after {} ms", limit.as_millis()));
        }
    };
    let tail = tokio::time::timeout(Duration::from_secs(2), reader)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    let tail = String::from_utf8_lossy(&tail);
    let tail = tail.trim().replace(['\r', '\n'], " ");
    let code = status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "a signal".into());
    Err(if tail.is_empty() {
        format!("exited with {code}")
    } else {
        format!(
            "exited with {code}: {}",
            crate::util::truncate_chars(&tail, STDERR_TAIL)
        )
    })
}
