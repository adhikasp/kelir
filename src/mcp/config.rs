//! MCP servers are configured in `config.toml` under `[mcp.servers.<name>]`.
//! Two config layers, machine then project, merge by server name: a project's
//! `[mcp.servers.x]` replaces the machine's whole, never field by field.
//! The shapes and validation live here; the parsing is in [`crate::config`].

use std::collections::BTreeMap;
use std::time::Duration;

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

/// A server name is a CLI word: it must not collide with `ls`, and stays in a
/// safe alphabet because it also names a job object and a directory.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "ls"
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Validate and resolve the `[mcp.servers.*]` table into the shape the broker
/// uses. Errors name the offending server, never its `env` values: the error
/// text reaches the model and the log.
pub fn from_raw(
    servers: &BTreeMap<String, crate::config::RawMcpServer>,
) -> Result<BTreeMap<String, ServerConfig>, String> {
    let mut out = BTreeMap::new();
    for (name, server) in servers {
        let at = format!("[mcp.servers.{name}]");
        if !valid_name(name) {
            return Err(format!(
                "invalid {at}: server name `{name}` must be letters, digits, `-` or `_` (and not `ls`)"
            ));
        }
        if server.command.trim().is_empty() {
            return Err(format!(
                "invalid {at}: server `{name}` has an empty command"
            ));
        }
        if server.timeout == Some(0) {
            return Err(format!("invalid {at}: server `{name}` has timeout = 0"));
        }
        for capability in &server.declare_capabilities {
            if crate::channel::forbidden_capability(capability) {
                return Err(format!(
                    "invalid {at}: server `{name}` declares `{capability}`: kelir has no permission relay"
                ));
            }
        }
        out.insert(
            name.clone(),
            ServerConfig {
                name: name.clone(),
                command: server.command.clone(),
                args: server.args.clone(),
                env: server.env.clone(),
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
                    .clone()
                    .unwrap_or_else(|| crate::channel::DEFAULT_REPLY_TOOL.to_string()),
                declare_capabilities: server.declare_capabilities.clone(),
            },
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse one `[mcp.servers.*]` layer through the real config reader, then
    /// validate it: the same path the session and `kelir mcp` use.
    fn servers(text: &str) -> Result<BTreeMap<String, ServerConfig>, String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        let cfg = crate::config::load(Some(&path)).map_err(|e| e.to_string())?;
        Ok(cfg.mcp)
    }

    #[test]
    fn a_server_parses_with_defaults() {
        let parsed = servers("[mcp.servers.a]\ncommand = \"x\"\n").unwrap();
        let a = &parsed["a"];
        assert_eq!(a.command, "x");
        assert!(a.args.is_empty() && a.env.is_empty() && !a.channel);
        assert_eq!(a.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
        assert_eq!(a.description, "");
    }

    #[test]
    fn the_project_replaces_a_machine_server_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[mcp.servers.a]\ncommand = \"m\"\nargs = [\"1\"]\ndescription = \"machine\"\n[mcp.servers.b]\ncommand = \"b\"\n",
        )
        .unwrap();
        let machine = crate::config::load(Some(&path)).unwrap().mcp;
        assert_eq!(machine["a"].command, "m");
        assert_eq!(machine["b"].command, "b");
        let mut project = machine;
        let override_server = servers("[mcp.servers.a]\ncommand = \"p\"\n").unwrap();
        project.insert("a".into(), override_server["a"].clone());
        assert_eq!(project["a"].command, "p");
        assert!(project["a"].args.is_empty(), "no half-merged args");
    }

    #[test]
    fn bad_servers_are_refused_with_the_server_named() {
        for (text, needle) in [
            ("[mcp.servers.a]\n", "command"),
            ("[mcp.servers.a]\ncommand = 'x'\nbogus = 1\n", "bogus"),
            ("[mcp.servers.ls]\ncommand = 'x'\n", "ls"),
            ("[mcp.servers.\"a b\"]\ncommand = 'x'\n", "a b"),
            ("[mcp.servers.a]\ncommand = 'x'\ntimeout = 0\n", "timeout"),
            ("[mcp.servers.a]\ncommand = ' '\n", "empty command"),
        ] {
            let error = servers(text).unwrap_err();
            assert!(error.contains(needle), "{text}: {error}");
        }
    }

    #[test]
    fn a_syntax_error_never_echoes_the_source() {
        // A malformed table body is rejected while parsing config.toml, before
        // any server values reach this module. The error must not echo source
        // text: it reaches the model and the log.
        for text in [
            "[mcp.servers.s]\ncommand='x'\nenv = {{ T = 'x' , }}\n".to_string(),
            "[mcp.servers.s]\ncommand='x'\nenv = {{ T = 'x' \n".to_string(),
            "[mcp.servers.s]\ncommand='x'\nenv = {{ T = 'x\n".to_string(),
            "[mcp.servers.s]\ncommand='x'\nenv = {{ T = 'x', T = 'x' }}\n".to_string(),
        ] {
            let error = servers(&text).unwrap_err();
            assert!(!error.contains("CANARY"), "{text}: {error}");
        }
    }

    #[test]
    fn a_bad_field_type_never_echoes_the_value() {
        let secret = "sk-CANARY-1";
        for field in ["args", "channel", "timeout", "command"] {
            let text = format!("[mcp.servers.s]\ncommand='x'\n{field} = {secret}\n");
            let error = servers(&text).unwrap_err();
            assert!(!error.contains("CANARY"), "{field}: {error}");
        }
    }

    #[test]
    fn only_the_first_description_line_is_kept() {
        let doc = "[mcp.servers.a]\ncommand = 'x'\ndescription = \"\"\"one\ntwo\"\"\"\n";
        let parsed = servers(doc).unwrap();
        assert_eq!(parsed["a"].description, "one");
    }
}
