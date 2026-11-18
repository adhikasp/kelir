//! Command line surface for M0: `kelir run` and `kelir ask`.

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
}

#[derive(Debug, Args, Clone)]
pub struct AskArgs {
    #[command(flatten)]
    pub common: CommonArgs,
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
                assert_eq!(args.common.model.as_deref(), Some("m"));
                assert_eq!(args.common.task, vec!["say hi".to_string()]);
            }
            Command::Ask(_) => panic!("expected run"),
        }
    }

    #[test]
    fn task_words_are_joined_later() {
        let cli = Cli::parse_from(["kelir", "run", "fix", "the", "bug"]);
        match cli.command {
            Command::Run(args) => assert_eq!(args.common.task.len(), 3),
            Command::Ask(_) => panic!("expected run"),
        }
    }

    #[test]
    fn ask_has_no_json_flag() {
        assert!(Cli::try_parse_from(["kelir", "ask", "--json", "q"]).is_err());
    }

    #[test]
    fn an_unknown_subcommand_is_a_usage_error() {
        assert!(Cli::try_parse_from(["kelir", "frobnicate"]).is_err());
    }
}
