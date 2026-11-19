//! `bash`: run a command in the configured shell.
//!
//! Every call runs under `kelir bg-shim` (this same binary, M4). The shim is
//! the call's process owner: it spawns the shell, hands it the output file
//! directly, and writes `bg/<dir>.status` (running, then exited). This module
//! only tails that file for the inline result and waits for the shim.
//!
//! Two things fall out of owning the tree in the shim:
//!
//! * **A call past `promote_after` is promoted, not killed**: the session stops
//!   waiting, marks the job promoted and lets go of the process group, and the
//!   shim keeps running (writing the same file) when the session exits. Work in
//!   flight is never destroyed by a clock.
//! * **A call's whole tree dies with it**: on Unix the shim leads the process
//!   group, on Windows the command is in the call's Job Object, and a session
//!   that dies without promoting takes the tree down itself (the shim watches
//!   its owner).
//!
//! Memory stays bounded: the file gets every byte, while the [`Capture`] keeps
//! only the inline window and a 1 KB tail ring.

use std::path::{Path, PathBuf};

use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::time::Instant;

use crate::bg;
use crate::proc::ProcessGroup;
use crate::shellcmd;
use crate::spill::Capture;
use crate::tools::{Ctx, ToolOutcome};

pub async fn run(command: &str, ctx: &Ctx, dir: &str, call_id: &str) -> ToolOutcome {
    let call_dir = ctx.call_dir(dir);
    if let Err(error) = tokio::fs::create_dir_all(&call_dir).await {
        return ToolOutcome::error(format!("cannot create {}: {error}", call_dir.display()));
    }
    let out_path = call_dir.join("out");
    if ctx.shell.is_empty() {
        return ToolOutcome::error("no shell configured");
    }

    let bg_dir = ctx.session_dir.join("bg");
    // The command goes to a file *here*, before the shim exists (on Windows
    // always, on Unix when it is long): the file is what the shell runs
    // (`cmd.sh`), it keeps a 40 KB command off the shim's own command line,
    // where Windows refuses to start it at all (`os error 206`), and it keeps
    // backslashes off msys' argv rewriting. A POSIX shell's argv then drops the
    // `-c` (`bash cmd.sh`); `cmd.exe` keeps its `/C` (`cmd.exe /C cmd.bat`).
    let shell_args: Vec<String> = ctx.shell.iter().skip(1).cloned().collect();
    let cmd_file = match shellcmd::input(command, &shell_args, &call_dir, "") {
        Ok(shellcmd::ShellInput::Argument(_)) => None,
        Ok(shellcmd::ShellInput::Script { path, .. }) => Some(path),
        Err(error) => return ToolOutcome::error(error),
    };
    let launch = bg::Launch {
        dir: bg_dir.clone(),
        id: dir.to_string(),
        out: out_path.clone(),
        cmd: command.to_string(),
        cmd_file,
        shell: ctx.shell.clone(),
        kind: bg::KIND_CALL,
        owner: Some(std::process::id()),
        call: Some(call_id.to_string()),
        job: Some(bg::job_name(&ctx.session_id, dir)),
        sock: Some(ctx.sock.clone()),
        token: Some(ctx.token.clone()).filter(|token| !token.is_empty()),
        // The call runs where the session (or `--cwd`, or a resumed session's
        // log) says, not in whatever directory the session process happens to
        // have (M4 review F2).
        cwd: Some(ctx.cwd.clone()),
    };
    let mut process = match bg::shim_command(&launch) {
        Ok(process) => process,
        Err(error) => return ToolOutcome::error(error),
    };

    let group = ProcessGroup::new();
    #[cfg(windows)]
    let group = {
        use crate::proc::win::Job;
        // Kill-on-close: a session that is killed outright still takes this
        // call's tree with it, including a child the command left behind
        // (M4 review F6). Promotion clears the flag before the session lets go.
        match Job::create_named(launch.job.as_deref().unwrap_or_default(), true) {
            Some(job) => group.with_job(job),
            // A call whose tree is not in a Job Object could not be stopped
            // (M1 review N7): refuse it instead of running it unreachable.
            None => {
                return ToolOutcome::error(format!(
                    "cannot create a job object for the call's tree ({})",
                    launch.job.as_deref().unwrap_or_default()
                ));
            }
        }
    };
    group.configure(&mut process);
    // Registered *before* the process exists, so a `stop` that arrives while
    // the shim is being created still finds (and kills) it.
    ctx.running.insert(dir, group.clone());

    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            ctx.running.remove(dir);
            return ToolOutcome::error(format!("cannot start {}: {error}", ctx.shell[0]));
        }
    };
    match child.id() {
        Some(pid) => {
            if let Err(error) = group.spawned(pid) {
                let _ = child.kill().await;
                ctx.running.remove(dir);
                return ToolOutcome::error(error);
            }
        }
        None => {
            let _ = child.kill().await;
            ctx.running.remove(dir);
            return ToolOutcome::error("the shim started without a pid");
        }
    }
    // The group outlives the call: closing it here would kill a child the
    // command deliberately left running, and the session owns it anyway.
    ctx.keep_group(group.clone());
    // If this task is cancelled (a stop, a settling batch) the tree must not
    // outlive it: `armed` turns the group kill into a `Drop` guarantee.
    let mut guard = TreeGuard {
        group: group.clone(),
        armed: true,
    };

    let mut capture = Capture::new(ctx.spill_limit);
    let mut tail = Tail::new(&out_path);
    let started = Instant::now();
    let mut promotion_tried = false;
    // The shim's own exit, when it got there first: on Unix an unpromoted call
    // whose tree still holds a child (`cmd &`) leaves the shim behind as the
    // tree's owner, so settlement is what the *status file* says, not the shim
    // process (M4 review F6).
    let mut shim_exit = None;
    loop {
        tail.drain_into(&mut capture).await;
        if bg::Status::read(&bg::status_path(&bg_dir, dir))
            .map(|status| status.settled())
            .unwrap_or(false)
        {
            break;
        }
        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => {
                shim_exit = Some(status);
                break;
            }
            Err(error) => {
                guard.armed = false;
                ctx.running.remove(dir);
                return ToolOutcome::error(format!("cannot read the shim's exit status: {error}"));
            }
        }
        if !promotion_tried && bg::promotion_due(ctx.promote_after, started.elapsed()) {
            promotion_tried = true;
            if let Err(error) = bg::mark_promoted(&bg_dir, dir) {
                guard.armed = false;
                ctx.running.remove(dir);
                return ToolOutcome::error(format!("cannot promote the call: {error}"));
            }
            if wait_for_promotion(&bg_dir, dir, &mut tail, &mut capture, &mut child).await {
                // The shim has the tree; the session must not kill it, now or
                // at exit. The output keeps growing in the same file.
                guard.armed = false;
                group.detach();
                ctx.running.remove(dir);
                let (bytes, lines) = (capture.bytes(), capture.lines());
                let tail_text = capture.tail_text();
                return ToolOutcome {
                    outcome: "promoted".into(),
                    exit_code: None,
                    bytes,
                    lines,
                    out_path: crate::util::path_string(&out_path),
                    spilled: false,
                    result: bg::promoted_text(
                        dir,
                        ctx.promote_after,
                        &out_path,
                        bytes,
                        lines,
                        &tail_text,
                    ),
                };
            }
            // It finished, or the shim never acknowledged: the session still
            // owns the tree, so the call settles normally — and the marker goes,
            // so a shim that looks later cannot promote a call the model has
            // already been answered (M4 review N4).
            let _ = std::fs::remove_file(bg::promote_path(&bg_dir, dir));
        }
        tokio::time::sleep(bg::POLL).await;
    }
    tail.drain_into(&mut capture).await;

    // The command's own exit code and output are in the status file the shim
    // wrote before it exited (or before it stayed behind as the tree's owner);
    // the shim's own exit code is only a fallback.
    let status = bg::Status::read(&bg::status_path(&bg_dir, dir));
    let settled = status
        .as_ref()
        .map(|status| status.settled())
        .unwrap_or(false);
    if let Some(error) = status.as_ref().and_then(|status| status.error.clone()) {
        guard.armed = false;
        ctx.running.remove(dir);
        return ToolOutcome::error(format!("the command did not run: {error}"));
    }
    if !settled && shim_exit.is_some() {
        // The shim died (a `taskkill /F`, an OOM kill) before the command
        // settled: nothing will ever write this call's result. Take what is
        // left of the tree down rather than leave a working orphan behind, and
        // say the outcome is unknown instead of hanging (M4 review F4).
        let out = crate::util::path_string(&out_path);
        let (bytes, lines) = (capture.bytes(), capture.lines());
        let mut result = "the call's shim exited before the command settled: no result was \
                          recorded and the outcome is unknown. Its process tree was stopped."
            .to_string();
        if bytes > 0 {
            result.push_str(&format!(
                "\npartial output: {bytes} bytes, {lines} lines; full output at {out}"
            ));
            let tail_text = capture.tail_text();
            if !tail_text.is_empty() {
                result.push_str("\n--- tail ---\n");
                result.push_str(&tail_text);
            }
        }
        // `guard` is still armed: returning kills the group.
        ctx.running.remove(dir);
        return ToolOutcome {
            outcome: "unknown".into(),
            exit_code: None,
            bytes,
            lines,
            out_path: out,
            spilled: false,
            result,
        };
    }
    // The call is over: a child the command deliberately left running (`cmd &`)
    // belongs to the session now, not to this task.
    guard.armed = false;
    ctx.running.remove(dir);

    let exit_code = status
        .as_ref()
        .and_then(|status| status.exit_code)
        .or_else(|| {
            child
                .try_wait()
                .ok()
                .flatten()
                .and_then(|status| status.code())
        });
    let (result, spilled) = capture.render(exit_code, &out_path);
    ToolOutcome {
        outcome: if exit_code == Some(0) { "ok" } else { "error" }.into(),
        exit_code,
        bytes: capture.bytes(),
        lines: capture.lines(),
        out_path: crate::util::path_string(&out_path),
        spilled,
        result,
    }
}

/// Wait for the shim to acknowledge the promotion in its status file, keeping
/// the inline capture up to date meanwhile. Bounded, so a wedged shim cannot
/// hold the turn; `false` means the shim finished (or never answered), and the
/// call settles the ordinary way.
async fn wait_for_promotion(
    bg_dir: &Path,
    id: &str,
    tail: &mut Tail,
    capture: &mut Capture,
    child: &mut tokio::process::Child,
) -> bool {
    let deadline = Instant::now() + bg::PROMOTE_ACK;
    loop {
        let status = bg::Status::read(&bg::status_path(bg_dir, id));
        // The acknowledgement is the flag in the status file.
        if status
            .as_ref()
            .map(|status| status.promoted)
            .unwrap_or(false)
        {
            return true;
        }
        // The command finished while we waited: there is nothing to hand over.
        if status
            .as_ref()
            .map(|status| status.settled())
            .unwrap_or(false)
        {
            return false;
        }
        if Instant::now() >= deadline {
            // One last look, so a shim that acknowledged at the edge of the
            // window is honoured instead of being demoted (M4 review N4).
            return bg::Status::read(&bg::status_path(bg_dir, id))
                .map(|status| status.promoted)
                .unwrap_or(false);
        }
        if let Ok(Some(_)) = child.try_wait() {
            return bg::Status::read(&bg::status_path(bg_dir, id))
                .map(|status| status.promoted)
                .unwrap_or(false);
        }
        tail.drain_into(capture).await;
        tokio::time::sleep(bg::POLL).await;
    }
}

/// A group kill that happens even when the call's task is cancelled.
struct TreeGuard {
    group: ProcessGroup,
    armed: bool,
}

impl Drop for TreeGuard {
    fn drop(&mut self) {
        if self.armed {
            self.group.kill();
        }
    }
}

/// Reads the output file as it grows. The command holds the file directly, so
/// this only has to follow the offset — and a session that stops reading never
/// blocks the command.
struct Tail {
    path: PathBuf,
    file: Option<File>,
}

impl Tail {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            file: None,
        }
    }

    async fn drain_into(&mut self, capture: &mut Capture) {
        if self.file.is_none() {
            match File::open(&self.path).await {
                Ok(file) => self.file = Some(file),
                // The shim creates the file; a poll before that is not an error.
                Err(_) => return,
            }
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let mut buffer = [0u8; 8192];
        loop {
            match file.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => capture.push(&buffer[..read]),
                // A truncated or replaced file is not the call's failure.
                Err(_) => break,
            }
        }
    }
}
