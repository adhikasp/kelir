//! Built-in gates: Rust code behind the same interface external gates use.
//! Every built-in gets `GateInput`, a bounded run and the same `gate` event;
//! only the provider client and log access in `GateCtx` are extra.

pub mod hint;
pub mod timebox;

use super::{Event, GateAnswer, GateCtx, GateDef, GateInput};
use serde_json::{Value, json};

pub const NAMES: &[&str] = &["hint", "timebox"];

pub fn is_known(name: &str) -> bool {
    NAMES.contains(&name)
}

pub fn default_event(name: &str) -> &'static str {
    match name {
        "hint" => "prompt_submit",
        "timebox" => "turn_end",
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
        "timebox" => &[
            "builtin",
            "event",
            "duration",
            "steering",
            "only_when_asked",
            "max_denies",
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
    if name == "timebox" {
        if let Some(value) = options.get("duration") {
            timebox_duration(value).map_err(|why| format!("{at}: {why}"))?;
        }
        if let Some(value) = options.get("steering")
            && !value.is_str()
        {
            return Err(format!("{at}: `steering` is a string"));
        }
        if let Some(value) = options.get("only_when_asked")
            && value.as_bool().is_none()
        {
            return Err(format!("{at}: `only_when_asked` is true or false"));
        }
        if let Some(value) = options.get("max_denies")
            && (!value.is_integer() || value.as_integer().is_none_or(|v| v < 1))
        {
            return Err(format!("{at}: `max_denies` is a positive integer"));
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
        "timebox" => "turn_end",
        _ => "",
    }
}

/// The events one built-in supports. Anything else is a config error.
pub fn supports_event(name: &str, event: Event) -> bool {
    match name {
        "hint" => event == Event::PromptSubmit,
        "timebox" => event == Event::TurnEnd,
        _ => false,
    }
}

/// A `timebox` duration at config time, so a bad one is a config error and not a
/// session that silently never nudges.
fn timebox_duration(value: &toml::Value) -> Result<(), String> {
    match value {
        toml::Value::Integer(seconds) if *seconds > 0 => Ok(()),
        toml::Value::Integer(_) => Err("`duration` is a positive number of seconds".into()),
        toml::Value::String(text) => {
            if timebox::budget_in_prompt(text).is_some()
                || timebox::budget_in_prompt(&format!("you have {text}")).is_some()
            {
                Ok(())
            } else {
                Err(format!(
                    "`duration` is not a duration: `{text}` (use `90s`, `15m`, `2h`, `1h30m` or \
                     seconds)"
                ))
            }
        }
        other => Err(format!("`duration` is a string, not {}", other.type_str())),
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
    if answer.max_denies > 0 {
        object["max_denies"] = json!(answer.max_denies);
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
