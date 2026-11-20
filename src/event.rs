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
use std::io::{self, Write};
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
            reasoning_effort,
            key_origin,
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
            reasoning_effort,
            key_origin,
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
            dir,
        } => EventBody::ToolStarted {
            call_id,
            name,
            input: spill_payload(blobs, seq, "input", input)?,
            pid,
            dir,
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
        // A background job's command line is the only field that could grow:
        // the rest are ids, paths and numbers.
        EventBody::BgStarted {
            bg_id,
            call_id,
            cmd,
            pid,
            out_path,
            pubkey,
        } => EventBody::BgStarted {
            bg_id,
            call_id,
            cmd: spill_payload(blobs, seq, "bg.cmd", cmd)?,
            pid,
            out_path,
            pubkey,
        },
        EventBody::BgSettled {
            bg_id,
            call_id,
            cmd,
            exit_code,
            state,
            out_path,
        } => EventBody::BgSettled {
            bg_id,
            call_id,
            cmd: spill_payload(blobs, seq, "bg.cmd", cmd)?,
            exit_code,
            state,
            out_path,
        },
        EventBody::TurnEnded {
            outcome,
            reason,
            final_text,
        } => EventBody::TurnEnded {
            outcome,
            reason,
            final_text: spill_payload(blobs, seq, "final_text", final_text)?,
        },
        EventBody::Compacted {
            through_seq,
            summary,
            pinned_seqs,
            by,
            tokens_before,
            tokens_after,
            cache_restart,
        } => EventBody::Compacted {
            through_seq,
            summary: spill_payload(blobs, seq, "summary", summary)?,
            pinned_seqs,
            by,
            tokens_before,
            tokens_after,
            cache_restart,
        },
        EventBody::Verb {
            name,
            args,
            from,
            boundary,
            error,
        } => EventBody::Verb {
            name,
            args: spill_payload(blobs, seq, "verb.args", args)?,
            from,
            boundary,
            error,
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
    /// The file the text came from; empty for the embedded base prompt (never
    /// a sentinel such as `embedded`, which a reader could mistake for a file).
    pub path: String,
    /// `prompt`, `instruction` or `skill`: what the source *is*, recorded where
    /// it was found rather than guessed from its name. Absent in a log written
    /// before M5-A's fixes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
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
        /// The `reasoning_effort` sent with every request of this session, when
        /// one is set. Recorded so the log and the request agree: the value is
        /// part of what the provider was asked for, and a reader of a session
        /// should not have to guess it (STEER-6).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
        /// Where this session's key came from (`env OPENROUTER_API_KEY`,
        /// `opencode auth export opencode`), never the key itself. A log reader
        /// can then tell a copied env credential from opencode's own store, and
        /// a stale one is diagnosed from the log alone (STEER-2).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key_origin: Option<String>,
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
        /// `None` for a model step. `"compact"` marks the summarising request,
        /// which is not a step of the turn: it has no `step_started` of its
        /// own, but its spend belongs in the log all the same (M3 review N4).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
    },
    /// Every verb received, applied or not.
    Verb {
        name: String,
        #[serde(default)]
        args: Payload,
        #[serde(rename = "from")]
        from: String,
        /// Where it applies (`now`, `next_step`, `queued`, `turn_end`) or
        /// `rejected` when it was refused.
        boundary: String,
        /// Why it was refused, when it was.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    ToolStarted {
        call_id: String,
        name: String,
        input: Payload,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
        /// The call's own directory under `commands/` (its job id, STEER-7).
        /// A resumed session adopts it as a job it may be told about: the
        /// registry is this process's memory, and a resume rebuilds it from
        /// the log. Absent from a log written by an older kelir.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dir: Option<String>,
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
        /// Why the turn paused (`steps`: an explicit `max_steps` cap,
        /// `provider`: the retry budget ran out). Absent for every other
        /// outcome.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        final_text: Payload,
    },
    /// A background job exists: an explicit `kelir bg run` (which reports
    /// itself over the session socket) or a bash call promoted past
    /// `promote_after`. `bg_id` is the job's id: the call's output directory
    /// name for a promoted call, `bg_<ulid>` for an explicit job. `call_id` is
    /// the provider call id of the bash call that started it, when there was
    /// one.
    BgStarted {
        bg_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        cmd: Payload,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
        out_path: String,
        /// The job's Ed25519 **public** key (hex), the one its shim's status
        /// records verify under. A resume adopts it from here, so a job that
        /// outlived its session still settles with its real exit code
        /// (STEER-7 round 2). Not a secret.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pubkey: Option<String>,
    },
    /// A background job settled (exited or was cancelled) while this session
    /// owned its log. `state` is `exited` or `cancelled`; the wake-up message
    /// the model reads is the `user` event that follows it.
    BgSettled {
        bg_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        cmd: Payload,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        state: String,
        out_path: String,
    },
    /// A compaction: everything up to `through_seq` is replaced by `summary`,
    /// except the pinned messages. `by` is `user`, `agent` or `auto`; the cache
    /// restart is inherent (it is the reason the field is here) and recorded so
    /// a reader of the log does not have to infer it from the seq numbers.
    Compacted {
        through_seq: u64,
        summary: Payload,
        #[serde(default)]
        pinned_seqs: Vec<u64>,
        by: String,
        tokens_before: u64,
        tokens_after: u64,
        #[serde(default)]
        cache_restart: bool,
    },
    /// A session reopened from its log: where it continued from, and which
    /// calls were started but never settled (they are reported to the model as
    /// unknown outcomes and never re-run).
    Resumed {
        from_seq: u64,
        #[serde(default)]
        unsettled: Vec<String>,
    },
    Error {
        #[serde(rename = "where")]
        location: String,
        message: String,
        retryable: bool,
        /// The HTTP status, when there was a response (`provider` errors).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        /// Which provider request failed, counting from 1 for this request.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<u32>,
        /// How long kelir will wait before the next attempt. Absent when it
        /// will not retry, so a reader can tell "waited 34 s" from "gave up".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_in_ms: Option<u64>,
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
            EventBody::Verb { .. } => "verb",
            EventBody::ToolStarted { .. } => "tool_started",
            EventBody::ToolSettled { .. } => "tool_settled",
            EventBody::TurnEnded { .. } => "turn_ended",
            EventBody::BgStarted { .. } => "bg_started",
            EventBody::BgSettled { .. } => "bg_settled",
            EventBody::Compacted { .. } => "compacted",
            EventBody::Resumed { .. } => "resumed",
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
        // A torn last line (a crash mid-write) is cut off here, so the log holds
        // whole events only: no unparseable line, and never two lines with the
        // same seq (M3 review S3). The fragment goes to `events.jsonl.torn`
        // first, so nothing is dropped silently.
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

/// Cut a torn final line off a log, keeping the fragment in a sidecar.
///
/// A final line that is a whole event without its terminator is *kept*: it is
/// what a crash between a payload write and its newline leaves, and dropping it
/// would lose a committed event. Anything shorter cannot be parsed, and is
/// copied to `<log>.torn` before it is truncated away, so a reader of the log
/// (`jq`, the TUI, the next resume) never sees it again.
fn remove_torn_tail(path: &Path) -> io::Result<()> {
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
fn stash_tail(path: &Path, tail: &[u8]) -> io::Result<()> {
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
        let tail = &bytes[last_complete..];
        // A crash can land between the payload and its newline. If what is
        // there is a whole event, keep it: losing a complete event would be
        // worse than a missing terminator. Anything less is torn.
        match serde_json::from_slice::<LogEvent>(tail) {
            Ok(event) => out.events.push(event),
            Err(_) => out.torn_last = Some(String::from_utf8_lossy(tail).into_owned()),
        }
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
            reasoning_effort: None,
            key_origin: None,
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
                kind: None,
            },
            EventBody::Usage {
                step: 1,
                prompt_tokens: 40,
                completion_tokens: 60,
                cached_tokens: 0,
                cache_write_tokens: 0,
                cost: 0.002,
                allowance: None,
                kind: Some("compact".into()),
            },
            EventBody::ToolStarted {
                call_id: "call_1".into(),
                name: "bash".into(),
                input: json!({"command": "ls"}).into(),
                pid: Some(42),
                dir: Some("9-1-1".into()),
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
                reason: None,
                final_text: "done".into(),
            },
            EventBody::BgStarted {
                bg_id: "bg_01".into(),
                call_id: Some("call_1".into()),
                cmd: "sleep 60".into(),
                pid: Some(4242),
                out_path: "bg/bg_01.out".into(),
                pubkey: None,
            },
            EventBody::BgSettled {
                bg_id: "bg_01".into(),
                call_id: None,
                cmd: "sleep 60".into(),
                exit_code: Some(0),
                state: "exited".into(),
                out_path: "bg/bg_01.out".into(),
            },
            EventBody::Compacted {
                through_seq: 7,
                summary: "we did the first half".into(),
                pinned_seqs: vec![2],
                by: "agent".into(),
                tokens_before: 900,
                tokens_after: 120,
                cache_restart: true,
            },
            EventBody::Resumed {
                from_seq: 9,
                unsettled: vec!["call_1".into()],
            },
            EventBody::Error {
                location: "provider".into(),
                message: "boom".into(),
                retryable: true,
                status: Some(500),
                attempt: Some(2),
                retry_in_ms: Some(1_500),
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
            EventBody::Verb {
                name: "say".into(),
                args: json!({"text": "hi", "now": true}).into(),
                from: "ctl".into(),
                boundary: "now".into(),
                error: None,
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
        // Reopening cuts the fragment off, so the log holds whole lines only:
        // `jq` reads it, seq stays monotonic and the next resume reports
        // nothing. The fragment itself is in the sidecar, not dropped.
        let mut log = EventLog::open(&path).unwrap();
        assert_eq!(log.append(started()).unwrap().seq, 2);
        let read = read_events(&path).unwrap();
        assert_eq!(read.events.len(), 2);
        assert_eq!(read.events[1].seq, 2);
        assert!(read.bad_lines.is_empty(), "{read:?}");
        assert!(read.torn_last.is_none(), "{read:?}");
        let sidecar = std::fs::read_to_string(path.with_file_name("events.jsonl.torn")).unwrap();
        assert!(sidecar.contains("assis"), "{sidecar}");
    }

    #[test]
    fn a_torn_tail_that_is_a_whole_event_keeps_its_newline() {
        let dir = temp_dir("torn-whole");
        let path = dir.join("events.jsonl");
        let good = serde_json::to_string(&LogEvent::new(1, started())).unwrap();
        let second = serde_json::to_string(&LogEvent::new(2, samples()[1].clone())).unwrap();
        std::fs::write(&path, format!("{good}\n{second}")).unwrap();
        let mut log = EventLog::open(&path).unwrap();
        assert_eq!(log.append(started()).unwrap().seq, 3);
        let read = read_events(&path).unwrap();
        assert_eq!(read.events.len(), 3);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert!(!dir.join("events.jsonl.torn").exists());
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
    fn a_torn_line_is_handled_at_every_byte_offset() {
        let dir = temp_dir("torn-offsets");
        let good = serde_json::to_string(&LogEvent::new(1, started())).unwrap();
        let second = serde_json::to_string(&LogEvent::new(2, samples()[1].clone())).unwrap();
        for cut in 0..=second.len() {
            let path = dir.join(format!("events-{cut}.jsonl"));
            let mut bytes = Vec::new();
            bytes.extend_from_slice(good.as_bytes());
            bytes.push(b'\n');
            bytes.extend_from_slice(&second.as_bytes()[..cut]);
            std::fs::write(&path, &bytes).unwrap();
            // The only cut that parses is the whole line (a crash between the
            // payload and its newline); it is kept, and every shorter prefix is
            // reported as torn.
            let kept = usize::from(cut == second.len());
            let read = read_events(&path).unwrap();
            assert_eq!(read.events.len(), 1 + kept, "cut {cut}: {read:?}");
            assert_eq!(read.events[0].seq, 1);
            if cut > 0 && kept == 0 {
                assert!(
                    read.torn_last.is_some() || read.bad_lines.len() == 1,
                    "cut {cut}: {read:?}"
                );
            }
            // Reopening after the fragment starts a clean line and keeps seq
            // monotonic: the next event is never swallowed by the fragment.
            let next = 2 + kept as u64;
            let mut log = EventLog::open(&path).unwrap();
            assert_eq!(log.append(started()).unwrap().seq, next, "cut {cut}");
            log.sync().unwrap();
            let read = read_events(&path).unwrap();
            assert_eq!(read.events.len(), 2 + kept, "cut {cut}: {read:?}");
            assert_eq!(read.events.last().unwrap().seq, next);
        }
    }

    #[test]
    fn a_batch_is_whole_on_disk_before_sync_returns() {
        let dir = temp_dir("batch");
        let path = dir.join("events.jsonl");
        let mut log = EventLog::open(&path).unwrap();
        for body in samples().into_iter().take(4) {
            log.append(body).unwrap();
        }
        log.sync().unwrap();
        // A fresh read (a new process, a new handle) sees every event of the
        // batch and nothing partial: the file ends on a line boundary.
        let read = read_events(&path).unwrap();
        assert_eq!(read.events.len(), 4);
        assert!(read.torn_last.is_none(), "{read:?}");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        // Every line is a whole event, in seq order.
        assert_eq!(
            read.events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
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
