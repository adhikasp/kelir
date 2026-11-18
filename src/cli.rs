//! Command line surface: `kelir run`, `kelir ask` and `kelir ctl`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "kelir",
    version,
    about = "a small, steerable coding-agent harness",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run a task to completion.
    Run(RunArgs),
    /// Ask one question and print the final text.
    Ask(AskArgs),
    /// Send a verb to a running session and print its answer.
    Ctl(CtlArgs),
}

#[derive(Debug, Args, Clone)]
pub struct CommonArgs {
    /// Model id for this run (overrides the config).
    #[arg(long)]
    pub model: Option<String>,
    /// Provider name from the config (`openrouter`, `opencode-go`, or a custom one).
    #[arg(long)]
    pub provider: Option<String>,
    /// Working directory for the session (defaults to the current directory).
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// Config file to use instead of the machine/project search.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// The task. Read from stdin when omitted.
    #[arg(value_name = "TASK")]
    pub task: Vec<String>,
}

#[derive(Debug, Args, Clone)]
pub struct RunArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Stream the event log as NDJSON on stdout.
    #[arg(long)]
    pub json: bool,
    /// Stay alive after the turn ends, accepting `say`, `later` and `end`.
    #[arg(long)]
    pub keep: bool,
}

#[derive(Debug, Args, Clone)]
pub struct AskArgs {
    #[command(flatten)]
    pub common: CommonArgs,
}

#[derive(Debug, Args, Clone)]
pub struct CtlArgs {
    /// Session id, a unique prefix, or `latest`.
    #[arg(value_name = "SESSION")]
    pub session: String,
    /// The verb: say, later, stop, model, tools, end.
    #[arg(value_name = "VERB")]
    pub verb: String,
    /// Text for `say`/`later`/`stop`, or one JSON object with the verb's args.
    #[arg(value_name = "ARGS")]
    pub args: Vec<String>,
    /// `say --now`: stop the running tool calls first.
    #[arg(long)]
    pub now: bool,
    /// `stop --call <id>`: stop one running call; the turn continues.
    #[arg(long, value_name = "CALL")]
    pub call: Option<String>,
    /// Why: goes into the stopped call's result.
    #[arg(long, value_name = "REASON")]
    pub reason: Option<String>,
    /// `tools --disable <name>` (repeatable).
    #[arg(long, value_name = "TOOL")]
    pub disable: Vec<String>,
    /// `tools --enable <name>` (repeatable).
    #[arg(long, value_name = "TOOL")]
    pub enable: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_parses_flags_and_a_quoted_task() {
        let cli = Cli::parse_from(["kelir", "run", "--json", "--model", "m", "say hi"]);
        match cli.command {
            Command::Run(args) => {
                assert!(args.json);
                assert!(!args.keep);
                assert_eq!(args.common.model.as_deref(), Some("m"));
                assert_eq!(args.common.task, vec!["say hi".to_string()]);
            }
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn run_keep_is_a_flag() {
        let cli = Cli::parse_from(["kelir", "run", "--keep", "go"]);
        match cli.command {
            Command::Run(args) => assert!(args.keep),
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn task_words_are_joined_later() {
        let cli = Cli::parse_from(["kelir", "run", "fix", "the", "bug"]);
        match cli.command {
            Command::Run(args) => assert_eq!(args.common.task.len(), 3),
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn ask_has_no_json_flag() {
        assert!(Cli::try_parse_from(["kelir", "ask", "--json", "q"]).is_err());
    }

    #[test]
    fn ctl_parses_a_verb_with_text_and_flags() {
        let cli = Cli::parse_from(["kelir", "ctl", "latest", "say", "--now", "change of plan"]);
        match cli.command {
            Command::Ctl(args) => {
                assert_eq!(args.session, "latest");
                assert_eq!(args.verb, "say");
                assert!(args.now);
                assert_eq!(args.args, vec!["change of plan".to_string()]);
            }
            _ => panic!("expected ctl"),
        }
    }

    #[test]
    fn ctl_parses_call_reason_and_tool_flags() {
        let cli = Cli::parse_from([
            "kelir", "ctl", "ses_1", "stop", "--call", "call_2", "--reason", "wrong",
        ]);
        match cli.command {
            Command::Ctl(args) => {
                assert_eq!(args.call.as_deref(), Some("call_2"));
                assert_eq!(args.reason.as_deref(), Some("wrong"));
            }
            _ => panic!("expected ctl"),
        }
        let cli = Cli::parse_from(["kelir", "ctl", "ses_1", "tools", "--disable", "bash"]);
        match cli.command {
            Command::Ctl(args) => assert_eq!(args.disable, vec!["bash".to_string()]),
            _ => panic!("expected ctl"),
        }
    }

    #[test]
    fn an_unknown_subcommand_is_a_usage_error() {
        assert!(Cli::try_parse_from(["kelir", "frobnicate"]).is_err());
    }
}
