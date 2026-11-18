//! The event log: the only source of truth for a session.
//!
//! One JSON object per line, `{"seq": n, "ts": "<RFC 3339>", "type": …}`,
//! `seq` monotonic from 1, fsync'd per batch. A torn last line is tolerated on
//! read (skipped and reported), never half-applied.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub fn now_ts() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// A tool call as recorded in an `assistant` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoggedToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub args: Value,
}

/// The frozen request prefix, recorded once in `session_started`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemInfo {
    pub text: String,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub name: String,
    pub path: String,
    pub hash: String,
}

/// The payload of a log line. `type` is the serde tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventBody {
    SessionStarted {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        depth: u32,
        cwd: String,
        os: String,
        kelir_version: String,
        provider: String,
        model: String,
        system: SystemInfo,
        #[serde(default)]
        sources: Vec<SourceInfo>,
        #[serde(default)]
        tools: Vec<String>,
    },
    User {
        text: String,
        #[serde(rename = "from")]
        from: String,
        delivery: String,
        #[serde(default)]
        attachments: Vec<Value>,
    },
    Source {
        kind: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    StepStarted {
        step: u32,
        provider: String,
        model: String,
        request_hash: String,
    },
    Assistant {
        step: u32,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
        #[serde(default)]
        tool_calls: Vec<LoggedToolCall>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish_reason: Option<String>,
    },
    Usage {
        step: u32,
        prompt_tokens: u64,
        completion_tokens: u64,
        cached_tokens: u64,
        cache_write_tokens: u64,
        cost: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowance: Option<Value>,
    },
    ToolStarted {
        call_id: String,
        name: String,
        input: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },
    ToolSettled {
        call_id: String,
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        bytes: u64,
        lines: u64,
        out_path: String,
        spilled: bool,
        result: String,
    },
    TurnEnded {
        outcome: String,
        final_text: String,
    },
    Error {
        #[serde(rename = "where")]
        location: String,
        message: String,
        retryable: bool,
    },
    SessionEnded {
        reason: String,
    },
}

/// A log line: envelope plus payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEvent {
    pub seq: u64,
    pub ts: String,
    #[serde(flatten)]
    pub body: EventBody,
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
            EventBody::Usage { .. } => "usage",
            EventBody::ToolStarted { .. } => "tool_started",
            EventBody::ToolSettled { .. } => "tool_settled",
            EventBody::TurnEnded { .. } => "turn_ended",
            EventBody::Error { .. } => "error",
            EventBody::SessionEnded { .. } => "session_ended",
        }
    }
}

/// Append-only writer. Sequences continue from an existing file so a session
/// can be reopened without rewriting history.
pub struct EventLog {
    path: PathBuf,
    file: File,
    seq: u64,
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
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        // A torn last line (crash mid-write) must not swallow the next event:
        // start a fresh line before appending anything.
        let len = file.metadata()?.len();
        if len > 0 {
            let mut probe = OpenOptions::new().read(true).open(path)?;
            probe.seek(SeekFrom::Start(len - 1))?;
            let mut last = [0u8; 1];
            probe.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.write_all(b"\n")?;
                file.flush()?;
            }
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
            seq: next,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one event, assigning the next `seq` and a fresh timestamp.
    /// The line is flushed to the OS; call [`EventLog::sync`] at a batch
    /// boundary to fsync it.
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
pub fn read_events(path: &Path) -> io::Result<LogRead> {
    let bytes = std::fs::read(path)?;
    let mut out = LogRead::default();
    if bytes.is_empty() {
        return Ok(out);
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
        out.torn_last = Some(String::from_utf8_lossy(&bytes[last_complete..]).into_owned());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kelir-event-test-{}-{}", ulid::Ulid::new(), name));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("events.jsonl")
    }

    fn started() -> EventBody {
        EventBody::SessionStarted {
            id: "ses_01".into(),
            parent: None,
            depth: 0,
            cwd: "/tmp".into(),
            os: "linux".into(),
            kelir_version: "0.1.0".into(),
            provider: "openrouter".into(),
            model: "test/model".into(),
            system: SystemInfo {
                text: "you are kelir".into(),
                hash: "abc".into(),
            },
            sources: vec![],
            tools: vec!["bash".into(), "edit".into()],
        }
    }

    fn samples() -> Vec<EventBody> {
        vec![
            started(),
            EventBody::User {
                text: "do it".into(),
                from: "cli".into(),
                delivery: "now".into(),
                attachments: vec![],
            },
            EventBody::StepStarted {
                step: 1,
                provider: "openrouter".into(),
                model: "test/model".into(),
                request_hash: "00".into(),
            },
            EventBody::Assistant {
                step: 1,
                text: "on it".into(),
                reasoning: Some("hmm".into()),
                tool_calls: vec![LoggedToolCall {
                    id: "call_1".into(),
                    name: "bash".into(),
                    args: json!({"command": "ls"}),
                }],
                finish_reason: Some("tool_calls".into()),
            },
            EventBody::Usage {
                step: 1,
                prompt_tokens: 10,
                completion_tokens: 5,
                cached_tokens: 4,
                cache_write_tokens: 2,
                cost: 0.001,
                allowance: Some(json!({"remaining": 1})),
            },
            EventBody::ToolStarted {
                call_id: "call_1".into(),
                name: "bash".into(),
                input: json!({"command": "ls"}),
                pid: Some(42),
            },
            EventBody::ToolSettled {
                call_id: "call_1".into(),
                outcome: "ok".into(),
                exit_code: Some(0),
                bytes: 3,
                lines: 1,
                out_path: "/state/commands/call_1/out".into(),
                spilled: false,
                result: "ok\n".into(),
            },
            EventBody::TurnEnded {
                outcome: "done".into(),
                final_text: "done".into(),
            },
            EventBody::Error {
                location: "provider".into(),
                message: "boom".into(),
                retryable: true,
            },
            EventBody::SessionEnded {
                reason: "done".into(),
            },
            EventBody::Source {
                kind: "hint".into(),
                name: "note".into(),
                text: Some("try this".into()),
                path: None,
            },
        ]
    }

    #[test]
    fn every_event_type_round_trips() {
        for body in samples() {
            let event = LogEvent::new(3, body.clone());
            let line = serde_json::to_string(&event).unwrap();
            let value: Value = serde_json::from_str(&line).unwrap();
            assert!(value.get("type").is_some(), "missing type in {line}");
            assert_eq!(value["seq"], 3);
            assert!(value.get("ts").and_then(|v| v.as_str()).is_some());
            let back: LogEvent = serde_json::from_str(&line).unwrap();
            assert_eq!(back, event);
            assert_eq!(event.event_type(), back.event_type());
        }
    }

    #[test]
    fn timestamp_is_rfc3339_utc() {
        let ts = now_ts();
        assert!(ts.ends_with('Z'), "{ts}");
        let parsed =
            time::OffsetDateTime::parse(&ts, &time::format_description::well_known::Rfc3339)
                .unwrap();
        assert_eq!(parsed.offset(), time::UtcOffset::UTC);
    }

    #[test]
    fn append_assigns_monotonic_seq_and_syncs() {
        let path = temp_path("seq");
        let mut log = EventLog::open(&path).unwrap();
        for i in 0..3 {
            let ev = log.append(started()).unwrap();
            assert_eq!(ev.seq, i + 1);
        }
        log.sync().unwrap();
        let read = read_events(&path).unwrap();
        assert_eq!(
            read.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(read.torn_last.is_none());
        // Reopening continues the sequence instead of restarting it.
        let mut log = EventLog::open(&path).unwrap();
        assert_eq!(log.append(started()).unwrap().seq, 4);
    }

    #[test]
    fn torn_last_line_is_skipped_and_reported() {
        let path = temp_path("torn");
        let mut file = std::fs::File::create(&path).unwrap();
        let good = serde_json::to_string(&LogEvent::new(1, started())).unwrap();
        write!(
            file,
            "{good}\n{{\"seq\": 2, \"ts\": \"x\", \"type\": \"assis"
        )
        .unwrap();
        drop(file);
        let read = read_events(&path).unwrap();
        assert_eq!(read.events.len(), 1);
        assert!(read.torn_last.is_some());
        // Appending after a torn line starts a clean line and keeps seq going;
        // the torn fragment stays in the file and is reported, not dropped.
        let mut log = EventLog::open(&path).unwrap();
        assert_eq!(log.append(started()).unwrap().seq, 2);
        let read = read_events(&path).unwrap();
        assert_eq!(read.events.len(), 2);
        assert_eq!(read.bad_lines.len(), 1);
        assert!(read.torn_last.is_none());
        assert!(read.bad_lines[0].contains("assis"));
    }

    #[test]
    fn mid_file_garbage_is_kept_out_but_reported() {
        let path = temp_path("bad");
        let mut file = std::fs::File::create(&path).unwrap();
        let a = serde_json::to_string(&LogEvent::new(1, started())).unwrap();
        let b = serde_json::to_string(&LogEvent::new(3, started())).unwrap();
        write!(file, "{a}\nnot json\n{b}\n").unwrap();
        drop(file);
        let read = read_events(&path).unwrap();
        assert_eq!(read.events.len(), 2);
        assert_eq!(read.bad_lines.len(), 1);
        assert!(read.torn_last.is_none());
    }

    #[test]
    fn empty_file_reads_empty() {
        let path = temp_path("empty");
        std::fs::write(&path, b"").unwrap();
        let read = read_events(&path).unwrap();
        assert!(read.events.is_empty());
        assert!(read.torn_last.is_none());
    }
}
