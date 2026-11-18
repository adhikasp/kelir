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
    let launch = bg::Launch {
        dir: bg_dir.clone(),
        id: dir.to_string(),
        out: out_path.clone(),
        cmd: command.to_string(),
        shell: ctx.shell.clone(),
        kind: bg::KIND_CALL,
        owner: Some(std::process::id()),
        call: Some(call_id.to_string()),
        job: Some(bg::job_name(&ctx.session_id, dir)),
        sock: Some(ctx.sock.clone()),
    };
    let mut process = match bg::shim_command(&launch) {
        Ok(process) => process,
        Err(error) => return ToolOutcome::error(error),
    };

    let group = ProcessGroup::new();
    #[cfg(windows)]
    let group = {
        use crate::proc::win::Job;
        match Job::create_named(launch.job.as_deref().unwrap_or_default()) {
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
    loop {
        tail.drain_into(&mut capture).await;
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
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
            // It finished (or the shim never acknowledged): the session still
            // owns the tree, so the call settles normally.
        }
        tokio::time::sleep(bg::POLL).await;
    }
    tail.drain_into(&mut capture).await;
    // The call is over: a child the command deliberately left running (`cmd &`)
    // belongs to the session now, not to this task.
    guard.armed = false;
    ctx.running.remove(dir);

    // The command's own exit code and output are in the status file the shim
    // wrote before it exited; the shim's own exit code is only a fallback.
    let status = bg::Status::read(&bg::status_path(&bg_dir, dir));
    if let Some(error) = status.as_ref().and_then(|status| status.error.clone()) {
        return ToolOutcome::error(format!("the command did not run: {error}"));
    }
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
        if bg::Status::read(&bg::status_path(bg_dir, id))
            .map(|status| status.promoted)
            .unwrap_or(false)
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        if let Ok(Some(_)) = child.try_wait() {
            return false;
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
