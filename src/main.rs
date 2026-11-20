use clap::Parser;
use std::process::ExitCode;

use kelir::cli::{Cli, Command};

/// Windows: stop children (and their children) from inheriting this process's
/// own stdout/stderr handles.
///
/// A background job must not hold the caller's pipe open: with the inherit flag
/// on, `kelir run --json | jq` would stay open until a promoted job exits, even
/// though kelir itself is long gone. `CreateProcess` still hands a child the
/// stdio handles it is asked for (they are made inheritable per spawn), so this
/// takes nothing away from a child that needs one.
#[cfg(windows)]
fn close_handle_inheritance() {
    use windows_sys::Win32::Foundation::{
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        unsafe {
            let handle = GetStdHandle(id);
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

#[cfg(not(windows))]
fn close_handle_inheritance() {}

#[tokio::main]
async fn main() -> ExitCode {
    close_handle_inheritance();
    let cli = Cli::parse();
    let code = match cli.command {
        Some(Command::Run(args)) => kelir::session::run(args).await,
        Some(Command::Ask(args)) => kelir::session::ask(args).await,
        // Flags before the subcommand fill in what it did not set.
        Some(Command::Tui(args)) => kelir::session::tui(args.with_fallback(cli.tui)).await,
        Some(Command::Resume(args)) => kelir::session::resume(args).await,
        Some(Command::Ctl(args)) => kelir::ctl::ctl(args).await,
        Some(Command::View(args)) => kelir::session::view(args).await,
        Some(Command::Observe(args)) => kelir::observe::cli(&args),
        Some(Command::Bg(args)) => kelir::bg::cli(args.command).await,
        Some(Command::Mcp(args)) => kelir::mcp::cli::run(args.words).await,
        Some(Command::Brief(args)) => kelir::childcli::brief(args).await,
        Some(Command::Ls(args)) => kelir::childcli::ls(args).await,
        Some(Command::Kill(args)) => kelir::childcli::kill(args).await,
        Some(Command::Job(args)) => match args.command {
            kelir::cli::JobCommand::Run(args) => kelir::jobrun::run(args).await,
            kelir::cli::JobCommand::Status(args) => kelir::jobrun::status(args),
            kelir::cli::JobCommand::History(args) => kelir::jobrun::history(args),
        },
        Some(Command::Sessions(args)) => match args.command {
            kelir::cli::SessionsCommand::Ls(args) => kelir::names::sessions_ls(&args),
            kelir::cli::SessionsCommand::Rename(args) => kelir::names::sessions_rename(&args),
            kelir::cli::SessionsCommand::Forget(args) => kelir::names::sessions_forget(&args),
        },
        Some(Command::Channel(args)) => kelir::channel::run(args).await,
        Some(Command::ChildHost) => kelir::session::child_host().await,
        Some(Command::Doctor(args)) => kelir::doctor::doctor(args),
        Some(Command::Docs(args)) => kelir::docs::cli(&args),
        Some(Command::BgShim(args)) => kelir::bg::shim_main(args).await,
        #[cfg(unix)]
        Some(Command::GateGuard(args)) => {
            kelir::gates::reap::guard_main(args.parent, args.group, &args.token, args.ttl)
        }
        #[cfg(not(unix))]
        Some(Command::GateGuard(_)) => 0,
        // `kelir` with no subcommand is the interactive session.
        None => kelir::session::tui(cli.tui).await,
    };
    ExitCode::from(code)
}
