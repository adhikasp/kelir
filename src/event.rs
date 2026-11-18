//! The event log: the only source of truth for a session.
//!
//! One JSON object per line, `{"seq": n, "ts": "<RFC 3339>", "type": …}`,
//! `seq` monotonic from 1, fsync'd per batch. A torn last line is tolerated on
//! read (skipped and reported), never half-applied.
//!
//! Payload rule: a string field over [`PAYLOAD_LIMIT`] is never inline. It is
//! written to `blobs/<seq>-<field>` in the session dir and the event holds
//! `{"spilled": true, "path": …, "bytes": N, "preview": …}` instead; `fold`
//! reads the blob back, so the request bytes (and the cache prefix) are the
//! same as if it had been inline.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Event payloads over 10 KB are never inline (the spec's number).
pub const PAYLOAD_LIMIT: usize = 10 * 1024;
/// How much of a spilled payload the event keeps for humans and gates.
pub const PREVIEW_BYTES: usize = 1024;

pub fn now_ts() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// A spilled payload: the event holds this object in place of the value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlobRef {
    pub spilled: bool,
    /// Relative to the session dir, forward slashes: `blobs/<seq>-<field>`.
    pub path: String,
    pub bytes: u64,
    pub preview: String,
}

/// A value in an event: inline, or spilled to a blob file.
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    Inline(Value),
    Blob(BlobRef),
}

impl Payload {
    pub fn text(text: impl Into<String>) -> Self {
        Payload::Inline(Value::String(text.into()))
    }

    pub fn value(value: Value) -> Self {
        Payload::Inline(value)
    }

    pub fn is_spilled(&self) -> bool {
        matches!(self, Payload::Blob(_))
    }

    /// The literal value, when it was small enough to stay inline.
    pub fn inline(&self) -> Option<&Value> {
        match self {
            Payload::Inline(value) => Some(value),
            Payload::Blob(_) => None,
        }
    }

    pub fn inline_str(&self) -> Option<&str> {
        self.inline().and_then(Value::as_str)
    }

    /// The value, reading the blob back if needed.
    pub fn resolve(&self, session_dir: &Path) -> Result<Value, String> {
        match self {
            Payload::Inline(value) => Ok(value.clone()),
            Payload::Blob(blob) => {
                let path = session_dir.join(&blob.path);
                let text = std::fs::read_to_string(&path).map_err(|error| {
                    format!("cannot read spilled payload {}: {error}", path.display())
                })?;
                serde_json::from_str(&text).map_err(|error| {
                    format!("spilled payload {} is not JSON: {error}", path.display())
                })
            }
        }
    }

    /// The text, reading the blob back if needed (text blobs hold raw UTF-8).
    pub fn resolve_text(&self, session_dir: &Path) -> Result<String, String> {
        match self {
            Payload::Inline(Value::String(text)) => Ok(text.clone()),
            Payload::Inline(value) => Ok(value.to_string()),
            Payload::Blob(blob) => {
                let path = session_dir.join(&blob.path);
                std::fs::read_to_string(&path).map_err(|error| {
                    format!("cannot read spilled payload {}: {error}", path.display())
                })
            }
        }
    }
}

impl Default for Payload {
    fn default() -> Self {
        Payload::Inline(Value::Null)
    }
}

impl From<String> for Payload {
    fn from(text: String) -> Self {
        Payload::text(text)
    }
}

impl From<&str> for Payload {
    fn from(text: &str) -> Self {
        Payload::text(text)
    }
}

impl From<Value> for Payload {
    fn from(value: Value) -> Self {
        Payload::value(value)
    }
}

impl Serialize for Payload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Payload::Inline(value) => value.serialize(serializer),
            Payload::Blob(blob) => blob.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Payload {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(match blob_ref(&value) {
            Some(blob) => Payload::Blob(blob),
            None => Payload::Inline(value),
        })
    }
}

/// Recognise the spilled-payload object. The `blobs/` prefix keeps a model's
/// tool arguments from ever being mistaken for one.
fn blob_ref(value: &Value) -> Option<BlobRef> {
    if !value.get("spilled")?.as_bool()? {
        return None;
    }
    let path = value.get("path")?.as_str()?;
    if !path.starts_with("blobs/") {
        return None;
    }
    Some(BlobRef {
        spilled: true,
        path: path.to_string(),
        bytes: value.get("bytes")?.as_u64()?,
        preview: value.get("preview")?.as_str()?.to_string(),
    })
}

/// Writes oversized payloads into `<session>/blobs/`.
#[derive(Debug, Clone)]
pub struct Blobs {
    dir: PathBuf,
}

impl Blobs {
    pub fn new(session_dir: &Path) -> Self {
        Self {
            dir: session_dir.join("blobs"),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Inline when small enough, otherwise a blob the fold can read back.
    pub fn spill_text(&self, seq: u64, field: &str, text: &str) -> Result<Payload, String> {
        if text.len() <= PAYLOAD_LIMIT {
            return Ok(Payload::text(text));
        }
        Ok(Payload::Blob(self.write(seq, field, text)?))
    }

    pub fn spill_value(&self, seq: u64, field: &str, value: &Value) -> Result<Payload, String> {
        let text = serde_json::to_string(value)
            .map_err(|error| format!("cannot serialise a payload for spilling: {error}"))?;
        if text.len() <= PAYLOAD_LIMIT {
            return Ok(Payload::Inline(value.clone()));
        }
        Ok(Payload::Blob(self.write(seq, field, &text)?))
    }

    fn write(&self, seq: u64, field: &str, text: &str) -> Result<BlobRef, String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|error| format!("cannot create {}: {error}", self.dir.display()))?;
        let name = format!("{seq}-{field}");
        let path = self.dir.join(&name);
        std::fs::write(&path, text.as_bytes())
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        Ok(BlobRef {
            spilled: true,
            path: format!("blobs/{name}"),
            bytes: text.len() as u64,
            preview: preview(text),
        })
    }
}

fn preview(text: &str) -> String {
    if text.len() <= PREVIEW_BYTES {
        return text.to_string();
    }
    let mut end = PREVIEW_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Apply the payload rule to every string field of an event.
pub fn spill_payloads(body: EventBody, blobs: &Blobs, seq: u64) -> Result<EventBody, String> {
    Ok(match body {
        EventBody::SessionStarted {
            id,
            parent,
            depth,
            cwd,
            os,
            kelir_version,
            provider,
            model,
            system,
            sources,
            tools,
        } => EventBody::SessionStarted {
            id,
            parent,
            depth,
            cwd,
            os,
            kelir_version,
            provider,
            model,
            system: SystemInfo {
                text: spill_payload(blobs, seq, "system.text", system.text)?,
                hash: system.hash,
            },
            sources,
            tools,
        },
        EventBody::User {
            text,
            from,
            delivery,
            attachments,
        } => EventBody::User {
            text: spill_payload(blobs, seq, "user.text", text)?,
            from,
            delivery,
            attachments,
        },
        EventBody::Source {
            kind,
            name,
            text,
            path,
        } => EventBody::Source {
            kind,
            name,
            text: text
                .map(|text| spill_payload(blobs, seq, "source.text", text))
                .transpose()?,
            path,
        },
        EventBody::Assistant {
            step,
            text,
            reasoning,
            tool_calls,
            finish_reason,
        } => {
            let mut spilled_calls = Vec::with_capacity(tool_calls.len());
            for (index, call) in tool_calls.into_iter().enumerate() {
                let field = format!("tool_calls.{index}.args");
                spilled_calls.push(LoggedToolCall {
                    id: call.id,
                    name: call.name,
                    args: spill_payload(blobs, seq, &field, call.args)?,
                });
            }
            EventBody::Assistant {
                step,
                text: spill_payload(blobs, seq, "text", text)?,
                reasoning: reasoning
                    .map(|reasoning| spill_payload(blobs, seq, "reasoning", reasoning))
                    .transpose()?,
                tool_calls: spilled_calls,
                finish_reason,
            }
        }
        EventBody::ToolStarted {
            call_id,
            name,
            input,
            pid,
        } => EventBody::ToolStarted {
            call_id,
            name,
            input: spill_payload(blobs, seq, "input", input)?,
            pid,
        },
        EventBody::ToolSettled {
            call_id,
            outcome,
            exit_code,
            bytes,
            lines,
            out_path,
            spilled,
            result,
        } => EventBody::ToolSettled {
            call_id,
            outcome,
            exit_code,
            bytes,
            lines,
            out_path,
            spilled,
            result: spill_payload(blobs, seq, "result", result)?,
        },
        EventBody::TurnEnded {
            outcome,
            final_text,
        } => EventBody::TurnEnded {
            outcome,
            final_text: spill_payload(blobs, seq, "final_text", final_text)?,
        },
        other => other,
    })
}

fn spill_payload(
    blobs: &Blobs,
    seq: u64,
    field: &str,
    payload: Payload,
) -> Result<Payload, String> {
    match payload {
        Payload::Inline(Value::String(text)) => blobs.spill_text(seq, field, &text),
        Payload::Inline(value) => blobs.spill_value(seq, field, &value),
        already_spilled => Ok(already_spilled),
    }
}

/// A tool call as recorded in an `assistant` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoggedToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub args: Payload,
}

/// The frozen request prefix, recorded once in `session_started`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemInfo {
    pub text: Payload,
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
        text: Payload,
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
        text: Option<Payload>,
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
        text: Payload,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<Payload>,
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
        input: Payload,
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
        result: Payload,
    },
    TurnEnded {
        outcome: String,
        final_text: Payload,
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

    /// The seq the next appended event will get (used to name blob files).
    pub fn next_seq(&self) -> u64 {
        self.seq
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

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kelir-event-test-{}-{}", ulid::Ulid::new(), name));
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
                    args: json!({"command": "ls"}).into(),
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
                input: json!({"command": "ls"}).into(),
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
    fn inline_payloads_serialise_as_plain_values() {
        assert_eq!(
            serde_json::to_value(Payload::text("hi")).unwrap(),
            json!("hi")
        );
        assert_eq!(
            serde_json::to_value(Payload::value(json!({"a": 1}))).unwrap(),
            json!({"a": 1})
        );
    }

    #[test]
    fn blob_payloads_round_trip_through_serde() {
        let blob = Payload::Blob(BlobRef {
            spilled: true,
            path: "blobs/9-text".into(),
            bytes: 11,
            preview: "hello".into(),
        });
        let value = serde_json::to_value(&blob).unwrap();
        assert_eq!(value["spilled"], true);
        assert_eq!(value["path"], "blobs/9-text");
        assert_eq!(value["bytes"], 11);
        assert_eq!(value["preview"], "hello");
        let back: Payload = serde_json::from_value(value).unwrap();
        assert_eq!(back, blob);
    }

    #[test]
    fn an_object_that_only_looks_like_a_blob_stays_inline() {
        let value = json!({"spilled": true, "path": "not-a-blob/1", "bytes": 1, "preview": "x"});
        let payload: Payload = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(payload, Payload::Inline(value));
        let value = json!({"spilled": "yes"});
        assert_eq!(
            serde_json::from_value::<Payload>(value.clone()).unwrap(),
            Payload::Inline(value)
        );
    }

    #[test]
    fn small_payloads_stay_inline() {
        let dir = temp_dir("small");
        let blobs = Blobs::new(&dir);
        let payload = blobs.spill_text(1, "text", "short").unwrap();
        assert_eq!(payload, Payload::text("short"));
        assert!(!blobs.dir().exists());
    }

    #[test]
    fn oversized_payloads_are_written_to_blobs_and_resolve_back() {
        let dir = temp_dir("spill");
        let blobs = Blobs::new(&dir);
        let big = "x".repeat(50_000);
        let payload = blobs.spill_text(7, "assistant.text", &big).unwrap();
        let blob = match &payload {
            Payload::Blob(blob) => blob.clone(),
            other => panic!("expected a blob, got {other:?}"),
        };
        assert_eq!(blob.path, "blobs/7-assistant.text");
        assert_eq!(blob.bytes, 50_000);
        assert_eq!(blob.preview.len(), PREVIEW_BYTES);
        assert_eq!(payload.resolve_text(&dir).unwrap(), big);
        assert!(dir.join("blobs").join("7-assistant.text").exists());
    }

    #[test]
    fn oversized_values_are_written_as_json() {
        let dir = temp_dir("value");
        let blobs = Blobs::new(&dir);
        let value = json!({"new": "y".repeat(20_000)});
        let payload = blobs.spill_value(3, "input", &value).unwrap();
        assert!(payload.is_spilled());
        assert_eq!(payload.resolve(&dir).unwrap(), value);
    }

    #[test]
    fn spill_payloads_rewrites_every_string_field() {
        let dir = temp_dir("body");
        let blobs = Blobs::new(&dir);
        let big = "z".repeat(PAYLOAD_LIMIT + 1);
        let body = EventBody::Assistant {
            step: 2,
            text: big.clone().into(),
            reasoning: Some(big.clone().into()),
            tool_calls: vec![LoggedToolCall {
                id: "c1".into(),
                name: "edit".into(),
                args: json!({"new": big.clone()}).into(),
            }],
            finish_reason: Some("tool_calls".into()),
        };
        let spilled = spill_payloads(body, &blobs, 11).unwrap();
        let value = serde_json::to_value(&spilled).unwrap();
        assert_eq!(value["text"]["spilled"], true);
        assert_eq!(value["text"]["path"], "blobs/11-text");
        assert_eq!(value["reasoning"]["path"], "blobs/11-reasoning");
        assert_eq!(
            value["tool_calls"][0]["args"]["path"],
            "blobs/11-tool_calls.0.args"
        );
        assert!(serde_json::to_string(&spilled).unwrap().len() < 10_000);
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
        let dir = temp_dir("seq");
        let path = dir.join("events.jsonl");
        let mut log = EventLog::open(&path).unwrap();
        assert_eq!(log.next_seq(), 1);
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
        let dir = temp_dir("torn");
        let path = dir.join("events.jsonl");
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
        let dir = temp_dir("bad");
        let path = dir.join("events.jsonl");
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
        let dir = temp_dir("empty");
        let path = dir.join("events.jsonl");
        std::fs::write(&path, b"").unwrap();
        let read = read_events(&path).unwrap();
        assert!(read.events.is_empty());
        assert!(read.torn_last.is_none());
    }
}
