//! `bash`: run a command in the configured shell, streaming output to disk.
//!
//! The call returns when the **shell** exits, not at pipe EOF: a background
//! child (`cmd &`) inherits the pipe and may outlive the shell, and must not
//! hold the call open. After the shell exits we drain for a short grace period
//! so output already in the pipe still reaches the log, then drop the readers.
//! The child's process group is kept by the session, so that leftover child is
//! neither killed nor waited for (background jobs get their own lifecycle in
//! M4).
//!
//! Memory stays bounded: the file gets every byte, while the [`Capture`] keeps
//! only the inline window and a 1 KB tail ring.

use std::io::ErrorKind;
use std::process::Stdio;
use std::time::Duration;

use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::proc::ProcessGroup;
use crate::spill::Capture;
use crate::tools::{Ctx, ToolOutcome};

/// How long output already in the pipe may take to arrive after the shell exits.
const DRAIN_GRACE: Duration = Duration::from_millis(200);

pub async fn run(command: &str, ctx: &Ctx, dir: &str) -> ToolOutcome {
    let call_dir = ctx.call_dir(dir);
    if let Err(error) = tokio::fs::create_dir_all(&call_dir).await {
        return ToolOutcome::error(format!("cannot create {}: {error}", call_dir.display()));
    }
    let out_path = call_dir.join("out");

    if ctx.shell.is_empty() {
        return ToolOutcome::error("no shell configured");
    }

    let mut out_file = match File::create(&out_path).await {
        Ok(file) => file,
        Err(error) => {
            return ToolOutcome::error(format!("cannot open {}: {error}", out_path.display()));
        }
    };

    let mut process = tokio::process::Command::new(&ctx.shell[0]);
    process
        .args(&ctx.shell[1..])
        .arg(command)
        .current_dir(&ctx.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let group = ProcessGroup::new();
    group.configure(&mut process);

    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ToolOutcome::error(format!("cannot start {}: {error}", ctx.shell[0]));
        }
    };
    if let Some(pid) = child.id() {
        group.assign(pid);
    }
    // The group outlives the call: closing it here would kill a child the
    // command deliberately left running, and the session owns it anyway.
    ctx.keep_group(group);

    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
    let mut pumps = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let tx = tx.clone();
        pumps.push(tokio::spawn(pump(stdout, tx)));
    }
    if let Some(stderr) = child.stderr.take() {
        let tx = tx.clone();
        pumps.push(tokio::spawn(pump(stderr, tx)));
    }
    drop(tx);

    let mut capture = Capture::new(ctx.spill_limit);
    let mut write_error: Option<String> = None;
    let mut status = None;

    // Phase 1: read while the shell runs; stop the moment it exits.
    loop {
        tokio::select! {
            item = rx.recv() => match item {
                Some(chunk) => {
                    write_chunk(&chunk, &mut capture, &mut out_file, &mut write_error).await;
                }
                None => break,
            },
            result = child.wait(), if status.is_none() => {
                status = Some(result);
                break;
            }
        }
    }

    // Phase 2: if the pipes closed first, the shell is about to exit.
    if status.is_none() {
        status = Some(child.wait().await);
    }

    // Phase 3: drain what is already buffered, for at most the grace period.
    let deadline = Instant::now() + DRAIN_GRACE;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(chunk)) => {
                write_chunk(&chunk, &mut capture, &mut out_file, &mut write_error).await;
            }
            Ok(None) | Err(_) => break,
        }
    }
    for handle in pumps {
        handle.abort();
    }

    let _ = out_file.flush().await;
    let _ = out_file.sync_all().await;

    if let Some(error) = write_error {
        return ToolOutcome::error(format!("cannot write {}: {error}", out_path.display()));
    }

    let status = match status {
        Some(Ok(status)) => status,
        Some(Err(error)) => {
            return ToolOutcome::error(format!("cannot read the exit status: {error}"));
        }
        None => return ToolOutcome::error("the shell never reported an exit status"),
    };

    let exit_code = status.code();
    let (result, spilled) = capture.render(exit_code, &out_path);
    ToolOutcome {
        outcome: if status.success() { "ok" } else { "error" }.into(),
        exit_code,
        bytes: capture.bytes(),
        lines: capture.lines(),
        out_path: crate::util::path_string(&out_path),
        spilled,
        result,
    }
}

async fn write_chunk(
    chunk: &[u8],
    capture: &mut Capture,
    out_file: &mut File,
    write_error: &mut Option<String>,
) {
    capture.push(chunk);
    if write_error.is_none() {
        if let Err(error) = out_file.write_all(chunk).await {
            *write_error = Some(error.to_string());
        }
    }
}

async fn pump<R: AsyncRead + Unpin>(mut reader: R, sender: mpsc::Sender<Vec<u8>>) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if sender.send(buf[..n].to_vec()).await.is_err() {
                    break;
                }
            }
            // A pipe can be reset by a background child writing after we
            // stopped reading; that is not the call's failure.
            Err(error) if error.kind() == ErrorKind::BrokenPipe => break,
            Err(_) => break,
        }
    }
}
