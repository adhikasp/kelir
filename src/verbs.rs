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
    /// Add or replace one skill or instruction source. It arrives as a
    /// `source` message at the tail, never as a system rewrite.
    Skills {
        kind: crate::sources::SourceKind,
        name: String,
        /// The file (or skill directory) to read at apply time.
        path: Option<String>,
        /// The text, inline: what a test or a script sends.
        text: Option<String>,
    },
    /// A job reported itself or changed state: `kelir bg run` sends this over
    /// the session socket, and the session's own watcher sends the same thing
    /// for a job that settled. It is bookkeeping, so it applies at the first
    /// boundary it can get (and immediately when the session is idle); the wake
    /// message it records is the one that starts a turn.
    Bg(Box<crate::bg::Status>),
    End,
}

impl Verb {
    /// Where the verb is meant to apply; this is what the `verb` event says.
    pub fn scope(&self) -> &'static str {
        match self {
            Verb::Say { now: true, .. } | Verb::Stop { .. } | Verb::End | Verb::Bg(..) => "now",
            Verb::Say { .. } => "queued",
            Verb::Later { .. } => "turn_end",
            Verb::Model { .. }
            | Verb::Tools { .. }
            | Verb::Compact { .. }
            | Verb::Skills { .. } => "next_step",
        }
    }

    /// Does it apply at this boundary?
    pub fn applies_at(&self, boundary: Boundary) -> bool {
        match self {
            // `say`, `stop` and a job's own report take the first boundary they
            // can get.
            Verb::Say { .. } | Verb::Stop { .. } | Verb::End | Verb::Bg(..) => true,
            Verb::Later { .. } => boundary == Boundary::TurnEnd,
            Verb::Model { .. }
            | Verb::Tools { .. }
            | Verb::Compact { .. }
            | Verb::Skills { .. } => boundary == Boundary::BeforeStep,
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
        // A skill or an instruction source added or replaced by a human: it
        // lands as a `source` message at the next boundary.
        "skills" => {
            let name = string_arg(args, "name")
                .or_else(|| args.as_str().map(str::to_string))
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
                .ok_or_else(|| "skills: missing `name`".to_string())?;
            if !valid_source_name(&name) {
                return Err(format!(
                    "skills: `{name}` is not a source name (letters, digits, `.`, `_`, `-`)"
                ));
            }
            let path = string_arg(args, "path").filter(|path| !path.trim().is_empty());
            let text = string_arg(args, "text").filter(|text| !text.trim().is_empty());
            if path.is_none() && text.is_none() {
                return Err("skills: pass `path` (a file or a skill directory) or `text`".into());
            }
            let kind = match args.get("kind").and_then(Value::as_str).unwrap_or("skill") {
                "skill" => crate::sources::SourceKind::Skill,
                "instruction" => crate::sources::SourceKind::Instruction,
                other => {
                    return Err(format!(
                        "skills: unknown kind `{other}` (skill, instruction)"
                    ));
                }
            };
            Ok(Verb::Skills {
                kind,
                name,
                path,
                text,
            })
        }
        "end" => Ok(Verb::End),
        // Applies at the next `before_step`: the summary is a provider call and
        // must not land inside a tool batch.
        "compact" => Ok(Verb::Compact {
            focus: string_arg(args, "focus").filter(|focus| !focus.trim().is_empty()),
        }),
        // A job's status file, as sent by `kelir bg run` or the session's own
        // watcher. The parser is strict: a malformed report is refused and
        // nothing is recorded.
        "bg" => {
            let status: crate::bg::Status = serde_json::from_value(args.clone())
                .map_err(|error| format!("bg: not a job status: {error}"))?;
            if status.id.trim().is_empty() {
                return Err("bg: the job has no id".into());
            }
            Ok(Verb::Bg(Box::new(status)))
        }
        other => Err(format!(
            "unknown verb `{other}` (say, later, stop, model, tools, compact, skills, end)"
        )),
    }
}

/// The flags `kelir ctl` accepts, whatever the verb. Each one becomes the
/// matching key of the request's `args`.
#[derive(Debug, Clone, Default)]
pub struct CliFlags {
    pub now: bool,
    pub call: Option<String>,
    pub reason: Option<String>,
    pub disable: Vec<String>,
    pub enable: Vec<String>,
    pub path: Option<std::path::PathBuf>,
    pub text: Option<String>,
    pub name: Option<String>,
    pub kind: Option<String>,
}

/// Build a request's `args` from the `kelir ctl` command line.
///
/// A single positional that parses as a JSON object is the args verbatim; the
/// flags are then merged over it. Otherwise the positional words are text.
pub fn cli_args(verb: &str, words: &[String], flags: &CliFlags) -> Result<Value, String> {
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
    if flags.now {
        args["now"] = json!(true);
    }
    if let Some(call) = &flags.call {
        args["call"] = json!(call);
    }
    if let Some(reason) = &flags.reason {
        args["reason"] = json!(reason);
    }
    if !flags.disable.is_empty() {
        args["disable"] = json!(flags.disable);
    }
    if !flags.enable.is_empty() {
        args["enable"] = json!(flags.enable);
    }
    if let Some(path) = &flags.path {
        // Forward slashes everywhere: the path crosses a JSON round trip and
        // then a `PathBuf` on this platform.
        args["path"] = json!(crate::util::path_string(path));
    }
    if let Some(text) = &flags.text {
        args["text"] = json!(text);
    }
    if let Some(name) = &flags.name {
        args["name"] = json!(name);
    }
    if let Some(kind) = &flags.kind {
        args["kind"] = json!(kind);
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

/// A source name: what a `sources[]` entry is called, and what a `skills` verb
/// replaces. No separators, so it can never escape a directory.
fn valid_source_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !name.starts_with('.')
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

    /// The job report is a verb so that it arrives through the same channel as
    /// everything else (the socket, or the session's own watcher) and is logged
    /// like everything else.
    #[test]
    fn a_job_status_is_a_verb_that_applies_at_the_first_boundary() {
        let status = crate::bg::Status {
            id: "bg_1".into(),
            state: "exited".into(),
            pid: Some(42),
            shim: Some(41),
            shim_started: None,
            pid_started: None,
            exit_code: Some(0),
            started: "2026-11-19T10:00:00Z".into(),
            finished: Some("2026-11-19T10:00:02Z".into()),
            cmd: "sleep 2".into(),
            cmd_path: None,
            out: "bg/bg_1.out".into(),
            kind: "job".into(),
            promoted: false,
            call: None,
            job: None,
            error: None,
        };
        let args = serde_json::to_value(&status).unwrap();
        let verb = parse("bg", &args).unwrap();
        assert_eq!(verb, Verb::Bg(Box::new(status)));
        assert_eq!(verb.scope(), "now");
        for boundary in [
            Boundary::BeforeStep,
            Boundary::AfterTools,
            Boundary::AfterStep,
            Boundary::TurnEnd,
        ] {
            assert!(verb.applies_at(boundary), "{boundary:?}");
        }
        // A report that is not a status file is refused, not half-applied.
        let error = parse("bg", &json!({"id": "", "state": "exited"})).unwrap_err();
        assert!(error.contains("bg:"), "{error}");
        assert!(parse("bg", &json!({"type": "nonsense"})).is_err());
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
        let args = cli_args(
            "say",
            &words,
            &CliFlags {
                now: true,
                ..CliFlags::default()
            },
        )
        .unwrap();
        assert_eq!(args, json!({"text": "change of plan", "now": true}));

        let args = cli_args(
            "stop",
            &[],
            &CliFlags {
                call: Some("call_2".into()),
                reason: Some("wrong".into()),
                ..CliFlags::default()
            },
        )
        .unwrap();
        assert_eq!(args, json!({"call": "call_2", "reason": "wrong"}));

        let args = cli_args("stop", &["enough".to_string()], &CliFlags::default()).unwrap();
        assert_eq!(args, json!({"reason": "enough"}));

        let args = cli_args(
            "tools",
            &[],
            &CliFlags {
                disable: vec!["bash".into()],
                ..CliFlags::default()
            },
        )
        .unwrap();
        assert_eq!(args, json!({"disable": ["bash"]}));
    }

    #[test]
    fn compact_words_become_the_focus() {
        let words = vec!["the".to_string(), "failing".to_string(), "test".to_string()];
        let args = cli_args("compact", &words, &CliFlags::default()).unwrap();
        assert_eq!(args, json!({"focus": "the failing test"}));
        assert_eq!(
            parse("compact", &args).unwrap(),
            Verb::Compact {
                focus: Some("the failing test".into())
            }
        );
        let args = cli_args("compact", &[], &CliFlags::default()).unwrap();
        assert_eq!(args, json!({}));
        assert_eq!(
            parse("compact", &args).unwrap(),
            Verb::Compact { focus: None }
        );
    }

    #[test]
    fn skills_names_a_source_and_checks_it() {
        let verb = parse("skills", &json!({"name": "sift", "text": "read the tail"})).unwrap();
        assert_eq!(
            verb,
            Verb::Skills {
                kind: crate::sources::SourceKind::Skill,
                name: "sift".into(),
                path: None,
                text: Some("read the tail".into()),
            }
        );
        assert_eq!(verb.scope(), "next_step");
        assert!(verb.applies_at(Boundary::BeforeStep));
        assert!(!verb.applies_at(Boundary::AfterTools));
        // An instruction source, named by a path.
        let verb = parse(
            "skills",
            &json!({"kind": "instruction", "name": "house.md", "path": "a/b.md"}),
        )
        .unwrap();
        assert_eq!(
            verb,
            Verb::Skills {
                kind: crate::sources::SourceKind::Instruction,
                name: "house.md".into(),
                path: Some("a/b.md".into()),
                text: None,
            }
        );
        // Nothing to add, an unknown kind, or a name that could escape a
        // directory are all refused before anything is logged.
        assert!(parse("skills", &json!({"name": "x"})).is_err());
        assert!(parse("skills", &json!({"text": "x"})).is_err());
        assert!(
            parse("skills", &json!({"name": "x", "text": "y", "kind": "hint"}))
                .unwrap_err()
                .contains("unknown kind")
        );
        let backslash = "a\\b";
        for bad in ["../x", "a/b", backslash, ".hidden", " "] {
            assert!(
                parse("skills", &json!({"name": bad, "text": "y"})).is_err(),
                "{bad} was accepted"
            );
        }
    }

    #[test]
    fn the_cli_builds_a_skills_verb_from_its_flags() {
        let flags = CliFlags {
            path: Some(std::path::PathBuf::from("skills/sift")),
            name: Some("sift".into()),
            kind: Some("skill".into()),
            ..CliFlags::default()
        };
        let args = cli_args("skills", &[], &flags).unwrap();
        assert_eq!(args["name"], "sift");
        assert_eq!(args["path"], "skills/sift");
        assert_eq!(args["kind"], "skill");
        assert!(parse("skills", &args).is_ok());
        // Inline text needs no path.
        let flags = CliFlags {
            name: Some("house".into()),
            text: Some("always run the gates".into()),
            ..CliFlags::default()
        };
        let args = cli_args("skills", &[], &flags).unwrap();
        assert!(parse("skills", &args).is_ok());
        // `ctl` validates locally: a skills verb with nothing to add never
        // leaves the machine.
        let flags = CliFlags {
            name: Some("x".into()),
            ..CliFlags::default()
        };
        assert!(cli_args("skills", &[], &flags).is_err());
    }

    #[test]
    fn a_json_positional_is_the_args() {
        let words = vec![r#"{"text":"from json","now":true}"#.to_string()];
        let args = cli_args("say", &words, &CliFlags::default()).unwrap();
        assert_eq!(args, json!({"text": "from json", "now": true}));
        // Words that are not a JSON object stay text.
        let words = vec!["{not json}".to_string()];
        let args = cli_args("say", &words, &CliFlags::default()).unwrap();
        assert_eq!(args, json!({"text": "{not json}"}));
    }

    #[test]
    fn an_empty_say_from_the_cli_is_rejected() {
        assert!(cli_args("say", &[], &CliFlags::default()).is_err());
    }
}
