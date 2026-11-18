use clap::Parser;
use std::process::ExitCode;

use kelir::cli::{Cli, Command, TuiArgs};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = match cli.command {
        Some(Command::Run(args)) => kelir::session::run(args).await,
        Some(Command::Ask(args)) => kelir::session::ask(args).await,
        Some(Command::Tui(args)) => kelir::session::tui(args).await,
        Some(Command::Ctl(args)) => kelir::ctl::ctl(args).await,
        // `kelir` with no subcommand is the interactive session.
        None => kelir::session::tui(TuiArgs::default()).await,
    };
    ExitCode::from(code)
}
