//! Event body types: usage, calls, notes and the event enum.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::log::event::*;

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
    /// The diagnostic line the UI shows under a failed call. It is not a second
    /// copy of `result`: the result is what the model sees, this is what a human
    /// needs to see without unfolding anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail: Option<String>,
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
    pub fn tail(mut self, value: impl Into<String>) -> Self {
        self.tail = Some(value.into());
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
