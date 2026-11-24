//! The TUI's `/` commands: one line in, one verb out.
//! Every action that changes the session is a verb (the same verb `kelir ctl` sends, applied at
//! the same boundary), and [`Action::request`] is the only place that translates one into the
//! other.
//! Local views (`/help`, the output tail, scrolling) carry no session state and are not actions.

use serde_json::{Value, json};

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
    /// `/compact [focus]` — the verb exists from, the compactor from.
    Compact(Option<String>),
    /// `/quit`: the `end` verb.
    Quit,
}

impl Action {
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

/// A local view command: no verb, no session state, no event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    /// `/expand n|last`: append call `n`'s output to the scrollback (the
    /// default is the last call). It cannot unfold a committed line.
    Expand(Option<u64>),
}

/// A parsed line: a session action, or something only the TUI shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Action(Action),
    /// Local information (help, usage): no verb, no session state.
    Note(String),
    /// A local view change (append a call's output): the TUI's own state only.
    View(View),
}

/// `/name args` → `(name, args)`.
fn split_command(line: &str) -> (&str, &str) {
    let body = line.trim_start_matches('/');
    match body.find(char::is_whitespace) {
        Some(index) => (&body[..index], body[index..].trim()),
        None => (body, ""),
    }
}

pub fn parse(line: &str) -> Result<Parsed, String> {
    let trimmed = line.trim_end();
    if trimmed.trim().is_empty() {
        return Err(String::new());
    }
    if !trimmed.starts_with('/') {
        return Ok(Parsed::Action(Action::Submit(trimmed.to_string())));
    }
    let (name, rest) = split_command(trimmed);
    // The local view command returns before the verb table.
    if name == "expand" {
        let word = non_empty(rest)
            .ok_or_else(|| "/expand takes a call number (1, 2, …) or `last`".to_string())?;
        return Ok(match word.as_str() {
            "last" => Parsed::View(View::Expand(None)),
            word => match word.parse::<u64>() {
                Ok(ordinal) if ordinal > 0 => Parsed::View(View::Expand(Some(ordinal))),
                _ => {
                    return Err(format!(
                        "/expand takes a call number (1, 2, …) or `last`, not `{word}`"
                    ));
                }
            },
        });
    }
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
    (
        "/expand [n|last]",
        "append that call's output here (a committed line cannot unfold)",
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
    fn action(line: &str) -> Action {
        match parse(line) {
            Ok(Parsed::Action(action)) => action,
            other => panic!("expected an action from {line:?}, got {other:?}"),
        }
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
}
