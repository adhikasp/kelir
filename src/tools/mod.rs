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
    /// The session's socket token, exported to every child as `KELIR_TOKEN`
    /// (STEER-7): a verb that crosses the socket must carry it. It is never
    /// written to the log, to stdout or to an argv.
    pub token: String,
    /// This session's id: the bash call's job object name is derived from it.
    pub session_id: String,
    /// The session's trust state: a bash call asks it for the status key the
    /// session minted for that call and hands it to the call's shim over a
    /// pipe (STEER-7 round 2). `None` outside a session (a test's `Ctx`).
    pub trust: Option<std::sync::Arc<crate::trust::Trust>>,
    /// A call still running after this long is promoted to a background job
    /// (`promote_after` in the config; zero disables it).
    pub promote_after: std::time::Duration,
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
            token: String::new(),
            session_id: String::new(),
            trust: None,
            promote_after: std::time::Duration::from_secs(600),
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

    /// A successful result the session produced itself (the `compact` tool: the
    /// compaction happens at the next boundary, not inside the call).
    pub fn ok(result: String) -> Self {
        Self::new("ok", result, None)
    }

    /// The turn was stopped (or this call was) while it ran.
    pub fn stopped(reason: &str) -> Self {
        Self::new("stopped", stopped_text(reason), None)
    }

    /// The same, for a call that had already written output: the model gets the
    /// byte and line counts, the file and its tail, so the partial work is not
    /// lost (M1 review N2).
    pub fn stopped_with_output(
        reason: &str,
        out_path: &str,
        bytes: u64,
        lines: u64,
        tail: &str,
    ) -> Self {
        let mut result = stopped_text(reason);
        result.push_str(&format!(
            "\npartial output: {bytes} bytes, {lines} lines; full output at {out_path}"
        ));
        if !tail.is_empty() {
            result.push_str("\n--- tail ---\n");
            result.push_str(tail);
            if !tail.ends_with('\n') {
                result.push('\n');
            }
        }
        Self {
            outcome: "stopped".into(),
            exit_code: None,
            bytes,
            lines,
            out_path: out_path.to_string(),
            spilled: false,
            result,
        }
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
    /// A call a `before_tool` gate vetoed: the model reads who and why.
    pub fn denied(gate: &str, reason: &str) -> Self {
        Self::new("denied", denied_text(gate, reason), None)
    }

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

fn stopped_text(reason: &str) -> String {
    if reason.is_empty() {
        "stopped by user".to_string()
    } else {
        format!("stopped by user: {reason}")
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
                "description": "Run a command in the session shell. The cwd is the session cwd and every call starts there; no shell state carries over between calls. Output goes to a file; a result over the spill limit comes back as a notice with a path and a head/tail. A command over 6000 characters is written to a file and run from there, so length is not a limit. Create big files with `edit` (empty `old`), not with a heredoc. Supply a short `description` of what the command does: it is what a human sees in the transcript instead of the command.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "The command line to run."},
                        "description": {"type": "string", "description": "Optional short human-readable label for what the command does, at most 120 characters; the command itself stays in the log, never in this field."}
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
                "description": "Replace exact strings in one file. One change: `old` + `new`. Several: `edits`, applied in order against the evolving text, all or none. Each `old` matches exactly once unless `replace_all`. An empty `old` creates a missing file. Line endings are preserved. Prefer this to a python/sed patch, which can fail with exit 0.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File, relative to the cwd or absolute."},
                        "old": {"type": "string", "description": "Exact text to replace; empty creates a missing file."},
                        "new": {"type": "string", "description": "Replacement text."},
                        "replace_all": {"type": "boolean", "description": "Replace every occurrence."},
                        "edits": {
                            "type": "array",
                            "description": "Several changes, in order; all or none.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "old": {"type": "string"},
                                    "new": {"type": "string"},
                                    "replace_all": {"type": "boolean"}
                                },
                                "required": ["old", "new"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "compact",
                "description": "Compact this session's context: the earlier conversation is replaced by a summary, keeping the task and the messages a human typed. Use it when the history has grown long and the details no longer matter. It applies at the next step.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "focus": {"type": "string", "description": "What the summary must not lose (optional)."}
                    },
                    "required": [],
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
/// model can read. `dir` is kelir's `<seq>-<step>-<n>` output directory name and
/// `call_id` is the provider's call id (recorded with a promoted job).
pub async fn dispatch(
    name: &str,
    args: &Value,
    ctx: &Ctx,
    dir: &str,
    call_id: &str,
) -> ToolOutcome {
    match name {
        "bash" => match args.get("command").and_then(|v| v.as_str()) {
            Some(command) if !command.trim().is_empty() => {
                bash::run(command, ctx, dir, call_id).await
            }
            _ => ToolOutcome::error("bash: missing non-empty `command`"),
        },
        "edit" => edit::run(args, ctx, dir),
        other => ToolOutcome::error(format!("unknown tool: {other}")),
    }
}

/// `denied by gate <name>: <reason>`: the one sentence both a native call and
/// an MCP call get (the SPEC's wording).
pub fn denied_text(gate: &str, reason: &str) -> String {
    if reason.trim().is_empty() {
        format!("denied by gate {gate}")
    } else {
        format!("denied by gate {gate}: {}", reason.trim())
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
        assert_eq!(specs.len(), 3);
        for spec in &specs {
            assert_eq!(spec["type"], "function");
            let function = &spec["function"];
            assert!(function["name"].is_string());
            assert!(function["description"].is_string());
            assert_eq!(function["parameters"]["type"], "object");
        }
    }

    #[test]
    fn bash_takes_an_optional_description_beside_the_command() {
        let spec = specs()
            .into_iter()
            .find(|spec| spec["function"]["name"] == "bash")
            .expect("a bash tool");
        let parameters = &spec["function"]["parameters"];
        assert_eq!(parameters["required"], json!(["command"]));
        assert_eq!(parameters["additionalProperties"], json!(false));
        let properties = parameters["properties"].as_object().expect("properties");
        assert_eq!(properties.len(), 2, "{properties:?}");
        assert_eq!(properties["command"]["type"], "string");
        assert_eq!(properties["description"]["type"], "string");
        assert!(
            properties["description"]["description"]
                .as_str()
                .is_some_and(|text| text.contains("120"))
        );
    }

    #[test]
    fn names_are_stable_and_ordered() {
        assert_eq!(
            names(),
            vec![
                "bash".to_string(),
                "edit".to_string(),
                "compact".to_string()
            ]
        );
    }

    #[test]
    fn compact_takes_an_optional_focus_and_nothing_else() {
        let spec = specs()
            .into_iter()
            .find(|spec| spec["function"]["name"] == "compact")
            .expect("a compact tool");
        assert_eq!(spec["function"]["parameters"]["required"], json!([]));
        assert_eq!(
            spec["function"]["parameters"]["properties"]["focus"]["type"],
            "string"
        );
        assert_eq!(
            spec["function"]["parameters"]["additionalProperties"],
            false
        );
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

    #[test]
    fn a_stopped_call_reports_its_partial_output_and_out_path() {
        let stopped = ToolOutcome::stopped_with_output(
            "wrong approach",
            "s/ses_1/commands/7-1-1/out",
            42,
            2,
            "line one\nline two\n",
        );
        assert_eq!(stopped.outcome, "stopped");
        assert_eq!(stopped.bytes, 42);
        assert_eq!(stopped.lines, 2);
        assert_eq!(stopped.out_path, "s/ses_1/commands/7-1-1/out");
        assert!(
            stopped
                .result
                .starts_with("stopped by user: wrong approach"),
            "{}",
            stopped.result
        );
        assert!(
            stopped.result.contains("42 bytes, 2 lines"),
            "{}",
            stopped.result
        );
        assert!(
            stopped.result.contains("s/ses_1/commands/7-1-1/out"),
            "{}",
            stopped.result
        );
        assert!(stopped.result.contains("line two"), "{}", stopped.result);
        // A missing tail still names the counts and the file.
        let bare = ToolOutcome::stopped_with_output("", "out", 5, 1, "");
        assert_eq!(
            bare.result,
            "stopped by user\npartial output: 5 bytes, 1 lines; full output at out"
        );
        assert!(!bare.result.contains("--- tail ---"));
    }
}
