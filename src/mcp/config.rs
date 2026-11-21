//! `mcp.toml`: which MCP servers exist.
//! Two layers, machine then project, merged **by server name**: a project's `[servers.x]` replaces
//! the machine's `[servers.x]` whole (a half-merged `command` from one file and `args` from the
//! other would run something neither file asked for).
//! The machine file is `<config dir>/mcp.toml` (`KELIR_MCP_CONFIG` replaces that path); the

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// Seconds one tool call may take when the server sets no `timeout`.
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub description: String,
    pub channel: bool,
    pub timeout: Duration,
    /// Channel mode: the tool this server declares for sending an answer.
    pub reply_tool: String,
    /// Channel mode: capabilities declared at `initialize` so the bridge knows
    /// to send inbound messages. Never a permission relay.
    pub declare_capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServer {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    channel: bool,
    timeout: Option<u64>,
    #[serde(default)]
    reply_tool: Option<String>,
    #[serde(default)]
    declare_capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    servers: BTreeMap<String, RawServer>,
}

/// A server name is a CLI word: it must not collide with `ls`, and stays in a
/// safe alphabet because it also names a job object and a directory.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "ls"
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A TOML error without the source text.
fn safe_error(text: &str, error: &toml::de::Error) -> String {
    let message = error.message();
    // "invalid type: string `x`, expected a boolean" -> keep the expectation.
    let message = match message.split_once(", expected ") {
        Some((head, tail)) if head.starts_with("invalid ") => {
            let kind = head.split(':').next().unwrap_or("invalid value");
            format!("{kind} (value hidden), expected {tail}")
        }
        _ if message.starts_with("invalid ") && message.contains('`') => {
            message
                .split(':')
                .next()
                .unwrap_or("invalid value")
                .to_string()
                + " (value hidden)"
        }
        _ => message.to_string(),
    };
    match error.span() {
        Some(span) => {
            let before = &text[..span.start.min(text.len())];
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            format!("line {line}, column {column}: {message}")
        }
        None => message,
    }
}

/// Parse one layer. `origin` is only for the error text.
pub fn parse_layer(text: &str, origin: &str) -> Result<BTreeMap<String, ServerConfig>, String> {
    let raw: RawFile = toml::from_str(text)
        .map_err(|error| format!("invalid {origin}: {}", safe_error(text, &error)))?;
    let mut out = BTreeMap::new();
    for (name, server) in raw.servers {
        if !valid_name(&name) {
            return Err(format!(
                "invalid {origin}: server name `{name}` must be letters, digits, `-` or `_` \
                 (and not `ls`)"
            ));
        }
        if server.command.trim().is_empty() {
            return Err(format!(
                "invalid {origin}: server `{name}` has an empty command"
            ));
        }
        if server.timeout == Some(0) {
            return Err(format!("invalid {origin}: server `{name}` has timeout = 0"));
        }
        for capability in &server.declare_capabilities {
            if crate::channel::forbidden_capability(capability) {
                return Err(format!(
                    "invalid {origin}: server `{name}` declares `{capability}`: \
                     kelir has no permission relay"
                ));
            }
        }
        out.insert(
            name.clone(),
            ServerConfig {
                name,
                command: server.command,
                args: server.args,
                env: server.env,
                description: server
                    .description
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                channel: server.channel,
                timeout: Duration::from_secs(server.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS)),
                reply_tool: server
                    .reply_tool
                    .unwrap_or_else(|| crate::channel::DEFAULT_REPLY_TOOL.to_string()),
                declare_capabilities: server.declare_capabilities,
            },
        );
    }
    Ok(out)
}

/// `project` wins by name.
pub fn layer(
    machine: BTreeMap<String, ServerConfig>,
    project: BTreeMap<String, ServerConfig>,
) -> BTreeMap<String, ServerConfig> {
    let mut merged = machine;
    merged.extend(project);
    merged
}

pub fn machine_path() -> PathBuf {
    match crate::envs::var_os("KELIR_MCP_CONFIG") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => crate::paths::config_dir().join("mcp.toml"),
    }
}

/// The nearest `.kelir/mcp.toml` at or above `cwd`.
pub fn project_path(cwd: &Path) -> Option<PathBuf> {
    let mut dir = cwd.to_path_buf();
    loop {
        let candidate = dir.join(".kelir").join("mcp.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn read_layer(path: &Path) -> Result<BTreeMap<String, ServerConfig>, String> {
    if !path.is_file() {
        return Ok(BTreeMap::new());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    parse_layer(&text, &path.display().to_string())
}

/// Both layers, for a process whose working directory is `cwd`.
pub fn load(cwd: &Path) -> Result<BTreeMap<String, ServerConfig>, String> {
    let machine = read_layer(&machine_path())?;
    let project = match project_path(cwd) {
        Some(path) => read_layer(&path)?,
        None => BTreeMap::new(),
    };
    Ok(layer(machine, project))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_parses_with_defaults() {
        let servers = parse_layer("[servers.a]\ncommand = \"x\"\n", "t").unwrap();
        let a = &servers["a"];
        assert_eq!(a.command, "x");
        assert!(a.args.is_empty() && a.env.is_empty() && !a.channel);
        assert_eq!(a.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
        assert_eq!(a.description, "");
    }

    #[test]
    fn the_project_replaces_a_machine_server_whole() {
        let machine = parse_layer(
            "[servers.a]\ncommand = \"m\"\nargs = [\"1\"]\ndescription = \"machine\"\n\
             [servers.b]\ncommand = \"b\"\n",
            "m",
        )
        .unwrap();
        let project = parse_layer("[servers.a]\ncommand = \"p\"\n", "p").unwrap();
        let merged = layer(machine, project);
        assert_eq!(merged["a"].command, "p");
        assert!(merged["a"].args.is_empty(), "no half-merged args");
        assert_eq!(merged["a"].description, "");
        assert_eq!(merged["b"].command, "b");
    }

    #[test]
    fn bad_files_are_refused_with_the_file_named() {
        for (text, needle) in [
            ("[servers.a]\n", "command"),
            ("[servers.a]\ncommand = \"x\"\nbogus = 1\n", "bogus"),
            ("[servers.ls]\ncommand = \"x\"\n", "ls"),
            ("[servers.\"a b\"]\ncommand = \"x\"\n", "a b"),
            ("[servers.a]\ncommand = \"x\"\ntimeout = 0\n", "timeout"),
            ("[servers.a]\ncommand = \" \"\n", "empty command"),
            ("[serverz.a]\ncommand = \"x\"\n", "serverz"),
        ] {
            let error = parse_layer(text, "FILE").unwrap_err();
            assert!(error.contains("FILE") && error.contains(needle), "{error}");
        }
    }

    #[test]
    fn a_syntax_error_never_echoes_the_source() {
        let head = "[servers.s]\ncommand = 'x'\n";
        for tail in [
            r#"env = { API_TOKEN = "sk-CANARY-1" , } oops"#,
            "env = { API_TOKEN = 12345678 }",
            "env = { API_TOKEN = \"sk-CANARY-1\" \n",
            r#"env = { A = "sk-CANARY-1", A = "sk-CANARY-2" }"#,
            r#"env = "sk-CANARY-1""#,
            r#"args = "sk-CANARY-1""#,
            r#"channel = "sk-CANARY-1""#,
            r#"timeout = "sk-CANARY-1""#,
            r#"env = { A = "sk-CANARY-1\q" }"#,
            r#"env = { A = ["sk-CANARY-1"] }"#,
        ] {
            let text = format!("{head}{tail}\n");
            let error = parse_layer(&text, "FILE").unwrap_err();
            assert!(
                !error.contains("CANARY") && !error.contains("12345678"),
                "{error}"
            );
            assert!(error.contains("FILE") && error.contains("line "), "{error}");
        }
    }

    #[test]
    fn only_the_first_description_line_is_kept() {
        let servers = parse_layer(
            "[servers.a]\ncommand = \"x\"\ndescription = \"\"\"one\ntwo\"\"\"\n",
            "t",
        )
        .unwrap();
        assert_eq!(servers["a"].description, "one");
    }

    #[test]
    fn the_nearest_project_file_wins() {
        let root = std::env::temp_dir().join(format!("kelir-mcp-cfg-{}", ulid::Ulid::generate()));
        let deep = root.join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(root.join(".kelir")).unwrap();
        std::fs::write(root.join(".kelir").join("mcp.toml"), "").unwrap();
        assert_eq!(
            project_path(&deep),
            Some(root.join(".kelir").join("mcp.toml"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
