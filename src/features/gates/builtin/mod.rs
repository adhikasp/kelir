//! Built-in gates: Rust code behind the same interface external gates use.
//! Every built-in gets `GateInput`, a bounded run and the same `gate` event;
//! only the provider client and log access in `GateCtx` are extra.

pub mod hint;

use super::{Event, GateAnswer, GateCtx, GateDef, GateInput};
use serde_json::{Value, json};

pub const NAMES: &[&str] = &["hint"];

pub fn is_known(name: &str) -> bool {
    NAMES.contains(&name)
}

pub fn default_event(name: &str) -> &'static str {
    match name {
        "hint" => "prompt_submit",
        _ => "session_start",
    }
}

/// Built-in options are keys of the gate's own table. Anything else is an
/// ordinary unknown-key config error.
pub fn validate_options(name: &str, options: &toml::Table, at: &str) -> Result<(), String> {
    let allowed: &[&str] = match name {
        "hint" => &[
            "builtin",
            "event",
            "model",
            "provider",
            "endpoint",
            "timeout",
            "timeout_ms",
            "max_names",
            "tools",
            "tool",
            "name",
        ],
        _ => &[],
    };
    for key in options.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!(
                "{at}: unknown key `{key}` ({})",
                allowed.join(", ")
            ));
        }
    }
    if name == "hint" {
        let model = options
            .get("model")
            .and_then(toml::Value::as_str)
            .filter(|m| !m.trim().is_empty())
            .ok_or_else(|| format!("{at}: a built-in `hint` needs a `model` string"))?;
        let _ = model;
        for key in ["timeout", "timeout_ms", "max_names"] {
            if let Some(value) = options.get(key)
                && (!value.is_integer() || value.as_integer().is_none_or(|v| v <= 0))
            {
                return Err(format!("{at}: `{key}` must be a positive integer"));
            }
        }
        if let Some(value) = options.get("provider")
            && !value.is_str()
        {
            return Err(format!("{at}: `provider` is a string"));
        }
        if let Some(value) = options.get("endpoint")
            && !value.is_str()
        {
            return Err(format!("{at}: `endpoint` is a string"));
        }
    }
    Ok(())
}

pub fn supported_events(name: &str) -> &'static str {
    match name {
        "hint" => "prompt_submit",
        _ => "",
    }
}

/// The events one built-in supports. Anything else is a config error.
pub fn supports_event(name: &str, event: Event) -> bool {
    match name {
        "hint" => event == Event::PromptSubmit,
        _ => false,
    }
}

/// Convert a built-in's answer to the same JSON an external gate prints, so
/// both go through one parse/record path.
pub fn to_stdout(answer: &GateAnswer) -> String {
    let mut object = json!({
        "decision": answer.decision,
        "reason": answer.reason,
    });
    if let Some(input) = &answer.input {
        object["input"] = input.clone();
    }
    if let Some(context) = &answer.context {
        object["context"] = Value::String(context.text.clone());
        object["kind"] = Value::String(context.kind.clone());
    }
    object["usage"] = answer
        .usage
        .as_ref()
        .map(|usage| {
            json!({
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "cached_tokens": usage.cached_tokens,
                "cache_write_tokens": usage.cache_write_tokens,
            })
        })
        .unwrap_or(Value::Null);
    object.to_string()
}
