//! Verbs: what a human, a script or a gate can do to a running session.
//!
//! A verb arrives as `{"verb": "say", "args": {…}}` (the socket, `kelir ctl`).
//! The session logs it as a `verb` event and then applies it at **one named
//! boundary**, never in the middle of a tool call. [`Scheduler`] is the pure
//! state machine that answers "which verbs are due now?"; the loop only has to
//! run them.

use serde_json::{Value, json};

use crate::tools;

/// The points where a verb may be applied. Never between a tool call and its
/// result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// Just before a provider request.
    BeforeStep,
    /// After every call in a batch settled.
    AfterTools,
    /// After a response that carried no tool calls.
    AfterStep,
    /// The turn is over (or the process is idle).
    TurnEnd,
}

impl Boundary {
    /// The name recorded on the `verb` event and returned to the client.
    pub fn name(self) -> &'static str {
        match self {
            Boundary::BeforeStep => "next_step",
            Boundary::AfterTools => "after_tools",
            Boundary::AfterStep => "after_step",
            Boundary::TurnEnd => "turn_end",
        }
    }
}

/// A validated verb.
#[derive(Debug, Clone, PartialEq)]
pub enum Verb {
    Say {
        text: String,
        now: bool,
    },
    Later {
        text: String,
    },
    Stop {
        call: Option<String>,
        reason: Option<String>,
    },
    /// `provider/model`, or just `model` on the current provider.
    Model {
        target: String,
    },
    Tools {
        disable: Vec<String>,
        enable: Vec<String>,
    },
    /// `compact [focus]`: summarise the earlier conversation at the next
    /// `before_step` boundary.
    Compact {
        focus: Option<String>,
    },
    End,
}

impl Verb {
    /// Where the verb is meant to apply; this is what the `verb` event says.
    pub fn scope(&self) -> &'static str {
        match self {
            Verb::Say { now: true, .. } | Verb::Stop { .. } | Verb::End => "now",
            Verb::Say { .. } => "queued",
            Verb::Later { .. } => "turn_end",
            Verb::Model { .. } | Verb::Tools { .. } | Verb::Compact { .. } => "next_step",
        }
    }

    /// Does it apply at this boundary?
    pub fn applies_at(&self, boundary: Boundary) -> bool {
        match self {
            // `say` and `stop` take the first boundary they can get.
            Verb::Say { .. } | Verb::Stop { .. } | Verb::End => true,
            Verb::Later { .. } => boundary == Boundary::TurnEnd,
            Verb::Model { .. } | Verb::Tools { .. } | Verb::Compact { .. } => {
                boundary == Boundary::BeforeStep
            }
        }
    }
}

/// A verb waiting for its boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    /// The `verb` event's seq: the log order, and the order they apply in.
    pub seq: u64,
    pub verb: Verb,
    /// Who sent it (`ctl`, `tui`, `gate`, …); a `say` carries it to the `user`
    /// event.
    pub from: String,
}

/// Verbs not yet due, in arrival order.
#[derive(Debug, Default)]
pub struct Scheduler {
    pending: Vec<Pending>,
}

impl Scheduler {
    pub fn push(&mut self, pending: Pending) {
        self.pending.push(pending);
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Take the verbs that apply at `boundary`, keeping the rest in order.
    pub fn due(&mut self, boundary: Boundary) -> Vec<Pending> {
        let mut due = Vec::new();
        let mut rest = Vec::with_capacity(self.pending.len());
        for pending in self.pending.drain(..) {
            if pending.verb.applies_at(boundary) {
                due.push(pending);
            } else {
                rest.push(pending);
            }
        }
        self.pending = rest;
        due
    }

    /// The verbs still pending (for a reply or a test).
    pub fn peek(&self) -> &[Pending] {
        &self.pending
    }
}

/// Parse and validate a request's `verb` and `args`.
pub fn parse(name: &str, args: &Value) -> Result<Verb, String> {
    match name {
        "say" => {
            let text = text_arg(args, "say")?;
            Ok(Verb::Say {
                text,
                now: flag(args, "now"),
            })
        }
        "later" => Ok(Verb::Later {
            text: text_arg(args, "later")?,
        }),
        "stop" => Ok(Verb::Stop {
            call: string_arg(args, "call").filter(|call| !call.trim().is_empty()),
            reason: string_arg(args, "reason").filter(|reason| !reason.trim().is_empty()),
        }),
        "model" => {
            let target = string_arg(args, "model")
                .or_else(|| string_arg(args, "target"))
                .or_else(|| args.as_str().map(str::to_string))
                .ok_or_else(|| "model: missing `model`".to_string())?;
            if target.trim().is_empty() {
                return Err("model: `model` is empty".into());
            }
            if target.split('/').any(|part| part.trim() != part)
                || target.contains(char::is_whitespace)
            {
                return Err(format!("model: `{target}` has whitespace"));
            }
            Ok(Verb::Model { target })
        }
        "tools" => {
            let disable = name_list(args, "disable")?;
            let enable = name_list(args, "enable")?;
            if disable.is_empty() && enable.is_empty() {
                return Err("tools: pass `disable` or `enable` with a tool name".into());
            }
            let known = tools::names();
            for name in disable.iter().chain(enable.iter()) {
                if !known.contains(name) {
                    return Err(format!(
                        "tools: unknown tool `{name}` (known: {})",
                        known.join(", ")
                    ));
                }
            }
            Ok(Verb::Tools { disable, enable })
        }
        "end" => Ok(Verb::End),
        // Applies at the next `before_step`: the summary is a provider call and
        // must not land inside a tool batch.
        "compact" => Ok(Verb::Compact {
            focus: string_arg(args, "focus").filter(|focus| !focus.trim().is_empty()),
        }),
        other => Err(format!(
            "unknown verb `{other}` (say, later, stop, model, tools, compact, end)"
        )),
    }
}

/// Build a request's `args` from the `kelir ctl` command line.
///
/// A single positional that parses as a JSON object is the args verbatim; the
/// flags are then merged over it. Otherwise the positional words are text.
pub fn cli_args(
    verb: &str,
    words: &[String],
    now: bool,
    call: Option<&str>,
    reason: Option<&str>,
    disable: &[String],
    enable: &[String],
) -> Result<Value, String> {
    let mut args = match json_object(words) {
        Some(object) => object,
        None => {
            let text = words.join(" ");
            match verb {
                "say" | "later" if text.trim().is_empty() => json!({}),
                "say" | "later" => json!({"text": text}),
                "stop" => {
                    let mut args = json!({});
                    if !text.trim().is_empty() {
                        args["reason"] = json!(text);
                    }
                    args
                }
                "model" if !text.trim().is_empty() => json!({"model": text}),
                "compact" if !text.trim().is_empty() => json!({"focus": text}),
                _ => json!({}),
            }
        }
    };
    if now {
        args["now"] = json!(true);
    }
    if let Some(call) = call {
        args["call"] = json!(call);
    }
    if let Some(reason) = reason {
        args["reason"] = json!(reason);
    }
    if !disable.is_empty() {
        args["disable"] = json!(disable);
    }
    if !enable.is_empty() {
        args["enable"] = json!(enable);
    }
    // Validate before the request leaves the machine: `ctl` should catch a bad
    // invocation itself, not the session.
    parse(verb, &args)?;
    Ok(args)
}

fn json_object(words: &[String]) -> Option<Value> {
    if words.len() != 1 {
        return None;
    }
    let value: Value = serde_json::from_str(words[0].trim()).ok()?;
    value.is_object().then_some(value)
}

fn text_arg(args: &Value, verb: &str) -> Result<String, String> {
    if let Some(text) = args.as_str() {
        if text.trim().is_empty() {
            return Err(format!("{verb}: text is empty"));
        }
        return Ok(text.to_string());
    }
    match args.get("text").and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => Ok(text.to_string()),
        _ => Err(format!("{verb}: missing non-empty `text`")),
    }
}

fn string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

fn flag(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn name_list(args: &Value, key: &str) -> Result<Vec<String>, String> {
    let Some(value) = args.get(key) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    match value {
        Value::String(name) => out.push(name.clone()),
        Value::Array(items) => {
            for item in items {
                match item.as_str() {
                    Some(name) => out.push(name.to_string()),
                    None => {
                        return Err(format!("tools: `{key}` must be a string or a list of them"));
                    }
                }
            }
        }
        Value::Null => {}
        _ => return Err(format!("tools: `{key}` must be a string or a list of them")),
    }
    for name in &mut out {
        *name = name.trim().to_string();
        if name.is_empty() {
            return Err(format!("tools: `{key}` has an empty name"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(seq: u64, verb: Verb) -> Pending {
        Pending {
            seq,
            verb,
            from: "ctl".into(),
        }
    }

    #[test]
    fn say_needs_text_and_reads_now() {
        let verb = parse("say", &json!({"text": "hello"})).unwrap();
        assert_eq!(
            verb,
            Verb::Say {
                text: "hello".into(),
                now: false
            }
        );
        assert_eq!(verb.scope(), "queued");
        let verb = parse("say", &json!({"text": "hello", "now": true})).unwrap();
        assert_eq!(verb.scope(), "now");
        assert_eq!(
            parse("say", &json!({})).unwrap_err(),
            "say: missing non-empty `text`"
        );
        assert!(parse("say", &json!({"text": "  "})).is_err());
        // A bare string is accepted as the text.
        assert_eq!(
            parse("say", &json!("hi")).unwrap(),
            Verb::Say {
                text: "hi".into(),
                now: false
            }
        );
    }

    #[test]
    fn stop_takes_an_optional_call_and_reason() {
        assert_eq!(
            parse("stop", &json!({})).unwrap(),
            Verb::Stop {
                call: None,
                reason: None
            }
        );
        assert_eq!(
            parse("stop", &json!({"call": "call_1", "reason": "wrong dir"})).unwrap(),
            Verb::Stop {
                call: Some("call_1".into()),
                reason: Some("wrong dir".into())
            }
        );
        // Empty strings are the same as absent.
        assert_eq!(
            parse("stop", &json!({"call": "", "reason": " "})).unwrap(),
            Verb::Stop {
                call: None,
                reason: None
            }
        );
    }

    #[test]
    fn model_names_are_checked() {
        assert_eq!(
            parse("model", &json!({"model": "openrouter/deepseek/x"})).unwrap(),
            Verb::Model {
                target: "openrouter/deepseek/x".into()
            }
        );
        assert!(parse("model", &json!({})).is_err());
        assert!(parse("model", &json!({"model": "two words"})).is_err());
    }

    #[test]
    fn tools_accepts_one_name_or_a_list_and_checks_it() {
        assert_eq!(
            parse("tools", &json!({"disable": "bash"})).unwrap(),
            Verb::Tools {
                disable: vec!["bash".into()],
                enable: vec![]
            }
        );
        assert_eq!(
            parse("tools", &json!({"enable": ["bash", "edit"]})).unwrap(),
            Verb::Tools {
                disable: vec![],
                enable: vec!["bash".into(), "edit".into()]
            }
        );
        assert!(parse("tools", &json!({})).is_err());
        let error = parse("tools", &json!({"disable": "spawn"})).unwrap_err();
        assert!(error.contains("unknown tool"), "{error}");
    }

    #[test]
    fn unknown_verbs_are_refused_with_the_list() {
        let error = parse("frobnicate", &json!({})).unwrap_err();
        assert!(error.contains("unknown verb"), "{error}");
        assert!(error.contains("say"), "{error}");
    }

    #[test]
    fn verbs_apply_at_their_boundaries() {
        let say = Verb::Say {
            text: "x".into(),
            now: false,
        };
        for boundary in [
            Boundary::BeforeStep,
            Boundary::AfterTools,
            Boundary::AfterStep,
            Boundary::TurnEnd,
        ] {
            assert!(say.applies_at(boundary), "{boundary:?}");
        }
        let late = Verb::Later { text: "x".into() };
        assert!(!late.applies_at(Boundary::BeforeStep));
        assert!(late.applies_at(Boundary::TurnEnd));
        let model = Verb::Model { target: "m".into() };
        assert!(model.applies_at(Boundary::BeforeStep));
        assert!(!model.applies_at(Boundary::AfterTools));
        assert!(!model.applies_at(Boundary::TurnEnd));
    }

    #[test]
    fn the_scheduler_keeps_order_and_defers_what_is_not_due() {
        let mut scheduler = Scheduler::default();
        scheduler.push(pending(5, Verb::Later { text: "b".into() }));
        scheduler.push(pending(
            6,
            Verb::Say {
                text: "a".into(),
                now: false,
            },
        ));
        scheduler.push(pending(7, Verb::Model { target: "m".into() }));
        let due = scheduler.due(Boundary::AfterTools);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].seq, 6);
        assert_eq!(scheduler.len(), 2);
        let due = scheduler.due(Boundary::BeforeStep);
        assert_eq!(due[0].seq, 7);
        assert!(scheduler.len() == 1);
        let due = scheduler.due(Boundary::TurnEnd);
        assert_eq!(due[0].seq, 5, "later messages keep their arrival order");
        assert!(scheduler.is_empty());
    }

    #[test]
    fn later_messages_queue_in_arrival_order() {
        let mut scheduler = Scheduler::default();
        for (seq, text) in [(1u64, "first"), (2, "second"), (3, "third")] {
            scheduler.push(pending(seq, Verb::Later { text: text.into() }));
        }
        let texts: Vec<String> = scheduler
            .due(Boundary::TurnEnd)
            .into_iter()
            .map(|pending| match pending.verb {
                Verb::Later { text } => text,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(texts, vec!["first", "second", "third"]);
    }

    #[test]
    fn cli_words_become_text_and_flags_merge() {
        let words = vec!["change".to_string(), "of".to_string(), "plan".to_string()];
        let args = cli_args("say", &words, true, None, None, &[], &[]).unwrap();
        assert_eq!(args, json!({"text": "change of plan", "now": true}));

        let args = cli_args("stop", &[], false, Some("call_2"), Some("wrong"), &[], &[]).unwrap();
        assert_eq!(args, json!({"call": "call_2", "reason": "wrong"}));

        let args = cli_args("stop", &["enough".to_string()], false, None, None, &[], &[]).unwrap();
        assert_eq!(args, json!({"reason": "enough"}));

        let args = cli_args("tools", &[], false, None, None, &["bash".to_string()], &[]).unwrap();
        assert_eq!(args, json!({"disable": ["bash"]}));
    }

    #[test]
    fn compact_words_become_the_focus() {
        let words = vec!["the".to_string(), "failing".to_string(), "test".to_string()];
        let args = cli_args("compact", &words, false, None, None, &[], &[]).unwrap();
        assert_eq!(args, json!({"focus": "the failing test"}));
        assert_eq!(
            parse("compact", &args).unwrap(),
            Verb::Compact {
                focus: Some("the failing test".into())
            }
        );
        let args = cli_args("compact", &[], false, None, None, &[], &[]).unwrap();
        assert_eq!(args, json!({}));
        assert_eq!(
            parse("compact", &args).unwrap(),
            Verb::Compact { focus: None }
        );
    }

    #[test]
    fn a_json_positional_is_the_args() {
        let words = vec![r#"{"text":"from json","now":true}"#.to_string()];
        let args = cli_args("say", &words, false, None, None, &[], &[]).unwrap();
        assert_eq!(args, json!({"text": "from json", "now": true}));
        // Words that are not a JSON object stay text.
        let words = vec!["{not json}".to_string()];
        let args = cli_args("say", &words, false, None, None, &[], &[]).unwrap();
        assert_eq!(args, json!({"text": "{not json}"}));
    }

    #[test]
    fn an_empty_say_from_the_cli_is_rejected() {
        assert!(cli_args("say", &[], false, None, None, &[], &[]).is_err());
    }
}
