//! The event log: the only source of truth for a session.
//! One JSON object per line, `{"seq": n, "ts": "<RFC 3339>", "type": …}`, `seq` monotonic
//! from 1, fsync'd per batch.
//! A torn last line is tolerated on read (skipped and reported), never half-applied.

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

/// A session's title, derived deterministically from its first prompt.
pub fn title_from_prompt(prompt: &str) -> String {
    let first = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let flat: String = first.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = flat.trim();
    if flat.is_empty() {
        return "untitled".to_string();
    }
    let max = 60;
    if flat.chars().count() <= max {
        return flat.to_string();
    }
    format!("{}…", flat.chars().take(max - 1).collect::<String>())
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

/// Build a call-start event. `kind` is `tool`, `mcp`, `bg` or `child`.
pub fn call_start(id: impl Into<String>, kind: &str, info: CallInfo) -> EventBody {
    EventBody::CallStarted(CallStart {
        id: id.into(),
        kind: kind.to_string(),
        info,
    })
}

/// Build a call-settled event with the given outcome. `CallSettle::default()` fills
/// unused fields; callers set only those their kind reads.
pub fn call_settle(
    id: impl Into<String>,
    kind: &str,
    outcome: &str,
    settle: CallSettle,
) -> EventBody {
    EventBody::CallSettled(CallSettle {
        id: id.into(),
        kind: kind.to_string(),
        outcome: outcome.to_string(),
        ..settle
    })
}

impl CallInfo {
    /// A call whose payload is its command or tool arguments.
    pub fn named(name: impl Into<String>, input: Payload) -> Self {
        Self {
            name: name.into(),
            input: Some(input),
            ..Self::default()
        }
    }

    pub fn bg(cmd: Payload) -> Self {
        Self {
            name: "bg".into(),
            cmd: Some(cmd),
            ..Self::default()
        }
    }

    pub fn mcp(server: &str, tool: &str, args: Payload) -> Self {
        Self {
            name: format!("{server}/{tool}"),
            input: Some(args),
            ..Self::default()
        }
    }

    pub fn child(
        task: Payload,
        session: &str,
        parent: &str,
        depth: u32,
        kind: &str,
        background: bool,
        model: &str,
    ) -> Self {
        Self {
            name: kind.to_string(),
            input: Some(task),
            session: Some(session.to_string()),
            parent: Some(parent.to_string()),
            depth: Some(depth),
            background: Some(background),
            model: Some(model.to_string()),
            ..Self::default()
        }
    }
}

/// Apply the payload rule to every string field of an event.
pub fn spill_payloads(body: EventBody, blobs: &Blobs, seq: u64) -> Result<EventBody, String> {
    Ok(match body {
        EventBody::SessionStarted {
            id,
            format,
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
            format,
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
            usage,
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
                usage,
            }
        }
        EventBody::CallStarted(mut start) => {
            if let Some(input) = start.info.input.take() {
                start.info.input = Some(spill_payload(blobs, seq, "input", input)?);
            }
            EventBody::CallStarted(start)
        }
        EventBody::CallSettled(mut settle) => {
            if let Some(result) = settle.result.take() {
                settle.result = Some(spill_payload(blobs, seq, "result", result)?);
            }
            EventBody::CallSettled(settle)
        }
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

/// Token counts for one provider request, carried on its `assistant` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowance: Option<Value>,
    /// `compact` for a summarising request, otherwise absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// The payload of one call event. Every field is read by fold, resume, a feature
/// or the viewer; nothing is carried for an older log format.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CallInfo {
    /// bash tool name, MCP `server/tool`, background command name, child kind.
    pub name: String,
    /// Tool arguments, MCP arguments or a child task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Payload>,
    /// A background job's command line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmd: Option<Payload>,
    /// The per-call directory a bash tool wrote into.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// Child session id, and its parent session id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Child nesting depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    /// Whether a child runs in the background.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    /// The model a child was started with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// A call's one lifecycle: intent, then one commit. `kind` is `tool`, `mcp`,
/// `bg` or `child`; `info` carries only the fields that kind uses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CallStart {
    pub id: String,
    pub kind: String,
    pub info: CallInfo,
}

/// The settlement of a call. Only fields a reader uses are present; the fields
/// a kind does not write are absent from the log rather than null.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CallSettle {
    pub id: String,
    pub kind: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub out_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spilled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Payload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

impl CallSettle {
    pub fn exit_code(mut self, value: Option<i32>) -> Self {
        self.exit_code = value;
        self
    }
    pub fn bytes(mut self, value: u64) -> Self {
        self.bytes = Some(value);
        self
    }
    pub fn lines(mut self, value: u64) -> Self {
        self.lines = Some(value);
        self
    }
    pub fn out_path(mut self, value: impl Into<String>) -> Self {
        self.out_path = Some(value.into());
        self
    }
    pub fn spilled(mut self, value: bool) -> Self {
        self.spilled = Some(value);
        self
    }
    pub fn result(mut self, value: Payload) -> Self {
        self.result = Some(value);
        self
    }
    pub fn state(mut self, value: impl Into<String>) -> Self {
        self.state = Some(value.into());
        self
    }
}

/// A diagnostic note: a threshold crossing or a notification-hook outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub kind: String,
    pub text: String,
}

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
    /// `prompt`, `instruction` or `skill`: what the source *is*, recorded where it was found
    /// rather than guessed from its name. Absent in a log written before's fixes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub hash: String,
}

/// The payload of a log line. `type` is the serde tag. Every field is
/// required on read: event log v1 has no readers for older logs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventBody {
    SessionStarted {
        /// Session id; also the provider's prompt-cache key.
        id: String,
        /// `1`: a log written by any other format is refused.
        format: u8,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
        /// Where the key came from, never the key itself.
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
        /// Usage from the final stream chunk of this step, when reported.
        usage: Option<Usage>,
    },
    /// Usage from a provider call with no assistant answer of its own
    /// (compaction, gate scoring). `kind` names the caller.
    StepUsage(Usage),
    Verb {
        name: String,
        #[serde(default)]
        args: Payload,
        #[serde(rename = "from")]
        from: String,
        boundary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    CallStarted(CallStart),
    CallSettled(CallSettle),
    TurnEnded {
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        final_text: Payload,
    },
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
    Error {
        #[serde(rename = "where")]
        location: String,
        message: String,
        retryable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_in_ms: Option<u64>,
    },
    SessionEnded {
        reason: String,
    },
    SessionTitle {
        title: String,
    },
    /// A notification hook outcome or a threshold crossing. `kind` says which;
    /// neither is part of the conversation the model reads.
    Note(Note),
    Gate {
        gate: String,
        event: String,
        decision: String,
        reason: String,
        ms: u64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        rewritten: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
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

#[cfg(test)]
pub fn test_usage(prompt: u64, completion: u64, cached: u64, kind: Option<&str>) -> Usage {
    Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        cached_tokens: cached,
        cache_write_tokens: 0,
        allowance: None,
        kind: kind.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kelir-event-test-{}-{}",
            ulid::Ulid::generate(),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn started() -> EventBody {
        EventBody::SessionStarted {
            id: "ses_01".into(),
            format: 1,
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
            EventBody::Source {
                kind: "hint".into(),
                name: "note".into(),
                text: Some("try this".into()),
                path: None,
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
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    cached_tokens: 4,
                    cache_write_tokens: 2,
                    allowance: None,
                    kind: None,
                }),
            },
            EventBody::StepUsage(Usage {
                prompt_tokens: 40,
                completion_tokens: 60,
                cached_tokens: 0,
                cache_write_tokens: 0,
                allowance: None,
                kind: Some("compact".into()),
            }),
            EventBody::Verb {
                name: "say".into(),
                args: json!({"text": "hi", "now": true}).into(),
                from: "ctl".into(),
                boundary: "now".into(),
                error: None,
            },
            call_start(
                "call_1",
                "tool",
                CallInfo {
                    name: "bash".into(),
                    input: Some(json!({"command": "ls"}).into()),
                    dir: Some("9-1-1".into()),
                    ..CallInfo::default()
                },
            ),
            call_settle(
                "call_1",
                "tool",
                "ok",
                CallSettle {
                    exit_code: Some(0),
                    bytes: Some(3),
                    lines: Some(1),
                    out_path: Some("commands/call_1/out".into()),
                    spilled: Some(false),
                    result: Some("ok\n".into()),
                    ..CallSettle::default()
                },
            ),
            call_start(
                "mcp_01",
                "mcp",
                CallInfo::mcp("demo", "echo", json!({"text": "hi"}).into()),
            ),
            call_start("bg_01", "bg", CallInfo::named("bg", "sleep 60".into())),
            call_start(
                "chd_01",
                "child",
                CallInfo::child(
                    "look at the tests".into(),
                    "ses_02",
                    "ses_01",
                    1,
                    "child",
                    true,
                    "test/model",
                ),
            ),
            EventBody::Compacted {
                through_seq: 7,
                summary: "we did the first half".into(),
                pinned_seqs: vec![2],
                by: "agent".into(),
                tokens_before: 900,
                tokens_after: 120,
                cache_restart: true,
            },
            EventBody::Error {
                location: "provider".into(),
                message: "boom".into(),
                retryable: true,
                status: Some(500),
                attempt: Some(2),
                retry_in_ms: Some(1_500),
            },
            EventBody::Note(Note {
                kind: "warn_prompt_tokens".into(),
                text: "too long".into(),
            }),
            EventBody::Note(Note {
                kind: "notify".into(),
                text: "turn_done: 1 hook(s) fired".into(),
            }),
            EventBody::TurnEnded {
                outcome: "done".into(),
                reason: None,
                final_text: "done".into(),
            },
            EventBody::Gate {
                gate: "g".into(),
                event: "turn_end".into(),
                decision: "allow".into(),
                reason: String::new(),
                ms: 1,
                rewritten: true,
                error: None,
            },
            EventBody::SessionEnded {
                reason: "done".into(),
            },
            EventBody::SessionTitle {
                title: "a title".into(),
            },
        ]
    }

    #[test]
    fn names_track_the_event_types_samples_write() {
        let mut seen: Vec<_> = samples()
            .iter()
            .map(|body| LogEvent::new(1, body.clone()).event_type())
            .collect();
        seen.sort_unstable();
        seen.dedup();
        let mut names = EventBody::NAMES.to_vec();
        names.sort_unstable();
        assert_eq!(seen, names);
    }

    #[test]
    fn read_events_refuses_a_log_from_another_format() {
        let dir = temp_dir("format");
        let path = dir.join("events.jsonl");
        let mut first = serde_json::to_value(LogEvent::new(1, started())).unwrap();
        first["format"] = json!(2);
        std::fs::write(&path, format!("{first}\n")).unwrap();
        let error = read_events(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains(FORMAT_MESSAGE));

        let user = serde_json::to_string(&LogEvent::new(
            1,
            EventBody::User {
                text: "hi".into(),
                from: "cli".into(),
                delivery: "now".into(),
                attachments: vec![],
            },
        ))
        .unwrap();
        std::fs::write(&path, format!("{user}\n")).unwrap();
        let error = read_events(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains(FORMAT_MESSAGE));
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
            usage: None,
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
        // Reopening cuts the fragment off, so the log holds whole lines only: `jq` reads it, seq stays
        // monotonic and the next resume reports nothing.
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
            // The only cut that parses is the whole line (a crash between the payload and its newline);
            // it is kept, and every shorter prefix is reported as torn.
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
    fn a_title_is_the_first_prompt_line_flattened_and_bounded() {
        assert_eq!(
            title_from_prompt("  fix the  input gap\nand more"),
            "fix the input gap"
        );
        assert_eq!(title_from_prompt("\n\n  "), "untitled");
        let long = "a".repeat(200);
        let title = title_from_prompt(&long);
        assert_eq!(title.chars().count(), 60);
        assert!(title.ends_with('…'));
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
