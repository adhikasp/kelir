use clap::Parser;
use std::process::ExitCode;

use kelir::cli::{Cli, Command};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Run(args) => kelir::session::run(args).await,
        Command::Ask(args) => kelir::session::ask(args).await,
        Command::Ctl(args) => kelir::ctl::ctl(args).await,
    };
    ExitCode::from(code)
}
