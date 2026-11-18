//! `bash`: run a command in the configured shell, tee output to disk.

use std::process::Stdio;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::proc::ProcessGroup;
use crate::spill;
use crate::tools::{Ctx, ToolOutcome};

pub async fn run(command: &str, ctx: &Ctx, call_id: &str) -> ToolOutcome {
    let dir = ctx.call_dir(call_id);
    if let Err(error) = tokio::fs::create_dir_all(&dir).await {
        return ToolOutcome::error(format!("cannot create {}: {error}", dir.display()));
    }
    let out_path = dir.join("out");

    if ctx.shell.is_empty() {
        return ToolOutcome::error("no shell configured");
    }
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

    let mut out_file = match tokio::fs::File::create(&out_path).await {
        Ok(file) => file,
        Err(error) => {
            let _ = child.kill().await;
            return ToolOutcome::error(format!("cannot open {}: {error}", out_path.display()));
        }
    };

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

    let mut collected: Vec<u8> = Vec::new();
    let mut write_error: Option<String> = None;
    while let Some(chunk) = rx.recv().await {
        collected.extend_from_slice(&chunk);
        if write_error.is_none() {
            if let Err(error) = out_file.write_all(&chunk).await {
                write_error = Some(error.to_string());
            }
        }
    }
    for handle in pumps {
        let _ = handle.await;
    }

    let status = child.wait().await;
    let _ = out_file.flush().await;
    let _ = out_file.sync_all().await;
    // Closing the group takes any stray descendants with it (Windows Job
    // Object); on Unix the call has already reaped the child.
    drop(group);

    if let Some(error) = write_error {
        return ToolOutcome::error(format!("cannot write {}: {error}", out_path.display()));
    }

    let exit_code = status.as_ref().ok().and_then(|status| status.code());
    let success = status.as_ref().map(|s| s.success()).unwrap_or(false);
    let (result, spilled) = spill::format_result(&collected, exit_code, ctx.spill_limit, &out_path);

    ToolOutcome {
        outcome: if success { "ok" } else { "error" }.into(),
        exit_code,
        bytes: collected.len() as u64,
        lines: spill::count_lines(&collected),
        out_path: out_path.display().to_string(),
        spilled,
        result,
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
            Err(_) => break,
        }
    }
}
