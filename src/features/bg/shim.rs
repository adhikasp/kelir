//! Background jobs: the shim, the status files, and the `kelir bg` CLI.
//! A job is one `kelir bg-shim` process (the same binary) that spawns the command, hands it the
//! output file directly and writes `bg/<id>.status` whenever its state changes.
//! The shim owns the command's process tree: on Unix both are in the shim's process group
//! (`setsid` for a job), on Windows the command is assigned to a named Job Object the shim keeps

use std::path::Path;
use std::time::Duration;

use crate::main_cli::BgShimArgs;
use crate::util::path_string;

use super::cli::notify;
use super::*;

/// The stdout and stderr handles handed to the child. Truncate once, then open
/// two *independent* handles: a shared offset loses writes on macOS, where
/// stderr reused stdout's position without an append flag.
///
/// On Windows a handle opened with only FILE_APPEND_DATA is rejected as a
/// child's stdout/stderr: the child exits 1 without writing a byte (the bash
/// tool's shim lost its script and the job settled `lost`). A duplicated handle
/// carries FILE_WRITE_DATA, and the child always writes at EOF on Windows, so
/// both streams still append in order there.
#[cfg(windows)]
fn output_handles(path: &Path) -> std::io::Result<(std::fs::File, std::fs::File)> {
    let out = std::fs::File::create(path)?;
    Ok((out.try_clone()?, out))
}

#[cfg(not(windows))]
fn output_handles(path: &Path) -> std::io::Result<(std::fs::File, std::fs::File)> {
    use std::io::Write as _;
    let mut truncate = std::fs::File::create(path)?;
    truncate.flush()?;
    drop(truncate);
    Ok((
        std::fs::OpenOptions::new().append(true).open(path)?,
        std::fs::OpenOptions::new().append(true).open(path)?,
    ))
}

// ------------------------------------------------------------------ the shim

/// `kelir bg-shim`: spawn the command, write the status file, and stay alive
/// until the command exits (or the tree is killed).
pub async fn shim_main(args: BgShimArgs) -> u8 {
    // `--dir` is the root (`calls/` or the global `bg/`); this job owns exactly one
    // directory below it. All of this shim's files go there and nowhere else.
    let dir = args.dir.clone();
    let job_dir = job_dir(&dir, &args.id);
    // The shim holds the session token for as long as the job lives: close the process to its own
    // user's ordinary memory reads (Windows; on Unix `/proc/<pid>/mem` is closed to a sibling).
    if let Err(error) = harden_process() {
        eprintln!("kelir bg-shim: {error}");
    }
    if let Err(error) = std::fs::create_dir_all(&job_dir) {
        eprintln!(
            "kelir bg-shim: cannot create {}: {error}",
            job_dir.display()
        );
        return 1;
    }
    // A command that came as a file (it is too long for an argument list) is
    // read here, in the shim's own memory, and never put on a command line.
    let script = args.cmd_file.clone();
    let args = match script.as_ref() {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => BgShimArgs { cmd: text, ..args },
            Err(error) => {
                eprintln!("kelir bg-shim: cannot read {}: {error}", path.display());
                return 1;
            }
        },
        None => args,
    };
    let mut status = Status::new(&args.id, &args.kind, &args.cmd, &args.out);
    status.call = args.call.clone();
    status.job = args.job.clone();
    // The shim reads *its own* start time from the OS, in this process: a reader in another process
    // compares that exact value with what the OS says about the pid before it signals anything, which is
    status.shim_started = crate::sys::proc::start_time_text(std::process::id());

    // The Job Object: for a call the session created it and the shim only opens it (so the tree is in the
    // call's job before the command runs); for a `kelir bg run` job the shim creates it.
    #[cfg(windows)]
    let job = {
        let Some(name) = args.job.clone() else {
            fail(&dir, &status, "no job object name was given");
            return 1;
        };
        // A job the shim creates must outlive everything: no kill-on-close.
        let job = crate::sys::proc::win::Job::open(&name)
            .or_else(|| crate::sys::proc::win::Job::create_named(&name, false));
        match job {
            Some(job) => Some(job),
            None => {
                fail(&dir, &status, "cannot create or open the call's job object");
                return 1;
            }
        }
    };

    let (out_file, out_for_err) = match output_handles(&args.out) {
        Ok(handles) => handles,
        Err(error) => {
            fail(
                &dir,
                &status,
                &format!("cannot create the output file: {error}"),
            );
            return 1;
        }
    };
    if args.shell.is_empty() {
        fail(&dir, &status, "no shell was given");
        return 1;
    }
    let argv: Vec<String> = std::iter::once(args.shell.clone())
        .chain(args.shell_args.iter().cloned())
        .collect();
    // A long command is written to a file and handed to the shell by name: a command line past ~8 KB is
    // silently cut on Windows (`shellcmd`), and the cut looks like success.
    let script_dir = args
        .out
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| job_dir.to_path_buf());
    // `job_dir` is the per-call directory (the Windows-only `Job` handle below is a different
    // binding and deliberately not cloned: the handle owns kill-on-close and must have one owner).
    // A caller that already wrote the script (the bash call, which had to keep the shim's own
    // argv short) decided it; otherwise the shim decides from the command and the shell.
    let shell_input = match &script {
        Some(path) => match crate::sys::shell::script_input(path, &args.shell_args) {
            Ok(input) => input,
            Err(error) => {
                fail(&dir, &status, &error);
                return 1;
            }
        },
        None => match crate::sys::shell::input(&args.cmd, &args.shell_args, &script_dir, "") {
            Ok(input) => input,
            Err(error) => {
                fail(&dir, &status, &error);
                return 1;
            }
        },
    };
    let mut command = crate::sys::runner::tokio_command(&argv[0]);
    match &shell_input {
        crate::sys::shell::ShellInput::Argument(cmd) => {
            command.args(&argv[1..]).arg(cmd);
        }
        crate::sys::shell::ShellInput::Script { path, keep_switch } => {
            // The switch is kept for a shell that needs one even with a file (`cmd.exe /C <script>`, `pwsh -File
            // <script>`): dropping `/C` starts an interactive cmd.exe, which runs nothing and exits 0.
            let rest =
                crate::sys::shell::script_args(&args.shell_args, keep_switch.as_deref(), path);
            command.args(&rest);
            // A reader of the status file (and the model, in a `bg` report) gets a reference, never a 40 KB
            // command; `cmd.sh` has the bytes.
            status.cmd = crate::util::truncate_chars(args.cmd.trim(), 300);
            status.cmd_path = Some(path_string(path));
        }
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(out_file))
        .stderr(std::process::Stdio::from(out_for_err));
    // A bash call's children must find `kelir bg` and their own call id. A session job's
    // root is `<session>/calls/`; a session-less job gets the dir explicitly from its launcher.
    if let Some(session_dir) = args.session_dir.as_deref().or_else(|| args.dir.parent()) {
        command.env("KELIR_SESSION_DIR", session_dir);
    }
    if let Ok(binary) = crate::util::self_binary() {
        // The same binary this shim is: `kelir bg run` inside a call works even
        // when kelir is not on PATH.
        command.env("KELIR_BIN", binary);
    }
    // The model's tools do not get the provider's keys: the shim and the
    // session keep them, a command that needs one says so in its own command.
    for name in crate::provider::keys::PROVIDER_KEY_ENVS {
        command.env_remove(name);
    }
    if let Some(call) = &args.call {
        command.env("KELIR_CALL_ID", call);
    }
    if let Some(sock) = &args.sock {
        command.env("KELIR_SOCK", sock);
    }
    // `KELIR_TOKEN` is not copied here: the shim was started with it in its own environment and the
    // command inherits that environment, so the token is never in an argv and never written to a status
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.as_std_mut().creation_flags(
            crate::sys::proc::win::CREATE_SUSPENDED
                | crate::sys::proc::win::CREATE_NEW_PROCESS_GROUP,
        );
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            fail(&dir, &status, &format!("cannot start {}: {error}", argv[0]));
            return 1;
        }
    };
    let Some(pid) = child.id() else {
        fail(&dir, &status, "the command started without a pid");
        return 1;
    };
    #[cfg(windows)]
    if let Some(job) = &job {
        let assigned = job
            .assign(pid)
            .and_then(|()| crate::sys::proc::win::resume(pid).map_err(|error| error.to_string()));
        if let Err(error) = assigned {
            // A call outside its job cannot be stopped: refuse to run it.
            crate::sys::proc::win::terminate(pid);
            fail(
                &dir,
                &status,
                &format!("cannot control the command's tree: {error}"),
            );
            return 1;
        }
        // A `kelir bg run` job outlives everything by definition: let the command hold the job handle too, so
        // the job's name survives the shim and `cancel` can still open it after the shim is killed,
        if args.kind == KIND_JOB
            && let Err(error) = job.lend_to(pid)
        {
            eprintln!("kelir bg-shim: {error}");
        }
    }
    status.pid = Some(pid);
    // The command's own start time, read here once: `cancel` uses it to prove that the pid it walks a lost
    // job's tree from is the command, not its recycled pid.
    status.pid_started = crate::sys::proc::start_time_text(pid);
    if let Err(error) = status.write(&status_path(&dir, &args.id)) {
        eprintln!("kelir bg-shim: cannot write the status file: {error}");
    }
    // noticing the file on its next poll. The token comes from the environment, as for every child.
    if let Some(sock) = args.sock.as_deref()
        && args.kind == KIND_JOB
    {
        let _ = tokio::time::timeout(Duration::from_secs(3), notify(&status, sock)).await;
    }

    // The owner watch: a session that dies without promoting this call must not leave the tree
    // behind. Promotion sets the marker and the shim keeps running on its own.
    let owner = Owner::new(args.owner);
    let mut promoted = false;
    let status_file = status_path(&dir, &args.id);
    let marker = promote_path(&dir, &args.id);
    loop {
        if let Ok(Some(exit)) = child.try_wait() {
            status.state = STATE_EXITED.into();
            status.exit_code = exit.code();
            status.finished = Some(crate::log::event::now_ts());
            if let Err(error) = status.write(&status_file) {
                eprintln!("kelir bg-shim: cannot write the status file: {error}");
            }
            if !promoted {
                // The command is gone, but it may have left a child behind (`cmd &`).
                linger(&owner);
            }
            return 0;
        }
        if !promoted && marker.exists() {
            promoted = true;
            status.promoted = true;
            // The marker is only meaningful before the session dies; recording
            // it in the status is the acknowledgement promotion waits for.
            if let Err(error) = status.write(&status_file) {
                eprintln!("kelir bg-shim: cannot write the status file: {error}");
            }
            // The tree is ours now. Let the command hold the job handle so the job's name outlives this
            // shim: a lost job is only reachable by name, and the name dies with its last handle.
            #[cfg(windows)]
            if let Some(job) = &job
                && let Err(error) = job.lend_to(pid)
            {
                eprintln!("kelir bg-shim: {error}");
            }
            #[cfg(not(windows))]
            let _ = pid;
        }
        if !promoted && owner.gone() {
            // The session that owns this call is gone and never promoted it: take the tree down, as the
            // session would have. The status goes first, because on Unix the kill includes this process.
            status.state = STATE_CANCELLED.into();
            status.finished = Some(crate::log::event::now_ts());
            status.error = Some("the session exited".into());
            let _ = status.write(&status_file);
            kill_tree(std::process::id(), &args.job);
            return 1;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Make this process's memory unreadable to other processes of the same user, as far as the platform
/// allows.
pub fn harden_process() -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::sys::proc::win::deny_same_user_memory_access()
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: prctl(PR_SET_DUMPABLE, 0) only changes this process's flag.
        if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
            return Err(format!(
                "cannot make this process non-dumpable: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Ok(())
    }
}

/// The command has exited; stay as the tree's owner while the tree still has a process in it, so the
/// owner's death can take those leftovers down with one `killpg`.
#[cfg(target_os = "linux")]
fn linger(owner: &Owner) {
    // A `kelir bg run` job has no owner to watch: nothing to protect it from,
    // so the shim exits with the command.
    if owner.pid.is_none() {
        return;
    }
    let mut interval = POLL;
    let mut scan_at = std::time::Instant::now();
    loop {
        if owner.gone() {
            kill_tree(std::process::id(), &None);
            return;
        }
        if std::time::Instant::now() >= scan_at {
            match crate::sys::proc::group_others(std::process::id()) {
                // Nothing but this shim is left in the group: done.
                Some(0) => return,
                Some(_) => {
                    interval = (interval * 2).min(LINGER_MAX);
                    scan_at = std::time::Instant::now() + interval;
                }
                // The platform cannot say: do not hold a process for nothing.
                None => return,
            }
        }
        let wait = scan_at
            .saturating_duration_since(std::time::Instant::now())
            .min(POLL);
        std::thread::sleep(wait);
    }
}

#[cfg(target_os = "linux")]
pub const LINGER_MAX: Duration = Duration::from_secs(1);

#[cfg(not(target_os = "linux"))]
fn linger(_owner: &Owner) {}

struct Owner {
    pid: Option<u32>,
    #[cfg(windows)]
    watch: Option<crate::sys::proc::win::OwnerWatch>,
}

impl Owner {
    fn new(pid: Option<u32>) -> Self {
        #[cfg(windows)]
        let watch = pid.and_then(crate::sys::proc::win::OwnerWatch::open);
        Self {
            pid,
            #[cfg(windows)]
            watch,
        }
    }

    fn gone(&self) -> bool {
        let Some(pid) = self.pid else {
            return false;
        };
        #[cfg(unix)]
        {
            // The kernel reparents an orphan immediately, so the parent pid is
            // the honest answer.
            unsafe { libc::getppid() != pid as libc::pid_t }
        }
        #[cfg(windows)]
        {
            let _ = pid;
            match &self.watch {
                Some(watch) => watch.gone(),
                // The handle could not be opened: the process is already gone.
                None => true,
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = pid;
            false
        }
    }
}

/// Record a failure the model can read, and give up.
fn fail(dir: &Path, status: &Status, message: &str) {
    let mut status = status.clone();
    status.state = STATE_EXITED.into();
    status.exit_code = None;
    status.finished = Some(crate::log::event::now_ts());
    status.error = Some(message.to_string());
    let _ = status.write(&status_path(dir, &status.id));
    eprintln!("kelir bg-shim: {message}");
}

/// Take a tree down from inside the shim (the session died) or from `cancel`.
pub(crate) fn kill_tree(pid: u32, job: &Option<String>) {
    cfg_select! {
        unix => {
            let _ = job;
            // The shim leads the group; the command inherited it, so this is the whole tree — including
            // the shim itself. Never signal group 0: that would include the agents running the tests.
            if pid == 0 {
                return;
            }
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        windows => {
            if let Some(name) = job
                && let Some(job) = crate::sys::proc::win::Job::open(name)
            {
                job.terminate_job();
                return;
            }
            if pid != 0 {
                crate::sys::proc::win::terminate(pid);
            }
        }
        _ => {
            let _ = (pid, job);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn output_handles_are_writable_by_both_streams() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out");
        let (mut stdout, mut stderr) = output_handles(&path).unwrap();
        stdout.write_all(b"out\n").unwrap();
        stderr.write_all(b"err\n").unwrap();
        drop((stdout, stderr));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "out\nerr\n");
    }
}
