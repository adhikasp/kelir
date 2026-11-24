//! Event payload representation, blobs and call helpers.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::log::event::*;

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
pub(super) fn blob_ref(value: &Value) -> Option<BlobRef> {
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
    pub dir: PathBuf,
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

pub(super) fn preview(text: &str) -> String {
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
