//! The native tools. Fixed for the life of a session so the request prefix
//! (and therefore the cache) stays stable.

pub mod bash;
pub mod edit;

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::proc::{Running, SessionGroups};

#[derive(Debug, Clone)]
pub struct Ctx {
    pub session_dir: PathBuf,
    pub cwd: PathBuf,
    pub shell: Vec<String>,
    pub spill_limit: usize,
    /// The address of the session socket, exported to every child as
    /// `KELIR_SOCK` (`kelir mcp` and `kelir bg` use it; tests assert it).
    pub sock: String,
    /// Process groups of every call this session started. Holding them keeps
    /// the Windows Job Objects open past the call (a leftover background child
    /// is not killed by returning) and closes them when the session ends.
    pub groups: SessionGroups,
    /// The calls running right now, so a verb can kill one or all of them.
    pub running: Running,
}

impl Ctx {
    pub fn new(
        session_dir: PathBuf,
        cwd: PathBuf,
        shell: Vec<String>,
        spill_limit: usize,
        sock: String,
    ) -> Self {
        Self {
            session_dir,
            cwd,
            shell,
            spill_limit,
            sock,
            groups: SessionGroups::new(),
            running: Running::new(),
        }
    }

    /// `<session>/commands/<dir>`, where `<dir>` is kelir's own
    /// `<seq>-<step>-<n>` name: monotonic, filesystem-safe, and never derived
    /// from a provider-supplied id.
    pub fn call_dir(&self, dir: &str) -> PathBuf {
        self.session_dir.join("commands").join(dir)
    }

    pub fn keep_group(&self, group: crate::proc::ProcessGroup) {
        self.groups.keep(group);
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub bytes: u64,
    pub lines: u64,
    pub out_path: String,
    pub spilled: bool,
    pub result: String,
}

impl ToolOutcome {
    pub fn error(message: impl Into<String>) -> Self {
        Self::new("error", message.into(), None)
    }

    /// The turn was stopped (or this call was) while it ran.
    pub fn stopped(reason: &str) -> Self {
        let result = if reason.is_empty() {
            "stopped by user".to_string()
        } else {
            format!("stopped by user: {reason}")
        };
        Self::new("stopped", result, None)
    }

    /// The turn was stopped before this call started.
    pub fn skipped(reason: &str) -> Self {
        let result = if reason.is_empty() {
            "[Skipped] the turn was stopped before this call ran".to_string()
        } else {
            format!("[Skipped] the turn was stopped before this call ran: {reason}")
        };
        Self::new("skipped", result, None)
    }

    /// The tool is turned off for this session; the list sent to the provider
    /// never changed.
    pub fn disabled(name: &str) -> Self {
        Self::new(
            "disabled",
            format!(
                "disabled by user: the `{name}` tool is off for this session (`kelir ctl <session> tools --enable {name}` turns it back on)"
            ),
            None,
        )
    }

    fn new(outcome: &str, result: String, exit_code: Option<i32>) -> Self {
        let bytes = result.len() as u64;
        let lines = crate::spill::count_lines(result.as_bytes());
        Self {
            outcome: outcome.into(),
            exit_code,
            bytes,
            lines,
            out_path: String::new(),
            spilled: false,
            result,
        }
    }
}

/// The tool schemas sent to the provider. The same names exist for the whole
/// session; later milestones add `spawn` and `compact`.
pub fn specs() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Run a command in the session shell. Output goes to a file; a result over the spill limit comes back as a notice with a path and a head/tail.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "The command line to run."}
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Replace an exact string in one file. `old` must match exactly once unless `replace_all` is true. An empty `old` creates a missing file with `new`. Line endings are preserved.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File to edit, relative to the session cwd or absolute."},
                        "old": {"type": "string", "description": "Exact text to replace; empty only when creating a missing file."},
                        "new": {"type": "string", "description": "Replacement text."},
                        "replace_all": {"type": "boolean", "description": "Replace every occurrence instead of requiring exactly one."}
                    },
                    "required": ["path", "old", "new"],
                    "additionalProperties": false
                }
            }
        }),
    ]
}

pub fn names() -> Vec<String> {
    specs()
        .iter()
        .filter_map(|spec| spec["function"]["name"].as_str().map(str::to_string))
        .collect()
}

/// Run one tool call. Never fails: any problem becomes an `error` outcome the
/// model can read. `dir` is kelir's `<seq>-<step>-<n>` output directory name.
pub async fn dispatch(name: &str, args: &Value, ctx: &Ctx, dir: &str) -> ToolOutcome {
    match name {
        "bash" => match args.get("command").and_then(|v| v.as_str()) {
            Some(command) if !command.trim().is_empty() => bash::run(command, ctx, dir).await,
            _ => ToolOutcome::error("bash: missing non-empty `command`"),
        },
        "edit" => edit::run(args, ctx, dir),
        other => ToolOutcome::error(format!("unknown tool: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        Ctx::new(
            PathBuf::from("s").join("ses_1"),
            PathBuf::from("."),
            vec!["bash".into(), "-c".into()],
            10_240,
            "s/ses_1/sock".into(),
        )
    }

    #[test]
    fn specs_are_openai_function_tools() {
        let specs = specs();
        assert_eq!(specs.len(), 2);
        for spec in &specs {
            assert_eq!(spec["type"], "function");
            let function = &spec["function"];
            assert!(function["name"].is_string());
            assert!(function["description"].is_string());
            assert_eq!(function["parameters"]["type"], "object");
        }
    }

    #[test]
    fn names_are_stable_and_ordered() {
        assert_eq!(names(), vec!["bash".to_string(), "edit".to_string()]);
    }

    #[test]
    fn call_dir_is_under_commands_without_the_call_id() {
        let dir = ctx().call_dir("12-3-2");
        assert!(dir.ends_with(PathBuf::from("commands").join("12-3-2")));
        assert!(!dir.to_string_lossy().contains("call_"));
    }

    #[test]
    fn stopped_skipped_and_disabled_outcomes_carry_readable_results() {
        let stopped = ToolOutcome::stopped("we changed plan");
        assert_eq!(stopped.outcome, "stopped");
        assert_eq!(stopped.result, "stopped by user: we changed plan");
        assert_eq!(ToolOutcome::stopped("").result, "stopped by user");

        let skipped = ToolOutcome::skipped("stopped");
        assert_eq!(skipped.outcome, "skipped");
        assert!(
            skipped.result.starts_with("[Skipped]"),
            "{}",
            skipped.result
        );
        assert!(!ToolOutcome::skipped("").result.contains('('));

        let disabled = ToolOutcome::disabled("bash");
        assert_eq!(disabled.outcome, "disabled");
        assert!(disabled.result.contains("disabled by user"));
        assert!(disabled.result.contains("--enable bash"));
        assert_eq!(disabled.exit_code, None);
    }
}
