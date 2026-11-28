use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

/// A gate that has not answered after this long is killed and allowed.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// More stdout than this is garbage (and the rest is discarded).
pub const OUTPUT_MAX: usize = 1024 * 1024;

/// The longest `reason` kept in a `gate` event or shown to the model.
pub const REASON_MAX: usize = 2000;

/// How much of a crashed gate's stderr goes into the `error` field.
pub(crate) const STDERR_TAIL: usize = 300;

/// After the gate ended, how long its output pipes get to close.
pub(crate) const DRAIN: Duration = Duration::from_secs(2);

/// The gate timeout: [`TIMEOUT`], unless a test shortened it.
pub fn timeout() -> Duration {
    crate::envs::var("KELIR_TEST_GATE_TIMEOUT_MS")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(TIMEOUT)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    SessionStart,
    PromptSubmit,
    BeforeTool,
    AfterTool,
    TurnEnd,
    SessionEnd,
}

impl Event {
    pub const ALL: [Event; 6] = [
        Event::SessionStart,
        Event::PromptSubmit,
        Event::BeforeTool,
        Event::AfterTool,
        Event::TurnEnd,
        Event::SessionEnd,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Event::SessionStart => "session_start",
            Event::PromptSubmit => "prompt_submit",
            Event::BeforeTool => "before_tool",
            Event::AfterTool => "after_tool",
            Event::TurnEnd => "turn_end",
            Event::SessionEnd => "session_end",
        }
    }

    pub fn parse(name: &str) -> Option<Event> {
        let name = name.trim().replace('-', "_").to_ascii_lowercase();
        Event::ALL.into_iter().find(|event| event.name() == name)
    }

    /// Whether `deny` does anything at this event (the SPEC table).
    pub fn can_deny(self) -> bool {
        matches!(
            self,
            Event::PromptSubmit | Event::BeforeTool | Event::TurnEnd
        )
    }

    /// Whether `rewrite` does anything at this event.
    pub fn can_rewrite(self) -> bool {
        !matches!(self, Event::SessionStart | Event::SessionEnd)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateDef {
    pub name: String,
    pub event: Event,
    /// Empty for a built-in gate, which runs in this process instead of a shell.
    pub command: String,
    /// `Some(name)` selects a Rust built-in (`hint` is the first one).
    pub builtin: Option<String>,
    /// The built-in's own options, from its gate table.
    pub options: toml::Table,
    /// `before_tool`/`after_tool` only: the tool names this gate sees (empty is every tool).
    /// `mcp:<server>` covers every tool of a server and a trailing `*` is a prefix match.
    pub tools: Vec<String>,
    /// Extra environment, from the gate's own `env` table.
    pub env: BTreeMap<String, String>,
    /// Where the declaration came from: `config` or `skill <name>`.
    pub origin: String,
}

impl GateDef {
    /// Whether this gate runs for `tool` (always true for events without one).
    pub fn matches(&self, tool: Option<&str>) -> bool {
        let Some(tool) = tool else { return true };
        self.tools.is_empty() || self.tools.iter().any(|pattern| tool_matches(pattern, tool))
    }
}

fn tool_matches(pattern: &str, tool: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        return tool.starts_with(prefix);
    }
    if pattern == tool {
        return true;
    }
    // `mcp:server` names every tool of the server.
    pattern.starts_with("mcp:")
        && !pattern.contains('.')
        && tool
            .strip_prefix(pattern)
            .is_some_and(|rest| rest.starts_with('.'))
}

// ------------------------------------------------------------------- config

/// Read the `[gates]` table: each key an event name, each value a command string, a table, or
/// a list of those. A table with `builtin = "<name>"` declares a built-in gate.
pub fn from_toml(value: &toml::Value) -> Result<Vec<GateDef>, String> {
    let table = value
        .as_table()
        .ok_or_else(|| "`gates` must be a table of event names".to_string())?;
    let mut defs: Vec<GateDef> = Vec::new();
    for (key, entry) in table {
        if let Some(name) = Event::parse(key) {
            let items: Vec<&toml::Value> = match entry {
                toml::Value::Array(items) => items.iter().collect(),
                other => vec![other],
            };
            for item in items {
                let def = one_from_toml(name, item)?;
                push_unique(&mut defs, def);
            }
        } else if let toml::Value::Table(options) = entry
            && options.contains_key("builtin")
        {
            // `[gates.<name>]` declares one named gate; its event and options
            // live in the same table.
            let def = builtin_from_toml(key, options)?;
            push_unique(&mut defs, def);
        } else {
            return Err(format!(
                "[gates]: `{key}` is not an event (the six are {})",
                Event::ALL.map(Event::name).join(", ")
            ));
        }
    }
    Ok(defs)
}

fn builtin_from_toml(name: &str, options: &toml::Table) -> Result<GateDef, String> {
    let at = format!("[gates].{name}");
    if !valid_name(name) {
        return Err(format!("{at}: `name` is letters, digits, `-`, `_`, `.`"));
    }
    let builtin = options
        .get("builtin")
        .and_then(toml::Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| format!("{at}: `builtin` needs a name"))?;
    if !builtin::is_known(builtin) {
        return Err(format!(
            "{at}: unknown builtin `{builtin}` (the known ones are {})",
            builtin::NAMES.join(", ")
        ));
    }
    let event_name = options
        .get("event")
        .and_then(toml::Value::as_str)
        .unwrap_or(builtin::default_event(builtin));
    let event = Event::parse(event_name).ok_or_else(|| {
        format!(
            "{at}: `{event_name}` is not an event (the six are {})",
            Event::ALL.map(Event::name).join(", ")
        )
    })?;
    if !builtin::supports_event(builtin, event) {
        return Err(format!(
            "{at}: the built-in `{builtin}` supports only {}",
            builtin::supported_events(builtin)
        ));
    }
    builtin::validate_options(builtin, options, &at)?;
    let mut tools = Vec::new();
    if let Some(value) = options.get("tools").or_else(|| options.get("tool")) {
        match value {
            toml::Value::String(tool) => tools.push(tool.clone()),
            toml::Value::Array(list) => {
                for tool in list {
                    tools.push(
                        tool.as_str()
                            .ok_or_else(|| format!("{at}: `tools` holds strings"))?
                            .to_string(),
                    );
                }
            }
            _ => return Err(format!("{at}: `tools` is a string or a list")),
        }
    }
    if !tools.is_empty() && !matches!(event, Event::BeforeTool | Event::AfterTool) {
        return Err(format!("{at}: `tools` applies to before_tool/after_tool"));
    }
    Ok(GateDef {
        name: name.to_string(),
        event,
        command: String::new(),
        builtin: Some(builtin.to_string()),
        options: options.clone(),
        tools,
        env: BTreeMap::new(),
        origin: "config".into(),
    })
}

fn one_from_toml(event: Event, item: &toml::Value) -> Result<GateDef, String> {
    let at = event.name();
    match item {
        toml::Value::String(command) => {
            if command.trim().is_empty() {
                return Err(format!("[gates].{at}: an empty command"));
            }
            Ok(GateDef {
                name: name_from_command(command),
                event,
                command: command.clone(),
                builtin: None,
                options: toml::Table::new(),
                tools: Vec::new(),
                env: BTreeMap::new(),
                origin: "config".into(),
            })
        }
        toml::Value::Table(table) => {
            for key in table.keys() {
                if !["command", "name", "tool", "tools", "env"].contains(&key.as_str()) {
                    return Err(format!(
                        "[gates].{at}: unknown key `{key}` (command, name, tools, env)"
                    ));
                }
            }
            let command = table
                .get("command")
                .and_then(toml::Value::as_str)
                .filter(|command| !command.trim().is_empty())
                .ok_or_else(|| format!("[gates].{at}: a table needs a `command` string"))?;
            let name = match table.get("name") {
                Some(toml::Value::String(name)) if valid_name(name) => name.clone(),
                Some(_) => {
                    return Err(format!(
                        "[gates].{at}: `name` is letters, digits, `-`, `_`, `.`"
                    ));
                }
                None => name_from_command(command),
            };
            let mut tools = Vec::new();
            for key in ["tool", "tools"] {
                match table.get(key) {
                    None => {}
                    Some(toml::Value::String(tool)) => tools.push(tool.clone()),
                    Some(toml::Value::Array(list)) => {
                        for tool in list {
                            tools.push(
                                tool.as_str()
                                    .ok_or_else(|| format!("[gates].{at}: `{key}` holds strings"))?
                                    .to_string(),
                            );
                        }
                    }
                    Some(_) => return Err(format!("[gates].{at}: `{key}` is a string or a list")),
                }
            }
            if !tools.is_empty() && !matches!(event, Event::BeforeTool | Event::AfterTool) {
                return Err(format!(
                    "[gates].{at}: `tools` applies to before_tool/after_tool"
                ));
            }
            let mut env = BTreeMap::new();
            if let Some(value) = table.get("env") {
                let map = value
                    .as_table()
                    .ok_or_else(|| format!("[gates].{at}: `env` is a table of strings"))?;
                for (name, value) in map {
                    let value = value
                        .as_str()
                        .ok_or_else(|| format!("[gates].{at}: env `{name}` is not a string"))?;
                    if !valid_env_name(name) {
                        return Err(format!("[gates].{at}: `{name}` is not a variable name"));
                    }
                    env.insert(name.clone(), value.to_string());
                }
            }
            Ok(GateDef {
                name,
                event,
                command: command.to_string(),
                builtin: None,
                options: toml::Table::new(),
                tools,
                env,
                origin: "config".into(),
            })
        }
        _ => Err(format!(
            "[gates].{at}: a command string, a table or a list of them"
        )),
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
}

/// A name for a gate that did not give one: the stem of the last path-like
/// word of its command (`python gates/verify.py --x` is `verify`).
pub fn name_from_command(command: &str) -> String {
    let word = command
        .split_whitespace()
        .rev()
        .find(|word| !word.starts_with('-') && !word.contains('='))
        .or_else(|| command.split_whitespace().next())
        .unwrap_or("gate");
    let word = word.trim_matches(|c| c == '"' || c == '\'');
    let file = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let stem = file.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(file);
    let name: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() { "gate".into() } else { name }
}

/// Add a gate; a name already used gets `-2`, `-3`… so a `gate` event always
/// says which one ran.
pub fn push_unique(defs: &mut Vec<GateDef>, mut def: GateDef) {
    let base = def.name.clone();
    let mut n = 1;
    while defs.iter().any(|other| other.name == def.name) {
        n += 1;
        def.name = format!("{base}-{n}");
    }
    defs.push(def);
}

// -------------------------------------------------------------- skill gates

/// The key a skill's `metadata` uses to bind a script to events.
pub const SKILL_KEY: &str = "kelir-gate";

/// The `kelir-gate` value of a skill's metadata block (block or flow form).
pub fn skill_binding(metadata: &str) -> Option<String> {
    for line in metadata.lines() {
        let line = line.trim().trim_start_matches('{').trim_end_matches('}');
        for part in line.split(',') {
            let part = part.trim();
            if let Some(rest) = part.strip_prefix(SKILL_KEY) {
                let rest = rest.trim_start();
                if let Some(value) = rest.strip_prefix(':') {
                    let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Gates declared by the skills in `found`: a skill whose metadata says `kelir-gate:
/// <event>[=<script>][; <event>=<script>…]` binds a file under its own `scripts/` directory.
pub fn from_skills(
    found: &[crate::features::sources::source::Found],
    notes: &mut Vec<String>,
) -> Vec<GateDef> {
    let mut defs = Vec::new();
    for source in found {
        if source.builtin || source.kind != crate::features::sources::source::SourceKind::Skill {
            continue;
        }
        let Ok(skill) = crate::features::sources::source::parse_skill(&source.text) else {
            continue;
        };
        let Some(metadata) = skill.metadata.as_deref() else {
            continue;
        };
        let Some(binding) = skill_binding(metadata) else {
            continue;
        };
        let Some(dir) = source.path.parent() else {
            continue;
        };
        for part in binding.split(';') {
            let (event_name, script) = match part.split_once('=') {
                Some((event, script)) => (event.trim(), Some(script.trim())),
                None => (part.trim(), None),
            };
            let Some(event) = Event::parse(event_name) else {
                notes.push(format!(
                    "skill {}: kelir-gate `{event_name}` is not an event",
                    skill.name
                ));
                continue;
            };
            match resolve_script(dir, event, script) {
                Ok(path) => push_unique(
                    &mut defs,
                    GateDef {
                        name: format!("{}.{}", skill.name, event.name()),
                        event,
                        command: script_command(&path),
                        builtin: None,
                        options: toml::Table::new(),
                        tools: Vec::new(),
                        env: BTreeMap::new(),
                        origin: if source.layer == crate::features::sources::source::Layer::Project
                        {
                            format!("project skill {}", skill.name)
                        } else {
                            format!("skill {}", skill.name)
                        },
                    },
                ),
                Err(why) => notes.push(format!("skill {}: kelir-gate: {why}", skill.name)),
            }
        }
    }
    defs
}

fn resolve_script(dir: &Path, event: Event, named: Option<&str>) -> Result<PathBuf, String> {
    let scripts = dir.join("scripts");
    if let Some(name) = named {
        let relative = Path::new(name);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(format!("`{name}` must stay inside the skill directory"));
        }
        let path = dir.join(relative);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!("`{name}` is not a file"))
        };
    }
    let files: Vec<PathBuf> = std::fs::read_dir(&scripts)
        .map_err(|_| "the skill has no scripts/ directory".to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    let stem = |path: &PathBuf| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default()
    };
    let wanted = event.name().replace('_', "-");
    for candidate in [event.name(), wanted.as_str(), "gate"] {
        if let Some(path) = files.iter().find(|path| stem(path) == candidate) {
            return Ok(path.clone());
        }
    }
    match files.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err("scripts/ is empty".into()),
        _ => Err(format!(
            "scripts/ holds several files and none is `{}.*` or `gate.*`: name one with `{}=scripts/<file>`",
            event.name(),
            event.name()
        )),
    }
}

/// A gate that came from the project's own files (its `.kelir/config.toml`, a skill under
/// `.kelir/skills` or `.claude/skills`): code the repository's author chose, not the person running
pub fn is_project_gate(def: &GateDef) -> bool {
    def.origin.starts_with("project ")
}

/// Whether the project at `cwd` may run its own gates. kelir has no prompt, so trust is declared:
/// `trusted_projects` in the machine config names a directory at or above `cwd`, or
pub fn project_trusted(cwd: &Path, trusted: &[PathBuf]) -> bool {
    if crate::envs::var("KELIR_TRUST_PROJECT").is_some_and(|v| !matches!(v.trim(), "" | "0")) {
        return true;
    }
    let canon = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let here = canon(cwd);
    trusted.iter().any(|dir| here.starts_with(canon(dir)))
}

/// Drop the project-layer gates of an untrusted project, saying so once per gate.
pub fn apply_trust(defs: Vec<GateDef>, trusted: bool, notes: &mut Vec<String>) -> Vec<GateDef> {
    if trusted {
        return defs;
    }
    defs.into_iter()
        .filter(|def| {
            let project = is_project_gate(def);
            if project {
                notes.push(format!(
                    "gate {} ({}) not started: this project is not trusted (add its directory to `trusted_projects` in the machine config, or set KELIR_TRUST_PROJECT=1)",
                    def.name, def.origin
                ));
            }
            !project
        })
        .collect()
}

pub fn script_command(path: &Path) -> String {
    let shown = crate::util::path_string(path).replace('\\', "/");
    let quoted = format!("\"{shown}\"");
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "py" => format!("{} {quoted}", python()),
        "sh" => format!("bash {quoted}"),
        "js" | "mjs" => format!("node {quoted}"),
        "ps1" => format!("powershell -NoProfile -ExecutionPolicy Bypass -File {quoted}"),
        "cmd" | "bat" => format!("cmd /C {quoted}"),
        _ => quoted,
    }
}

/// Adapt a `cmd /C …` command line to the shell that will run it.
pub fn adapt_for_shell(command: &str, shell_program: &str) -> String {
    let stem = Path::new(shell_program)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let posix = !matches!(stem.as_str(), "cmd" | "powershell" | "pwsh");
    if !cfg!(windows) || !posix {
        return command.to_string();
    }
    let trimmed = command.trim_start();
    for head in ["cmd /c ", "cmd.exe /c "] {
        if trimmed.len() >= head.len() && trimmed[..head.len()].eq_ignore_ascii_case(head) {
            let program = &trimmed[..head.len() - 3];
            return format!("{program}//C {}", &trimmed[head.len()..]);
        }
    }
    command.to_string()
}

/// `python3` where it exists, else `python` (on Windows the `python3` on PATH
/// is often a Store stub).
pub fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::runner::allowed_env;
    use serde_json::json;

    fn def(event: Event, command: &str) -> GateDef {
        GateDef {
            name: name_from_command(command),
            event,
            command: command.to_string(),
            builtin: None,
            options: toml::Table::new(),
            tools: Vec::new(),
            env: BTreeMap::new(),
            origin: "test".into(),
        }
    }

    #[allow(dead_code)]
    fn runner(dir: &Path, defs: Vec<GateDef>) -> Runner {
        std::fs::create_dir_all(dir).unwrap();
        Runner::new(
            defs,
            crate::config::default_shell(),
            dir.to_path_buf(),
            "ses_test".into(),
            dir.to_path_buf(),
        )
        .with_timeout(Duration::from_secs(20))
    }
    // ------------------------------------------------------- decision parsing

    #[test]
    fn duplicate_gate_names_get_distinct_suffixes_and_names_are_validated() {
        let mut defs = Vec::new();
        push_unique(&mut defs, def(Event::TurnEnd, "sh check.sh"));
        push_unique(&mut defs, def(Event::BeforeTool, "sh check.sh"));
        assert_eq!(defs[0].name, "check");
        assert_eq!(defs[1].name, "check-2");
        assert!(valid_name("ok-1_2.3"));
        assert!(!valid_name(""));
        assert!(!valid_name(&"x".repeat(65)));
        assert!(!valid_name("bad name"));
        let token = run_token();
        assert!(
            token.contains('-') && !token.starts_with("fixed-"),
            "{token}"
        );
    }

    #[test]
    fn parse_decision_accepts_allow_deny_rewrite_and_context() {
        let cases: [(&str, Parsed); 5] = [
            (
                r#"{"decision":"allow"}"#,
                Parsed::Allow {
                    reason: String::new(),
                    context: None,
                    usage: None,
                },
            ),
            (
                r#" {"decision":"deny","reason":"no"} "#,
                Parsed::Deny {
                    reason: "no".into(),
                    context: None,
                    usage: None,
                    max_denies: 0,
                },
            ),
            (
                r#"{"decision":"rewrite","input":{"text":"x"}}"#,
                Parsed::Rewrite {
                    reason: String::new(),
                    input: json!({"text": "x"}),
                    context: None,
                    usage: None,
                },
            ),
            (
                r#"{"decision":"allow","context":"a note","kind":"hint"}"#,
                Parsed::Allow {
                    reason: String::new(),
                    context: Some(Context {
                        text: "a note".into(),
                        kind: "hint".into(),
                    }),
                    usage: None,
                },
            ),
            (
                "",
                Parsed::Allow {
                    reason: String::new(),
                    context: None,
                    usage: None,
                },
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_decision(text), Ok(expected), "{text}");
        }
    }

    #[test]
    fn parse_decision_bounds_answers_and_rejects_garbage() {
        let huge = format!(
            r#"{{"decision":"deny","reason":"{}"}}"#,
            "x".repeat(OUTPUT_MAX)
        );
        let long = format!(r#"{{"decision":"deny","reason":"{}"}}"#, "y".repeat(10_000));
        assert!(
            parse_decision("[]")
                .unwrap_err()
                .contains("not a JSON object")
        );
        for text in [
            huge.as_str(),
            "not JSON",
            "[]",
            "{}",
            r#"{"decision":"other"}"#,
            r#"{"decision":"rewrite"}"#,
        ] {
            assert!(parse_decision(text).is_err(), "{text}");
        }
        match parse_decision(&long).unwrap() {
            Parsed::Deny { reason, .. } => assert!(reason.chars().count() <= REASON_MAX + 1),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn declarations_take_strings_tables_lists_and_validated_builtin_options() {
        let value: toml::Value = toml::from_str(
            r#"
turn_end = "python gates/verify.py"
before_tool = [ "sh a.sh", { name = "no-rm", command = "sh b.sh", tools = ["bash"], env = { A = "b" } } ]
session_end = { command = "git push" }
"#,
        )
        .unwrap();
        let defs = from_toml(&value).unwrap();
        assert_eq!(defs.len(), 4);
        let turn_end = defs.iter().find(|d| d.event == Event::TurnEnd).unwrap();
        assert_eq!(turn_end.name, "verify");
        let named = defs.iter().find(|d| d.name == "no-rm").unwrap();
        assert_eq!(named.tools, vec!["bash"]);
        assert_eq!(named.env.get("A").map(String::as_str), Some("b"));

        let value: toml::Value = toml::from_str(
            "hint = { builtin = 'hint', model = 'cheap/model', timeout_ms = 400, max_names = 2 }",
        )
        .unwrap();
        let defs = from_toml(&value).unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "hint");
        assert_eq!(defs[0].event, Event::PromptSubmit);
        assert_eq!(defs[0].builtin.as_deref(), Some("hint"));
        assert!(defs[0].command.is_empty());
        assert_eq!(defs[0].options["model"].as_str(), Some("cheap/model"));

        for (text, key) in [
            (
                "hint = { builtin = 'hint', model = 'm', max_names = \"3\" }",
                "max_names",
            ),
            (
                "hint = { builtin = 'hint', model = 'm', timeout_ms = \"400\" }",
                "timeout_ms",
            ),
            (
                "hint = { builtin = 'hint', model = 'm', timeout = 0 }",
                "timeout",
            ),
            (
                "hint = { builtin = 'hint', model = 'm', max_names = -1 }",
                "max_names",
            ),
        ] {
            let value: toml::Value = toml::from_str(text).unwrap();
            let error = from_toml(&value).unwrap_err();
            assert!(error.contains(key), "{text}: {error}");
        }
        let value: toml::Value =
            toml::from_str("hint = { builtin = 'hint', model = 'm', max_names = 10 }").unwrap();
        assert!(from_toml(&value).is_ok());
    }

    /// A skill may bind a gate script by name, but the name must stay inside
    /// the skill directory, and an unresolved binding is a note, never a gate.
    #[test]
    fn a_skill_binding_stays_inside_the_skill_or_becomes_a_note() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("verify");
        let scripts = root.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        for name in ["turn_end.py", "a.py", "b.py"] {
            std::fs::write(scripts.join(name), "print()").unwrap();
        }
        let found = |metadata: &str| {
            let text = format!("---\nname: verify\ndescription: d\n{metadata}\n---\nbody\n");
            vec![crate::features::sources::source::Found {
                kind: crate::features::sources::source::SourceKind::Skill,
                name: "verify".into(),
                path: root.join("SKILL.md"),
                text,
                layer: crate::features::sources::source::Layer::Project,
                builtin: false,
            }]
        };
        let mut notes = Vec::new();
        let defs = from_skills(&found("metadata: {kelir-gate: turn_end}"), &mut notes);
        assert_eq!(defs.len(), 1, "{notes:?}");
        assert_eq!(defs[0].name, "verify.turn_end");
        assert!(
            defs[0].command.contains("turn_end.py"),
            "{:?}",
            defs[0].command
        );
        notes.clear();

        let defs = from_skills(&found("metadata:\n  kelir-gate: turn_end"), &mut notes);
        assert_eq!(defs.len(), 1, "{notes:?}");
        assert!(
            defs[0].command.starts_with("python3 "),
            "{:?}",
            defs[0].command
        );
        notes.clear();

        // Two script files and no name: a note, not a guess. A traversal out
        // of the skill directory is refused, so a skill cannot name /etc/x.
        std::fs::write(scripts.join("other.py"), "print()").unwrap();
        from_skills(&found("metadata: {kelir-gate: after_tool}"), &mut notes);
        assert!(notes[0].contains("name one"), "{notes:?}");
        notes.clear();
        from_skills(
            &found("metadata: {kelir-gate: \"turn_end=../../etc/x\"}"),
            &mut notes,
        );
        assert!(
            notes.iter().any(|n| n.contains("inside the skill")),
            "{notes:?}"
        );
        notes.clear();
        assert!(from_skills(&found("metadata: {kelir-gate: sometimes}"), &mut notes).is_empty());
    }

    #[test]
    fn tool_patterns_and_skill_metadata_bind_gates() {
        for (pattern, tool, expected) in [
            ("bash", "bash", true),
            ("bash", "edit", false),
            ("mcp:notes", "mcp:notes.search", true),
            ("mcp:notes", "mcp:notesy.search", false),
            ("mcp:notes.search", "mcp:notes.search", true),
            ("mcp:*", "mcp:a.b", true),
        ] {
            assert_eq!(tool_matches(pattern, tool), expected, "{pattern} / {tool}");
        }
        let mut gate = def(Event::BeforeTool, "x");
        assert!(gate.matches(Some("anything")));
        gate.tools = vec!["edit".into()];
        assert!(gate.matches(Some("edit")) && !gate.matches(Some("bash")));

        for (metadata, expected) in [
            ("kelir-gate: turn_end", Some("turn_end")),
            ("{kelir-gate: turn_end}", Some("turn_end")),
            (
                "owner: x, kelir-gate: \"after_tool=scripts/a.py\"",
                Some("after_tool=scripts/a.py"),
            ),
            ("owner: x", None),
        ] {
            assert_eq!(skill_binding(metadata).as_deref(), expected, "{metadata}");
        }
    }

    #[test]
    fn a_gate_never_inherits_a_secret() {
        for secret in [
            "KELIR_TOKEN",
            "KELIR_SOCK",
            "OPENROUTER_API_KEY",
            "KELIR_GO_API_KEY",
            "MY_CUSTOM_KEY_ENV",
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
        ] {
            assert!(!allowed_env(secret), "{secret} must not reach a gate");
        }
        for ordinary in [
            "PATH",
            "HOME",
            "TEMP",
            "SystemRoot",
            "SSH_AUTH_SOCK",
            "LC_ALL",
        ] {
            assert!(allowed_env(ordinary), "{ordinary}");
        }
    }
}
