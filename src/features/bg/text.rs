//! Background jobs: the shim, the status files, and the `kelir bg` CLI.
//! A job is one `kelir bg-shim` process (the same binary) that spawns the command, hands it the
//! output file directly and writes `bg/<id>.status` whenever its state changes.
//! The shim owns the command's process tree: on Unix both are in the shim's process group
//! (`setsid` for a job), on Windows the command is assigned to a named Job Object the shim keeps

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::util::path_string;

use super::*;

// ------------------------------------------------------------- what the model sees

/// The `user` message that wakes the agent when a job settles. It is a message,
/// not a tool result: a job settles whenever it wants, not inside a call.
pub fn wake_text(status: &Status) -> String {
    let took = match status.elapsed_secs() {
        Some(secs) => format!(" after {secs:.1}s"),
        None => String::new(),
    };
    let headline = match status.state.as_str() {
        STATE_CANCELLED => format!("[bg job {} was cancelled{took}]", status.id),
        STATE_LOST => format!(
            "[bg job {} was lost{took}: its shim is gone, so nothing can report the result]",
            status.id
        ),
        _ => match status.exit_code {
            Some(code) => format!("[bg job {} exited {code}{took}]", status.id),
            None => format!("[bg job {} ended{took}]", status.id),
        },
    };
    let mut text = headline;
    if !status.cmd.trim().is_empty() {
        text.push_str(&format!(
            "\ncommand: {}",
            crate::util::truncate_chars(status.cmd.trim(), 300)
        ));
    }
    let out = PathBuf::from(&status.out);
    text.push_str(&format!("\noutput: {}", status.out));
    if let Ok(partial) = crate::log::spill::partial(&out) {
        text.push_str(&format!(
            " ({} bytes, {} lines)",
            partial.bytes, partial.lines
        ));
        if !partial.tail.trim().is_empty() {
            text.push_str("\n--- tail ---\n");
            text.push_str(partial.tail.trim_end());
            text.push('\n');
        }
    }
    text.push_str(&format!(
        "read it with `kelir bg out {}`; the log records it as `call_settled{{kind:\"bg\"}}`",
        status.id
    ));
    text
}

/// What a bash call's result says when its command is promoted: the job id, how
/// to read it, and what it had already printed.
pub fn promoted_text(
    id: &str,
    limit: Duration,
    out: &Path,
    bytes: u64,
    lines: u64,
    tail: &str,
) -> String {
    let mut text = format!(
        "promoted to a background job after running past the {}s limit: bg job `{id}`\n\
         the job keeps running when the session ends.\n\
         read its output: kelir bg out {id} [--follow]\n\
         wait for it:     kelir bg wait {id}\n\
         cancel it:       kelir bg cancel {id}\n\
         output so far:   {bytes} bytes, {lines} lines at {}",
        limit.as_secs(),
        path_string(out)
    );
    if !tail.trim().is_empty() {
        text.push_str("\n--- tail ---\n");
        text.push_str(tail.trim_end());
        text.push('\n');
    }
    text.push_str(
        "you will get a message when it settles (a headless run that has already \
         exited delivers it on `kelir run --resume`).",
    );
    text
}
