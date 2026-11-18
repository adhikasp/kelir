//! The TUI's `/` commands: one line in, one verb out.
//!
//! Every action that changes the session is a verb (the same verb `kelir ctl`
//! sends, applied at the same boundary), and [`Action::request`] is the only
//! place that translates one into the other. Local views (`/help`, the output
//! tail, scrolling) carry no session state and are not actions.

use serde_json::{Value, json};

/// What the human asked for. One variant per session-affecting TUI action.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Plain text: a `say` at the next boundary.
    Submit(String),
    /// `/stop [reason]`: stop the turn now.
    Stop(Option<String>),
    /// `/say-now <text>`: stop the running calls, then deliver the message.
    SayNow(String),
    /// `/later <text>`: queue a message for the end of the turn.
    Later(String),
    /// `/model <provider/model>`.
    Model(String),
    /// `/tools off|on <name>…`.
    Tools {
        disable: Vec<String>,
        enable: Vec<String>,
    },
    /// `/compact [focus]` — the verb exists from M2, the compactor from M3.
    Compact(Option<String>),
    /// `/quit`: the `end` verb.
    Quit,
}

impl Action {
    /// The verb this action sends.
    pub fn verb(&self) -> &'static str {
        match self {
            Action::Submit(_) | Action::SayNow(_) => "say",
            Action::Stop(_) => "stop",
            Action::Later(_) => "later",
            Action::Model(_) => "model",
            Action::Tools { .. } => "tools",
            Action::Compact(_) => "compact",
            Action::Quit => "end",
        }
    }

    /// The verb's args, exactly as `kelir ctl` would build them.
    pub fn args(&self) -> Value {
        match self {
            Action::Submit(text) => json!({"text": text}),
            Action::SayNow(text) => json!({"text": text, "now": true}),
            Action::Stop(None) => json!({}),
            Action::Stop(Some(reason)) => json!({"reason": reason}),
            Action::Later(text) => json!({"text": text}),
            Action::Model(target) => json!({"model": target}),
            Action::Tools { disable, enable } => {
                let mut args = json!({});
                if !disable.is_empty() {
                    args["disable"] = json!(disable);
                }
                if !enable.is_empty() {
                    args["enable"] = json!(enable);
                }
                args
            }
            Action::Compact(None) => json!({}),
            Action::Compact(Some(focus)) => json!({"focus": focus}),
            Action::Quit => json!({}),
        }
    }

    /// What the TUI shows in the scrollback before the session answers.
    pub fn line(&self) -> String {
        match self {
            Action::Submit(text) => format!("› {text}"),
            Action::SayNow(text) => format!("› {text}  (now)"),
            Action::Stop(None) => "» stop".to_string(),
            Action::Stop(Some(reason)) => format!("» stop: {reason}"),
            Action::Later(text) => format!("» later: {text}"),
            Action::Model(target) => format!("» model {target}"),
            Action::Tools { disable, enable } => {
                let mut parts = Vec::new();
                if !disable.is_empty() {
                    parts.push(format!("off {}", disable.join(" ")));
                }
                if !enable.is_empty() {
                    parts.push(format!("on {}", enable.join(" ")));
                }
                format!("» tools {}", parts.join(", "))
            }
            Action::Compact(None) => "» compact".to_string(),
            Action::Compact(Some(focus)) => format!("» compact {focus}"),
            Action::Quit => "» quit".to_string(),
        }
    }
}

/// A parsed line: a session action, or something only the TUI shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Action(Action),
    /// Local information (help, usage): no verb, no session state.
    Note(String),
}

/// `/name args` → `(name, args)`.
fn split_command(line: &str) -> (&str, &str) {
    let body = line.trim_start_matches('/');
    match body.find(char::is_whitespace) {
        Some(index) => (&body[..index], body[index..].trim()),
        None => (body, ""),
    }
}

/// Parse one submitted editor line.
///
/// A line that does not start with `/` is a message; anything else is a
/// command. An empty line is an error with no message, which the TUI treats as
/// "nothing to do".
pub fn parse(line: &str) -> Result<Parsed, String> {
    let trimmed = line.trim_end();
    if trimmed.trim().is_empty() {
        return Err(String::new());
    }
    if !trimmed.starts_with('/') {
        return Ok(Parsed::Action(Action::Submit(trimmed.to_string())));
    }
    let (name, rest) = split_command(trimmed);
    let action = match name {
        "stop" => Action::Stop(non_empty(rest)),
        "say-now" | "say_now" | "now" => Action::SayNow(text_arg("say-now", rest)?),
        "later" => Action::Later(text_arg("later", rest)?),
        "model" => Action::Model(text_arg("model", rest)?),
        "tools" => {
            let (mode, rest) = split_command(rest);
            let names: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
            match (mode, names.is_empty()) {
                ("off", false) => Action::Tools {
                    disable: names,
                    enable: Vec::new(),
                },
                ("on", false) => Action::Tools {
                    disable: Vec::new(),
                    enable: names,
                },
                ("off" | "on", true) => {
                    return Err(format!("/tools {mode}: name a tool (bash, edit)"));
                }
                _ => return Ok(Parsed::Note(usage("tools"))),
            }
        }
        "compact" => Action::Compact(non_empty(rest)),
        "quit" | "exit" => Action::Quit,
        "help" | "?" => return Ok(Parsed::Note(help())),
        "" => return Err("no command after `/` (try `/help`)".to_string()),
        other => {
            return Err(format!("unknown command `/{other}` (try `/help`)"));
        }
    };
    Ok(Parsed::Action(action))
}

fn text_arg(command: &str, rest: &str) -> Result<String, String> {
    match non_empty(rest) {
        Some(text) => Ok(text),
        None => Err(format!("/{command} needs something after it")),
    }
}

fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The `/` commands, for `/help`.
pub const COMMANDS: &[(&str, &str)] = &[
    ("/stop [reason]", "stop the turn now (Ctrl-C does the same)"),
    (
        "/say-now <text>",
        "stop the running calls, then send the message",
    ),
    ("/later <text>", "queue a message for the end of the turn"),
    (
        "/model <p/m>",
        "swap provider/model for the rest of the session",
    ),
    (
        "/tools off|on <name>",
        "disable or enable a tool (bash, edit)",
    ),
    (
        "/compact [focus]",
        "summarise the earlier context, keeping the task and your messages",
    ),
    ("/quit", "end the session (a second Ctrl-C does the same)"),
    ("/help", "this list"),
];

pub fn usage(command: &str) -> String {
    COMMANDS
        .iter()
        .find(|(name, _)| name.trim_start_matches('/').starts_with(command))
        .map(|(name, what)| format!("usage: {name} — {what}"))
        .unwrap_or_else(help)
}

pub fn help() -> String {
    let lines: Vec<String> = COMMANDS
        .iter()
        .map(|(name, what)| format!("{name:<22} {what}"))
        .collect();
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verbs;

    fn action(line: &str) -> Action {
        match parse(line) {
            Ok(Parsed::Action(action)) => action,
            other => panic!("expected an action from {line:?}, got {other:?}"),
        }
    }

    /// Every action, with a sample of the data it needs. `Action::verb` and
    /// `Action::args` match on every variant, so a new action cannot be added
    /// without being handled — and the test below then checks its verb.
    fn every_action() -> Vec<Action> {
        vec![
            Action::Submit("hello".into()),
            Action::Stop(None),
            Action::Stop(Some("wrong turn".into())),
            Action::SayNow("change of plan".into()),
            Action::Later("after this".into()),
            Action::Model("openrouter/deepseek/deepseek-chat".into()),
            Action::Tools {
                disable: vec!["bash".into()],
                enable: Vec::new(),
            },
            Action::Tools {
                disable: Vec::new(),
                enable: vec!["bash".into(), "edit".into()],
            },
            Action::Compact(None),
            Action::Compact(Some("the failing test".into())),
            Action::Quit,
        ]
    }

    #[test]
    fn every_tui_action_is_a_verb_the_session_accepts() {
        for action in every_action() {
            let verb = action.verb();
            let args = action.args();
            let parsed = verbs::parse(verb, &args);
            assert!(
                parsed.is_ok(),
                "action {action:?} sends verb `{verb}` with {args}, which the session refuses: {:?}",
                parsed.err()
            );
            assert!(
                !action.line().is_empty(),
                "action {action:?} needs a scrollback line"
            );
        }
    }

    #[test]
    fn each_action_sends_the_variant_its_name_promises() {
        assert_eq!(
            verbs::parse("say", &Action::Submit("hi".into()).args()).unwrap(),
            verbs::Verb::Say {
                text: "hi".into(),
                now: false
            }
        );
        assert_eq!(
            verbs::parse("say", &Action::SayNow("hi".into()).args()).unwrap(),
            verbs::Verb::Say {
                text: "hi".into(),
                now: true
            }
        );
        assert_eq!(
            action("/stop").args(),
            json!({}),
            "a bare /stop carries no reason"
        );
        assert_eq!(
            action("/model deepseek/deepseek-chat").args(),
            json!({"model": "deepseek/deepseek-chat"})
        );
        assert_eq!(
            action("/tools off bash edit").args(),
            json!({"disable": ["bash", "edit"]})
        );
        assert_eq!(action("/tools on bash").args(), json!({"enable": ["bash"]}));
        assert_eq!(action("/quit").verb(), "end");
        assert_eq!(action("/compact").args(), json!({}));
        assert_eq!(
            action("/compact the failing test").args(),
            json!({"focus": "the failing test"})
        );
    }

    #[test]
    fn plain_lines_are_messages() {
        assert_eq!(action("fix the bug"), Action::Submit("fix the bug".into()));
        // Multi-line input keeps its newlines.
        assert_eq!(
            action("fix the bug\nand the test"),
            Action::Submit("fix the bug\nand the test".into())
        );
        // Trailing whitespace is dropped, leading text is not.
        assert_eq!(action("  x  "), Action::Submit("  x".into()));
    }

    #[test]
    fn the_slash_commands_parse() {
        assert_eq!(action("/stop"), Action::Stop(None));
        assert_eq!(
            action("/stop wrong dir"),
            Action::Stop(Some("wrong dir".into()))
        );
        assert_eq!(action("/say-now hello"), Action::SayNow("hello".into()));
        assert_eq!(action("/later hello"), Action::Later("hello".into()));
        assert_eq!(action("/model a/b"), Action::Model("a/b".into()));
        assert_eq!(
            action("/tools off bash"),
            Action::Tools {
                disable: vec!["bash".into()],
                enable: Vec::new()
            }
        );
        assert_eq!(action("/compact"), Action::Compact(None));
        assert_eq!(action("/quit"), Action::Quit);
        assert_eq!(action("/exit"), Action::Quit);
    }

    #[test]
    fn help_is_a_note_not_a_verb() {
        match parse("/help").unwrap() {
            Parsed::Note(text) => assert!(text.contains("/compact"), "{text}"),
            other => panic!("{other:?}"),
        }
        match parse("/tools").unwrap() {
            Parsed::Note(text) => assert!(text.starts_with("usage: /tools"), "{text}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_bad_command_says_what_to_do_instead() {
        assert!(
            parse("/frobnicate")
                .unwrap_err()
                .contains("unknown command")
        );
        assert!(parse("/say-now").unwrap_err().contains("needs something"));
        assert!(parse("/model ").unwrap_err().contains("needs something"));
        assert!(parse("/tools off").unwrap_err().contains("name a tool"));
        assert!(parse("/").unwrap_err().contains("/help"));
        assert_eq!(parse("").unwrap_err(), "", "an empty line is a no-op");
        assert_eq!(parse("   \n").unwrap_err(), "");
    }

    #[test]
    fn every_help_entry_starts_with_a_slash() {
        for (name, what) in COMMANDS {
            assert!(name.starts_with('/'), "{name}");
            assert!(!what.is_empty(), "{name}");
        }
    }
}
