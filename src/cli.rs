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
    /// Flags for the default session (`kelir --model x`): no subcommand is the
    /// interactive session (`kelir tui`).
    #[command(flatten)]
    pub tui: TuiArgs,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run a task to completion.
    Run(RunArgs),
    /// Ask one question and print the final text.
    Ask(AskArgs),
    /// Start the interactive session (the default with no subcommand).
    Tui(TuiArgs),
    /// Continue a session from its log (never re-runs a tool call).
    Resume(ResumeArgs),
    /// Send a verb to a running session and print its answer.
    Ctl(CtlArgs),
}

#[derive(Debug, Args, Clone, Default)]
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
}

#[derive(Debug, Args, Clone)]
pub struct RunArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// The task. Read from stdin when omitted.
    #[arg(value_name = "TASK")]
    pub task: Vec<String>,
    /// Stream the event log as NDJSON on stdout.
    #[arg(long)]
    pub json: bool,
    /// Stay alive after the turn ends, accepting `say`, `later` and `end`.
    #[arg(long)]
    pub keep: bool,
    /// Continue a session from its log instead of starting one.
    #[arg(long, value_name = "ID")]
    pub resume: Option<String>,
}

/// `kelir resume <id> [task]`: continue a session from its log.
#[derive(Debug, Args, Clone)]
pub struct ResumeArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Session id, or a unique prefix of one.
    #[arg(value_name = "ID")]
    pub id: String,
    /// A message to add before the turn continues. Read from stdin with `-`.
    #[arg(value_name = "TASK")]
    pub task: Vec<String>,
    /// Stream the event log as NDJSON on stdout.
    #[arg(long)]
    pub json: bool,
    /// Stay alive after the turn ends.
    #[arg(long)]
    pub keep: bool,
}

#[derive(Debug, Args, Clone)]
pub struct AskArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// The question. Read from stdin when omitted.
    #[arg(value_name = "QUESTION")]
    pub question: Vec<String>,
}

/// `kelir` with no subcommand, or `kelir tui`.
#[derive(Debug, Args, Clone, Default)]
pub struct TuiArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Continue a session from its log.
    #[arg(long, value_name = "ID")]
    pub resume: Option<String>,
}

impl TuiArgs {
    /// Flags given before the subcommand (`kelir --model x tui`) fill in what
    /// the subcommand did not set, so both spellings mean the same session.
    pub fn with_fallback(self, outer: Self) -> Self {
        Self {
            common: CommonArgs {
                model: self.common.model.or(outer.common.model),
                provider: self.common.provider.or(outer.common.provider),
                cwd: self.common.cwd.or(outer.common.cwd),
                config: self.common.config.or(outer.common.config),
            },
            resume: self.resume.or(outer.resume),
        }
    }
}

#[derive(Debug, Args, Clone)]
pub struct CtlArgs {
    /// Session id, a unique prefix, or `latest` (the newest session only; an
    /// error if it is not running).
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
            Some(Command::Run(args)) => {
                assert!(args.json);
                assert!(!args.keep);
                assert_eq!(args.common.model.as_deref(), Some("m"));
                assert_eq!(args.task, vec!["say hi".to_string()]);
            }
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn run_keep_is_a_flag() {
        let cli = Cli::parse_from(["kelir", "run", "--keep", "go"]);
        match cli.command {
            Some(Command::Run(args)) => assert!(args.keep),
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn task_words_are_joined_later() {
        let cli = Cli::parse_from(["kelir", "run", "fix", "the", "bug"]);
        match cli.command {
            Some(Command::Run(args)) => assert_eq!(args.task.len(), 3),
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
            Some(Command::Ctl(args)) => {
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
            Some(Command::Ctl(args)) => {
                assert_eq!(args.call.as_deref(), Some("call_2"));
                assert_eq!(args.reason.as_deref(), Some("wrong"));
            }
            _ => panic!("expected ctl"),
        }
        let cli = Cli::parse_from(["kelir", "ctl", "ses_1", "tools", "--disable", "bash"]);
        match cli.command {
            Some(Command::Ctl(args)) => assert_eq!(args.disable, vec!["bash".to_string()]),
            _ => panic!("expected ctl"),
        }
    }

    #[test]
    fn resume_parses_an_id_a_task_and_the_flags() {
        let cli = Cli::parse_from(["kelir", "resume", "--json", "ses_01", "carry", "on"]);
        match cli.command {
            Some(Command::Resume(args)) => {
                assert_eq!(args.id, "ses_01");
                assert_eq!(args.task, vec!["carry".to_string(), "on".to_string()]);
                assert!(args.json);
                assert!(!args.keep);
            }
            _ => panic!("expected resume"),
        }
        // `kelir resume <id>` alone: continue whatever the log left in flight.
        let cli = Cli::parse_from(["kelir", "resume", "ses_01"]);
        match cli.command {
            Some(Command::Resume(args)) => assert!(args.task.is_empty()),
            _ => panic!("expected resume"),
        }
        // The same thing as a flag on `run`.
        let cli = Cli::parse_from(["kelir", "run", "--resume", "ses_01", "more"]);
        match cli.command {
            Some(Command::Run(args)) => {
                assert_eq!(args.resume.as_deref(), Some("ses_01"));
                assert_eq!(args.task, vec!["more".to_string()]);
            }
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn an_unknown_subcommand_is_a_usage_error() {
        assert!(Cli::try_parse_from(["kelir", "frobnicate"]).is_err());
    }

    #[test]
    fn bare_kelir_takes_the_tui_flags() {
        // `kelir --model x` is the default session with a model, not a usage
        // error (M2 review N2).
        let cli = Cli::parse_from(["kelir", "--model", "m", "--provider", "p"]);
        assert!(cli.command.is_none(), "no subcommand is the TUI");
        assert_eq!(cli.tui.common.model.as_deref(), Some("m"));
        assert_eq!(cli.tui.common.provider.as_deref(), Some("p"));
        let cli = Cli::parse_from(["kelir", "--resume", "ses_01"]);
        assert_eq!(cli.tui.resume.as_deref(), Some("ses_01"));
    }

    #[test]
    fn the_tui_subcommand_and_the_flags_before_it_are_the_same_session() {
        let cli = Cli::parse_from(["kelir", "--model", "a", "tui", "--provider", "b"]);
        let outer = cli.tui;
        match cli.command {
            Some(Command::Tui(args)) => {
                let args = args.with_fallback(outer);
                assert_eq!(args.common.model.as_deref(), Some("a"));
                assert_eq!(args.common.provider.as_deref(), Some("b"));
            }
            _ => panic!("expected tui"),
        }
        // The subcommand's own flag wins over the same flag before it.
        let cli = Cli::parse_from(["kelir", "--model", "a", "tui", "--model", "b"]);
        let outer = cli.tui;
        match cli.command {
            Some(Command::Tui(args)) => {
                assert_eq!(args.with_fallback(outer).common.model.as_deref(), Some("b"));
            }
            _ => panic!("expected tui"),
        }
    }
}
