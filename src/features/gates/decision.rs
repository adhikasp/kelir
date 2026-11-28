use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::*;

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Allow {
        reason: String,
        context: Option<Context>,
        usage: Option<crate::provider::stream::Usage>,
    },
    Deny {
        reason: String,
        context: Option<Context>,
        usage: Option<crate::provider::stream::Usage>,
        /// How many times this turn the deny may be enforced; zero is the
        /// default of one.
        max_denies: u32,
    },
    Rewrite {
        reason: String,
        input: Value,
        context: Option<Context>,
        usage: Option<crate::provider::stream::Usage>,
    },
}

/// The longest `context` a gate may contribute, in characters.
pub const CONTEXT_MAX: usize = 32 * 1024;

/// The most the `context` field of a gate answer may add to the tail.
///
/// Extra context a gate contributes: appended as a `source` event at the tail,
/// after the user message, so the cached prefix is untouched.
#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub text: String,
    pub kind: String,
}

/// Take the shared fields out of a parsed decision so the runner can record
/// `context`/`usage` before it matches on the decision-specific fields.
pub(crate) fn split_answer(
    parsed: &mut Parsed,
) -> (Option<Context>, Option<crate::provider::stream::Usage>) {
    match parsed {
        Parsed::Allow { context, usage, .. }
        | Parsed::Deny { context, usage, .. }
        | Parsed::Rewrite { context, usage, .. } => (context.take(), usage.take()),
    }
}

/// Parse the optional `context` field shared by external and built-in gates.
fn answer_context(object: &serde_json::Map<String, Value>) -> Result<Option<Context>, String> {
    let Some(value) = object.get("context").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .ok_or_else(|| "`context` is not a string".to_string())?;
    if text.is_empty() {
        return Ok(None);
    }
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .unwrap_or("gate");
    let kind = crate::util::truncate_chars(kind, 64);
    let text = crate::util::truncate_chars(text, CONTEXT_MAX);
    Ok(Some(Context { text, kind }))
}

/// Parse a gate's stdout. Empty is `allow` (a gate that only has side effects prints
/// nothing); anything else must be one JSON object with a known `decision`.
pub fn parse_decision(stdout: &str) -> Result<Parsed, String> {
    let text = stdout.trim();
    if text.is_empty() {
        return Ok(Parsed::Allow {
            reason: String::new(),
            context: None,
            usage: None,
        });
    }
    if text.len() > OUTPUT_MAX {
        return Err(format!("output over {OUTPUT_MAX} bytes"));
    }
    let value: Value = serde_json::from_str(text).map_err(|error| {
        format!(
            "not JSON ({error}): {}",
            crate::util::truncate_chars(text, 80)
        )
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| "not a JSON object".to_string())?;
    let decision = object
        .get("decision")
        .and_then(Value::as_str)
        .ok_or_else(|| "no `decision` string".to_string())?;
    let reason = match object.get("reason") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(reason)) => crate::util::truncate_chars(reason, REASON_MAX),
        Some(_) => return Err("`reason` is not a string".into()),
    };
    let context = answer_context(object)?;
    let usage = match object.get("usage").filter(|usage| !usage.is_null()) {
        Some(Value::Object(usage)) => Some(crate::provider::stream::parse_usage(&Value::Object(
            usage.clone(),
        ))),
        Some(_) => return Err("`usage` is not an object".into()),
        None => None,
    };
    match decision {
        "allow" => Ok(Parsed::Allow {
            reason,
            context,
            usage,
        }),
        "deny" => Ok(Parsed::Deny {
            reason,
            context,
            usage,
            max_denies: match object.get("max_denies").filter(|v| !v.is_null()) {
                None => 0,
                Some(value) => match value.as_u64() {
                    Some(n) => u32::try_from(n)
                        .map_err(|_| "`max_denies` is a positive integer".to_string())?,
                    None => return Err("`max_denies` is a positive integer".into()),
                },
            },
        }),
        "rewrite" => {
            let input = object
                .get("input")
                .filter(|input| input.is_object())
                .ok_or_else(|| "`rewrite` needs an `input` object".to_string())?;
            Ok(Parsed::Rewrite {
                reason,
                input: input.clone(),
                context,
                usage,
            })
        }
        other => Err(format!(
            "unknown decision `{}`",
            crate::util::truncate_chars(other, 20)
        )),
    }
}

/// One gate run, as the `gate` event records it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub gate: String,
    pub event: String,
    pub decision: String,
    pub reason: String,
    pub ms: u64,
    pub rewritten: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Allow,
    Deny {
        gate: String,
        reason: String,
        /// How many times this turn the gate's deny may be enforced before the
        /// next one is only logged. One is what a gate that answers "not yet"
        /// wants; a timebox sets more, because the clock, not the count, is what
        /// should end it.
        max_denies: u32,
    },
    /// The event's value after every rewrite: `{"text"}` for a prompt or a final answer, the tool
    /// input for `before_tool`, `{"result"}` for `after_tool`.
    Rewrite(Value),
}

/// Every run of one event, and what they add up to.
#[derive(Debug, Clone, PartialEq)]
pub struct Chain {
    pub runs: Vec<Run>,
    pub verdict: Verdict,
    /// `context` from every gate that answered with one, in declaration order.
    pub contexts: Vec<Context>,
    /// Token usage a built-in reported from its own provider call.
    pub usages: Vec<(String, crate::provider::stream::Usage)>,
}

impl Chain {
    pub fn allow() -> Chain {
        Chain {
            runs: Vec::new(),
            verdict: Verdict::Allow,
            contexts: Vec::new(),
            usages: Vec::new(),
        }
    }
}

/// Apply a rewrite's `input` to the event's current payload, if its shape fits
/// the event. Returns the new payload and the rewritten value.
pub fn apply_rewrite(
    event: Event,
    gate: &str,
    payload: &mut Value,
    input: &Value,
) -> Result<Value, String> {
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("`rewrite` at {} needs input.{key} (a string)", event.name()))
    };
    match event {
        Event::PromptSubmit => {
            let new = text("text")?;
            payload["text"] = json!(new);
            Ok(json!({"text": new}))
        }
        Event::TurnEnd => {
            let new = text("text")?;
            payload["final_text"] = json!(new);
            Ok(json!({"text": new}))
        }
        Event::BeforeTool => {
            payload["input"] = input.clone();
            Ok(input.clone())
        }
        Event::AfterTool => {
            // `result` replaces what the model sees; `note` appends a line.
            let mut result = payload
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let mut changed = false;
            if let Some(new) = input.get("result").and_then(Value::as_str) {
                result = new.to_string();
                changed = true;
            }
            if let Some(note) = input.get("note").and_then(Value::as_str) {
                if !result.is_empty() && !result.ends_with('\n') {
                    result.push('\n');
                }
                result.push_str(&format!("[gate {gate}] {note}"));
                changed = true;
            }
            if !changed {
                return Err("`rewrite` at after_tool needs input.result or input.note".into());
            }
            payload["result"] = json!(result);
            Ok(json!({"result": result}))
        }
        Event::SessionStart | Event::SessionEnd => {
            Err(format!("{} has nothing to rewrite", event.name()))
        }
    }
}

/// The event document a gate sees. The JSON shape is the same for an external
/// gate reading stdin and a built-in reading its context: the six common keys
/// plus the event's own fields.
#[derive(Debug, Clone, PartialEq)]
pub struct GateInput(pub Value);

impl GateInput {
    pub fn new(
        event: Event,
        turn: u32,
        payload: &Value,
        session: &str,
        session_dir: &Path,
        cwd: &Path,
    ) -> GateInput {
        let mut object = serde_json::Map::new();
        object.insert("event".into(), json!(event.name()));
        object.insert("session".into(), json!(session));
        object.insert(
            "session_dir".into(),
            json!(crate::util::path_string(session_dir)),
        );
        object.insert(
            "log".into(),
            json!(crate::util::path_string(&session_dir.join("events.jsonl"))),
        );
        object.insert("cwd".into(), json!(crate::util::path_string(cwd)));
        object.insert("turn".into(), json!(turn));
        if let Some(fields) = payload.as_object() {
            for (key, value) in fields {
                object.insert(key.clone(), value.clone());
            }
        }
        GateInput(Value::Object(object))
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }

    pub fn text(&self) -> Option<&str> {
        self.0.get("text").and_then(Value::as_str)
    }

    pub fn from(&self) -> Option<&str> {
        self.0.get("from").and_then(Value::as_str)
    }

    pub fn event_name(&self) -> Option<&str> {
        self.0.get("event").and_then(Value::as_str)
    }
}

/// What a built-in may use that an external gate cannot: a provider client for
/// its own call, the log so far (read-only) and the session directory.
pub struct GateCtx<'a> {
    pub client: &'a crate::provider::Client,
    pub provider: &'a crate::config::ProviderConfig,
    pub config: &'a crate::config::Config,
    pub provider_name: &'a str,
    /// A line to record in the session log (a note from this built-in).
    pub note: &'a (dyn Fn(&str) + Sync),
    pub events: &'a [crate::log::event::LogEvent],
    pub dir: &'a Path,
    /// Where skills are discovered, from the session's config.
    pub skills_dirs: &'a [std::path::PathBuf],
    /// The machine config dir, for machine-level skills.
    pub config_dir: Option<&'a Path>,
    /// The session's cwd (also on `GateInput`, but typed here).
    pub cwd: &'a Path,
}

/// One built-in gate's answer. The same three fields the external runner parses
/// from JSON, plus a usage event the built-in's own provider call reported.
pub struct GateAnswer {
    pub reason: String,
    pub decision: &'static str,
    pub input: Option<Value>,
    pub context: Option<Context>,
    pub usage: Option<crate::provider::stream::Usage>,
    /// `deny` only: how many times this turn the deny may be enforced. Zero
    /// means the default of one.
    pub max_denies: u32,
}

impl GateAnswer {
    pub fn allow() -> GateAnswer {
        GateAnswer {
            reason: String::new(),
            decision: "allow",
            input: None,
            context: None,
            usage: None,
            max_denies: 0,
        }
    }

    /// A `deny`, enforced once when the answer does not raise the budget.
    pub fn deny(reason: impl Into<String>) -> GateAnswer {
        GateAnswer {
            reason: reason.into(),
            decision: "deny",
            max_denies: 0,
            ..GateAnswer::allow()
        }
    }
}
