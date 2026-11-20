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
    /// Queue an image for the model's next message (`view <path>`).
    View(ViewArgs),
    /// Background jobs: run, ls, out, wait, cancel.
    Bg(BgArgs),
    /// Show which key each provider would use (never the key itself).
    Doctor(DoctorArgs),
    /// Read kelir's own reference (config, CLI, verbs, files, …).
    Docs(DocsArgs),
    /// Internal: the background-job shim. Not for humans.
    #[command(hide = true, name = "bg-shim")]
    BgShim(BgShimArgs),
}

#[derive(Debug, Args, Clone, Default)]
pub struct CommonArgs {
    /// Model id for this run (overrides the config).
    #[arg(long)]
    pub model: Option<String>,
    /// Provider name: `openrouter` or `opencode-go` (there are exactly two).
    #[arg(long)]
    pub provider: Option<String>,
    /// Working directory for the session (defaults to the current directory).
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// Config file to use instead of the machine/project search.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Cap one turn at N steps, then pause it (overrides `max_steps`). There is
    /// no cap by default; `0` also means no cap.
    #[arg(long, value_name = "N")]
    pub max_steps: Option<u32>,
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
                max_steps: self.common.max_steps.or(outer.common.max_steps),
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
    /// The verb: say, later, stop, model, tools, compact, skills, view, cancel, status, end.
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
    /// `skills --path <file-or-dir>`: the file (or skill directory) to read.
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// `skills --text <body>`: the source's text, inline.
    #[arg(long, value_name = "TEXT")]
    pub text: Option<String>,
    /// `skills --name <name>`: the source's name (default: the path's).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// `skills --kind <skill|instruction>` (default `skill`).
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// `status --json`: print the answer as one JSON line instead of text.
    #[arg(long)]
    pub json: bool,
}

/// `kelir view <path>`: make an image arrive in the model's next message.
///
/// The model reaches it through `bash`, like every other kelir CLI, so it costs
/// no tool-schema text. It appends a `source` event (kind `attachment`) at the
/// tail of the conversation, which renders as a user message holding one image
/// part; every earlier message (and the provider's cached prefix) stays
/// byte-identical.
#[derive(Debug, Args, Clone)]
pub struct ViewArgs {
    /// The image: `attachments/1-x.png` (resolved against the session dir
    /// first), or any path.
    #[arg(value_name = "PATH")]
    pub path: String,
}

/// `kelir docs [topic]`: the reference, embedded in this binary, so a session
/// with no checkout on disk can still read how to operate kelir.
#[derive(Debug, Args, Clone, Default)]
#[command(after_help = "Exit 0 on a read, 1 for an unknown topic or no search hit.")]
pub struct DocsArgs {
    /// The topic: `config`, `cli`, `verbs`, `env`, `events`, `files`, …
    #[arg(value_name = "TOPIC")]
    pub topic: Option<String>,
    /// List the topics and their one-line summaries.
    #[arg(long)]
    pub list: bool,
    /// Ranked search over every topic: which file mentions these words.
    #[arg(long, value_name = "WORDS")]
    pub find: Option<String>,
    /// Rewrite the generated blocks (`cli`, `env`, `events`, `verbs`, `config`)
    /// from the code. Needs a checkout.
    #[arg(long)]
    pub generate: bool,
    /// Fail when a generated block is stale: CI and the drift test.
    #[arg(long)]
    pub check: bool,
}

/// `kelir doctor`: what the two providers are wired to, and where each key
/// comes from — the source, never the secret.
#[derive(Debug, Args, Clone, Default)]
#[command(after_help = "Exit 0 when every provider checked has a usable key, 1 otherwise.")]
pub struct DoctorArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// One JSON object instead of the text report.
    #[arg(long)]
    pub json: bool,
}

/// `kelir bg …` inside a bash call (or standalone): jobs live in the session's
/// `bg/` when `KELIR_SESSION_DIR` is set, else in the state dir.
#[derive(Debug, Args, Clone)]
pub struct BgArgs {
    #[command(subcommand)]
    pub command: BgCommand,
}

#[derive(Debug, Subcommand, Clone)]
pub enum BgCommand {
    /// Start a job and return at once: `<cmd…>` runs in the configured shell.
    Run(BgRunArgs),
    /// List the jobs of this session (or the state dir's).
    Ls,
    /// Print a job's output (`--follow` streams it until the job settles).
    Out(BgOutArgs),
    /// Wait for a job to settle.
    Wait(BgWaitArgs),
    /// Kill a job's whole process tree and record it as `cancelled`.
    Cancel(BgCancelArgs),
}

#[derive(Debug, Args, Clone)]
pub struct BgRunArgs {
    /// The command, run by the configured shell. One argument is a command line
    /// as written, so shell syntax is allowed; several arguments are literal
    /// words, quoted so their boundaries survive.
    #[arg(
        required = true,
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "COMMAND"
    )]
    pub command: Vec<String>,
}

#[derive(Debug, Args, Clone)]
pub struct BgOutArgs {
    /// The job id (`bg_<ulid>`, or a promoted call's output dir name).
    #[arg(value_name = "ID")]
    pub id: String,
    /// Keep printing as the job writes, until it settles.
    #[arg(long)]
    pub follow: bool,
}

#[derive(Debug, Args, Clone)]
pub struct BgWaitArgs {
    #[arg(value_name = "ID")]
    pub id: String,
    /// Give up after this many seconds instead of waiting forever.
    #[arg(long, value_name = "SECONDS")]
    pub timeout: Option<u64>,
}

#[derive(Debug, Args, Clone)]
pub struct BgCancelArgs {
    #[arg(value_name = "ID")]
    pub id: String,
}

/// `kelir bg-shim`: started by the session (for every bash call) and by
/// `kelir bg run`. Everything it needs is in its arguments, so a shim is a
/// self-contained, detachable process.
#[derive(Debug, Args, Clone)]
pub struct BgShimArgs {
    /// The `bg/` directory the status file lives in.
    #[arg(long, value_name = "DIR")]
    pub dir: PathBuf,
    /// The job id (also the status file's stem).
    #[arg(long, value_name = "ID")]
    pub id: String,
    /// The file the command's stdout and stderr go to.
    #[arg(long, value_name = "FILE")]
    pub out: PathBuf,
    /// The command, as one shell command line. Ignored when `cmd_file` is
    /// given: a command past the Windows command-line limit travels as a file
    /// and never on this process's own argv.
    #[arg(
        long,
        value_name = "CMD",
        allow_hyphen_values = true,
        default_value = ""
    )]
    pub cmd: String,
    /// A file holding the command (`--cmd` is then empty). The session writes
    /// it for a command too long to pass as an argument.
    #[arg(long, value_name = "FILE")]
    pub cmd_file: Option<PathBuf>,
    /// The shell program (`bash`).
    #[arg(long, value_name = "PROGRAM")]
    pub shell: String,
    /// An argument of the shell's argv, before the command (`-c`).
    #[arg(long = "shell-arg", value_name = "ARG", allow_hyphen_values = true)]
    pub shell_args: Vec<String>,
    /// `job` (`kelir bg run`) or `call` (a bash call's command).
    #[arg(long, value_name = "KIND", default_value = "job")]
    pub kind: String,
    /// The pid of the session that owns this call: if it dies without
    /// promoting the call, the shim takes the tree down.
    #[arg(long, value_name = "PID")]
    pub owner: Option<u32>,
    /// The provider call id of the bash call that started this job.
    #[arg(long, value_name = "CALL_ID")]
    pub call: Option<String>,
    /// The Windows job object the call's tree lives in.
    #[arg(long, value_name = "NAME")]
    pub job: Option<String>,
    /// The session socket, exported to the command as `KELIR_SOCK`.
    #[arg(long, value_name = "ADDRESS")]
    pub sock: Option<String>,
    /// The status key arrives on stdin (one line), never in argv or the
    /// environment: it signs every status record this shim writes.
    #[arg(long)]
    pub key_stdin: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_takes_one_path() {
        let cli = Cli::parse_from(["kelir", "view", "attachments/1-x.png"]);
        match cli.command {
            Some(Command::View(args)) => assert_eq!(args.path, "attachments/1-x.png"),
            other => panic!("{other:?}"),
        }
    }

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
    fn bg_parses_run_ls_out_wait_and_cancel() {
        let cli = Cli::parse_from(["kelir", "bg", "run", "sleep", "60"]);
        match cli.command {
            Some(Command::Bg(args)) => match args.command {
                BgCommand::Run(run) => assert_eq!(run.command, vec!["sleep", "60"]),
                other => panic!("expected run, got {other:?}"),
            },
            _ => panic!("expected bg"),
        }
        // A quoted command, with flags inside it, stays one argument.
        let cli = Cli::parse_from(["kelir", "bg", "run", "sleep 3; echo --ok"]);
        match cli.command {
            Some(Command::Bg(args)) => match args.command {
                BgCommand::Run(run) => assert_eq!(run.command, vec!["sleep 3; echo --ok"]),
                other => panic!("expected run, got {other:?}"),
            },
            _ => panic!("expected bg"),
        }
        assert!(Cli::try_parse_from(["kelir", "bg", "run"]).is_err());
        assert!(matches!(
            Cli::parse_from(["kelir", "bg", "ls"]).command,
            Some(Command::Bg(BgArgs {
                command: BgCommand::Ls
            }))
        ));
        let cli = Cli::parse_from(["kelir", "bg", "out", "bg_1", "--follow"]);
        match cli.command {
            Some(Command::Bg(args)) => match args.command {
                BgCommand::Out(out) => {
                    assert_eq!(out.id, "bg_1");
                    assert!(out.follow);
                }
                other => panic!("expected out, got {other:?}"),
            },
            _ => panic!("expected bg"),
        }
        let cli = Cli::parse_from(["kelir", "bg", "wait", "bg_1", "--timeout", "5"]);
        match cli.command {
            Some(Command::Bg(args)) => match args.command {
                BgCommand::Wait(wait) => assert_eq!(wait.timeout, Some(5)),
                other => panic!("expected wait, got {other:?}"),
            },
            _ => panic!("expected bg"),
        }
        let cli = Cli::parse_from(["kelir", "bg", "cancel", "bg_1"]);
        match cli.command {
            Some(Command::Bg(args)) => match args.command {
                BgCommand::Cancel(cancel) => assert_eq!(cancel.id, "bg_1"),
                other => panic!("expected cancel, got {other:?}"),
            },
            _ => panic!("expected bg"),
        }
    }

    #[test]
    fn the_shim_parses_its_scripted_arguments() {
        let cli = Cli::parse_from([
            "kelir",
            "bg-shim",
            "--dir",
            "s/ses_1/bg",
            "--id",
            "11-2-1",
            "--out",
            "s/ses_1/commands/11-2-1/out",
            "--cmd",
            "sleep 60",
            "--shell",
            "bash",
            "--shell-arg",
            "-c",
            "--kind",
            "call",
            "--owner",
            "1234",
            "--call",
            "call_1",
            "--job",
            "kelir-bg-ses_1-11-2-1",
        ]);
        match cli.command {
            Some(Command::BgShim(args)) => {
                assert_eq!(args.id, "11-2-1");
                assert_eq!(args.cmd, "sleep 60");
                assert_eq!(args.shell, "bash");
                assert_eq!(args.shell_args, vec!["-c"]);
                assert_eq!(args.kind, "call");
                assert_eq!(args.owner, Some(1234));
                assert_eq!(args.call.as_deref(), Some("call_1"));
                assert_eq!(args.job.as_deref(), Some("kelir-bg-ses_1-11-2-1"));
            }
            _ => panic!("expected bg-shim"),
        }
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

    /// `--max-steps N` is a per-session flag on every command that runs a
    /// session, and it falls through the same way the other common flags do.
    #[test]
    fn the_max_steps_flag_is_common_and_overrides_the_config() {
        match Cli::parse_from(["kelir", "run", "--max-steps", "5", "task"]).command {
            Some(Command::Run(args)) => assert_eq!(args.common.max_steps, Some(5)),
            _ => panic!("expected run"),
        }
        match Cli::parse_from(["kelir", "ask", "--max-steps", "0", "q"]).command {
            Some(Command::Ask(args)) => assert_eq!(args.common.max_steps, Some(0)),
            _ => panic!("expected ask"),
        }
        // Unset stays unset: no cap is the default, never a zero.
        match Cli::parse_from(["kelir", "run", "task"]).command {
            Some(Command::Run(args)) => assert_eq!(args.common.max_steps, None),
            _ => panic!("expected run"),
        }
        // Before the subcommand, `--max-steps` lands on the TUI and falls
        // through to `tui` like the other common flags.
        let cli = Cli::parse_from(["kelir", "--max-steps", "9", "tui"]);
        let outer = cli.tui;
        assert_eq!(outer.common.max_steps, Some(9));
        match cli.command {
            Some(Command::Tui(args)) => {
                assert_eq!(args.with_fallback(outer).common.max_steps, Some(9));
            }
            _ => panic!("expected tui"),
        }
    }
}
