use clap::Parser;
use std::process::ExitCode;

use kelir::cli::{Cli, Command};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = match cli.command {
        Some(Command::Run(args)) => kelir::session::run(args).await,
        Some(Command::Ask(args)) => kelir::session::ask(args).await,
        // Flags before the subcommand fill in what it did not set.
        Some(Command::Tui(args)) => kelir::session::tui(args.with_fallback(cli.tui)).await,
        Some(Command::Resume(args)) => kelir::session::resume(args).await,
        Some(Command::Ctl(args)) => kelir::ctl::ctl(args).await,
        // `kelir` with no subcommand is the interactive session.
        None => kelir::session::tui(cli.tui).await,
    };
    ExitCode::from(code)
}
