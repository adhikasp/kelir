//! Log writer and reader: the append-only JSONL file.
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::log::event::*;

/// A log line: envelope plus payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEvent {
    pub seq: u64,
    pub ts: String,
    #[serde(flatten)]
    pub body: EventBody,
}

impl EventBody {
    /// Token counts from this event, when it carries a provider call's usage.
    pub fn usage(&self) -> Option<&Usage> {
        match self {
            EventBody::Assistant { usage, .. } => usage.as_ref(),
            EventBody::StepUsage(usage) => Some(usage),
            _ => None,
        }
    }
}

impl EventBody {
    /// Every event type the log writes, for the docs drift test.
    pub const NAMES: &'static [&'static str] = &[
        "session_started",
        "user",
        "source",
        "step_started",
        "assistant",
        "step_usage",
        "verb",
        "call_started",
        "call_settled",
        "turn_ended",
        "compacted",
        "error",
        "session_ended",
        "session_title",
        "note",
        "gate",
    ];
}

impl LogEvent {
    pub fn new(seq: u64, body: EventBody) -> Self {
        Self {
            seq,
            ts: now_ts(),
            body,
        }
    }

    pub fn event_type(&self) -> &'static str {
        match self.body {
            EventBody::SessionStarted { .. } => "session_started",
            EventBody::User { .. } => "user",
            EventBody::Source { .. } => "source",
            EventBody::StepStarted { .. } => "step_started",
            EventBody::Assistant { .. } => "assistant",
            EventBody::StepUsage(_) => "step_usage",
            EventBody::Verb { .. } => "verb",
            EventBody::CallStarted(_) => "call_started",
            EventBody::CallSettled(_) => "call_settled",
            EventBody::TurnEnded { .. } => "turn_ended",
            EventBody::Compacted { .. } => "compacted",
            EventBody::Error { .. } => "error",
            EventBody::SessionEnded { .. } => "session_ended",
            EventBody::SessionTitle { .. } => "session_title",
            EventBody::Note(_) => "note",
            EventBody::Gate { .. } => "gate",
        }
    }
}

/// Append-only writer. Sequences continue from an existing file so a session
/// can be reopened without rewriting history.
pub struct EventLog {
    pub path: PathBuf,
    pub file: File,
    pub seq: u64,
}

impl EventLog {
    /// Open (or create) `path`, continuing `seq` after the last valid line.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let next = if path.exists() {
            let read = read_events(path)?;
            read.events.last().map(|e| e.seq + 1).unwrap_or(1)
        } else {
            1
        };
        // A torn last line (a crash mid-write) is cut off here, so the log holds whole events only: no
        // unparseable line, and never two lines with the same seq.
        remove_torn_tail(path)?;
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            seq: next,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn next_seq(&self) -> u64 {
        self.seq
    }

    /// Append one event, assigning the next `seq` and a fresh timestamp. The line is flushed to
    /// the OS; call [`EventLog::sync`] at a batch boundary to fsync it.
    pub fn append(&mut self, body: EventBody) -> io::Result<LogEvent> {
        let event = LogEvent::new(self.seq, body);
        self.seq += 1;
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.flush()?;
        Ok(event)
    }

    /// fsync the log (end of a batch; a crash loses at most the in-flight step).
    pub fn sync(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.file.sync_all()
    }
}

pub(super) fn remove_torn_tail(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let bytes = std::fs::read(path)?;
    if bytes.is_empty() || bytes.last() == Some(&b'\n') {
        return Ok(());
    }
    let complete = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let tail = &bytes[complete..];
    if serde_json::from_slice::<LogEvent>(tail).is_ok() {
        let mut file = OpenOptions::new().append(true).open(path)?;
        file.write_all(b"\n")?;
        file.flush()?;
        return Ok(());
    }
    stash_tail(path, tail)?;
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(complete as u64)?;
    file.sync_all()?;
    Ok(())
}

/// Copy a torn fragment to `<log>.torn`, one record per tear.
pub(super) fn stash_tail(path: &Path, tail: &[u8]) -> io::Result<()> {
    let mut name = path.as_os_str().to_os_string();
    name.push(".torn");
    let mut file = OpenOptions::new().create(true).append(true).open(&name)?;
    writeln!(file, "--- torn at {} ({} bytes) ---", now_ts(), tail.len())?;
    file.write_all(tail)?;
    file.write_all(b"\n")?;
    file.flush()?;
    file.sync_all()
}

/// The parsed log plus anything unreadable.
#[derive(Debug, Default)]
pub struct LogRead {
    pub events: Vec<LogEvent>,
    /// A final line with no trailing newline, or that failed to parse: a torn
    /// write from a crash. Skipped, and reported by the caller.
    pub torn_last: Option<String>,
    /// Lines before the last that failed to parse. Kept for reporting; never
    /// silently dropped.
    pub bad_lines: Vec<String>,
}

/// Read every complete event line. A torn last line never fails the read.
/// A log not written by format 1 is refused, with no migration path.
pub const FORMAT_MESSAGE: &str = "written by a different kelir format; start a new session";

pub fn read_events(path: &Path) -> io::Result<LogRead> {
    let bytes = std::fs::read(path)?;
    let mut out = LogRead::default();
    if bytes.is_empty() {
        return Ok(out);
    }
    let first: LogEvent = serde_json::from_slice(
        bytes
            .split(|byte| *byte == b'\n')
            .find(|line| !line.is_empty())
            .unwrap_or_default(),
    )
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, FORMAT_MESSAGE))?;
    if !matches!(first.body, EventBody::SessionStarted { format: 1, .. }) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, FORMAT_MESSAGE));
    }
    let mut start = 0usize;
    let mut last_complete = 0usize;
    for (i, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let line = &bytes[start..i];
        last_complete = i + 1;
        start = i + 1;
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<LogEvent>(line) {
            Ok(event) => out.events.push(event),
            Err(_) => out
                .bad_lines
                .push(String::from_utf8_lossy(line).into_owned()),
        }
    }
    if last_complete < bytes.len() {
        let tail = &bytes[last_complete..];
        // A crash can land between the payload and its newline. If what is there is a whole event,
        // keep it: losing a complete event would be worse than a missing terminator.
        match serde_json::from_slice::<LogEvent>(tail) {
            Ok(event) => out.events.push(event),
            Err(_) => out.torn_last = Some(String::from_utf8_lossy(tail).into_owned()),
        }
    }
    Ok(out)
}
