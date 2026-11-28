use clap::Parser;
use std::process::ExitCode;

use kelir::main_cli::{Cli, Command, LsArgs};

/// Windows: stop children (and their children) from inheriting this process's own
/// stdout/stderr handles.
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
    // Only the interactive session can ask for a fresh process. The sentinel
    // shares its number with `kelir run --single-flight`'s "skipped, another run
    // is in flight" (both are `EX_TEMPFAIL`), and a job that re-execed itself on
    // every skip would never finish: the check is on the command, not the code.
    let interactive = matches!(cli.command, None | Some(Command::Tui(_)));
    let code = run(cli).await;
    if interactive && code == kelir::core::session::EXIT_RESTART {
        return relaunch();
    }
    ExitCode::from(code)
}

/// Start a fresh `kelir` in this process's place, with the same arguments minus
/// `--resume`. The new process gets this terminal, this working directory and
/// this environment; nothing is kept in memory.
///
/// On Unix the new session takes this process over with `exec`: one process, one
/// owner of the terminal, so a `/new` leaves nothing behind and the terminal is
/// never held by a parent that is waiting. Windows has no `exec`, so there the
/// parent waits for the child and returns its code.
fn relaunch() -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            eprintln!("kelir: cannot find this binary to start a new session: {error}");
            return ExitCode::FAILURE;
        }
    };
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let mut command = kelir::sys::runner::std_command(&exe);
    command.args(without_resume(&args));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let error = command.exec();
        eprintln!("kelir: cannot start a new session: {error}");
        ExitCode::FAILURE
    }
    #[cfg(not(unix))]
    match command.status() {
        Ok(status) => status.code().map_or(ExitCode::FAILURE, |code| {
            ExitCode::from((code & 0xff) as u8)
        }),
        Err(error) => {
            eprintln!("kelir: cannot start a new session: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `args` without `--resume <id>` and `--resume=<id>`.
fn without_resume(args: &[std::ffi::OsString]) -> Vec<std::ffi::OsString> {
    let mut out = Vec::with_capacity(args.len());
    let mut skip = false;
    for arg in args {
        let text = arg.to_string_lossy();
        if skip {
            skip = false;
            continue;
        }
        if text == "--resume" || text == "-r" {
            skip = true;
            continue;
        }
        if text.starts_with("--resume=") {
            continue;
        }
        out.push(arg.clone());
    }
    out
}

async fn run(cli: Cli) -> u8 {
    match cli.command {
        Some(Command::Run(args)) => kelir::core::session::run(args).await,
        // Flags before the subcommand fill in what it did not set.
        Some(Command::Tui(args)) => kelir::core::session::tui(args.with_fallback(cli.tui)).await,
        Some(Command::Ctl(args)) => kelir::ipc::ctl::ctl(args).await,
        Some(Command::Attach(args)) => kelir::ipc::attach::run(args).await,
        Some(Command::View(args)) => kelir::core::session::view(args).await,
        Some(Command::Observe(args)) => kelir::ipc::observe::cli(&args),
        Some(Command::Bg(args)) => kelir::features::bg::cli(args.command).await,
        Some(Command::Mcp(args)) => kelir::features::mcp::cli::run(args.words).await,
        Some(Command::Brief(args)) => kelir::features::children::cli::brief(args).await,
        Some(Command::Ls(LsArgs { command, json, all })) => match command {
            Some(kelir::main_cli::LsCommand::Sessions(a)) => {
                kelir::features::names::sessions_ls(&a)
            }
            Some(kelir::main_cli::LsCommand::Rename(a)) => {
                kelir::features::names::sessions_rename(&a)
            }
            Some(kelir::main_cli::LsCommand::Forget(a)) => {
                kelir::features::names::sessions_forget(&a)
            }
            None => {
                kelir::features::children::cli::ls(kelir::main_cli::LsArgs {
                    command: None,
                    json,
                    all,
                })
                .await
            }
        },
        Some(Command::Kill(args)) => kelir::features::children::cli::kill(args).await,
        Some(Command::Channel(args)) => kelir::features::channel::run(args).await,
        Some(Command::ChildHost) => kelir::core::session::child_host().await,
        Some(Command::Doctor(args)) => kelir::ipc::doctor::doctor(args),
        Some(Command::Docs(args)) => kelir::docs::cli(&args),
        Some(Command::BgShim(args)) => kelir::features::bg::shim_main(args).await,
        #[cfg(unix)]
        Some(Command::GateGuard(args)) => {
            kelir::features::gates::reap::guard_main(args.parent, args.group, &args.token, args.ttl)
        }
        #[cfg(not(unix))]
        Some(Command::GateGuard(_)) => 0,
        // `kelir` with no subcommand is the interactive session.
        None => kelir::core::session::tui(cli.tui).await,
    }
}
