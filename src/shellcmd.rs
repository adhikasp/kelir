//! How a command reaches the shell: as an argument, or as a file.
//!
//! `bash -c <cmd>` is a good and cheap way to run one command — until the
//! command is long. On Windows, Git for Windows' `bash.exe` receives a command
//! line past about 8,191 characters **cut**, and the cut is silent: a heredoc
//! simply writes less, a shell script stops mid-statement, and the exit code is
//! often 0. Two dogfood runs lost steps to it (M5-A seq 344/399/1295, STEER-4
//! seq 613: a 9 KB heredoc wrote an 8,118-byte file and reported success), and
//! the model never found the cause.
//!
//! So a long command is written to a file next to the call's output and handed
//! to the shell by name (`bash <dir>/cmd.sh`, `cmd.exe <dir>/cmd.bat`). The file
//! is not a temporary: it lives in the call's own directory, so the exact bytes
//! that ran stay readable afterwards, which is what makes a truncated result
//! diagnosable at all.
//!
//! The threshold is deliberate and cross-platform: a command over
//! [`SHELL_CMD_MAX`] takes the file path on **every** OS, so the path is
//! exercised by the whole suite on Linux and on Windows both, and a command
//! under it keeps the short, well-tested argument path.

use std::path::{Path, PathBuf};

/// A command longer than this goes through a file instead of `-c`.
///
/// The cut is at 8,191 characters (the Windows command-line limit after the
/// program name and quoting), and it is a *cliff*, not a slope: 8,160 works,
/// 8,190 is cut. 6,000 leaves room for the shell's own argv and for the
/// argument-count limit a caller's command line already spent.
pub const SHELL_CMD_MAX: usize = 6_000;

/// The file a long command is written to, inside the directory kelir owns for
/// the call (`<session>/commands/<id>/`, or `bg/` for a job).
pub const SCRIPT_NAME_UNIX: &str = "cmd.sh";
pub const SCRIPT_NAME_CMD: &str = "cmd.bat";

/// What the shim hands the shell.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellInput {
    /// `shell <shell_args…> <command>`: unchanged behaviour, short commands.
    Argument(String),
    /// `shell <path>`: the command was written to `path`, and no `-c` is passed.
    Script(PathBuf),
}

/// Does this shell take the command as the argument after `-c`/`/C`? That is
/// the only form the length limit applies to; a shell whose argv is something
/// else is left exactly as configured.
fn command_switch(shell_args: &[String]) -> Option<&str> {
    let last = shell_args.last()?.as_str();
    match last {
        "-c" | "/C" | "/c" | "-Command" | "-command" => Some(last),
        _ => None,
    }
}

/// The name of the script a long command is written to, by shell.
fn script_name(shell_args: &[String]) -> &'static str {
    match command_switch(shell_args) {
        Some("/C") | Some("/c") => SCRIPT_NAME_CMD,
        _ => SCRIPT_NAME_UNIX,
    }
}

/// Write `cmd` to `dir/<cmd.sh|cmd.bat>` and return the path. Unix shells get
/// LF endings (a CRLF `cmd.sh` breaks a heredoc terminator); `cmd.exe` gets
/// CRLF, which is what its parser expects.
pub fn write_script(cmd: &str, shell_args: &[String], dir: &Path) -> Result<PathBuf, String> {
    let path = dir.join(script_name(shell_args));
    let mut body = cmd.replace("\r\n", "\n");
    if !body.ends_with('\n') {
        body.push('\n');
    }
    if script_name(shell_args) == SCRIPT_NAME_CMD {
        body = body.replace('\n', "\r\n");
    }
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    std::fs::write(&path, body.as_bytes())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    Ok(path)
}

/// The decision, and the write when the answer is a file. `dir` must exist or
/// be creatable; it is the call's own directory, never a temp dir.
pub fn input(cmd: &str, shell_args: &[String], dir: &Path) -> Result<ShellInput, String> {
    if command_switch(shell_args).is_none() {
        return Ok(ShellInput::Argument(cmd.to_string()));
    }
    if cmd.len() <= SHELL_CMD_MAX {
        return Ok(ShellInput::Argument(cmd.to_string()));
    }
    Ok(ShellInput::Script(write_script(cmd, shell_args, dir)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash() -> Vec<String> {
        vec!["-c".to_string()]
    }

    fn cmd_exe() -> Vec<String> {
        vec!["/C".to_string()]
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kelir-shellcmd-{}-{name}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_short_command_stays_an_argument() {
        let dir = temp("short");
        assert_eq!(
            input("ls -la", &bash(), &dir).unwrap(),
            ShellInput::Argument("ls -la".into())
        );
        assert!(!dir.join(SCRIPT_NAME_UNIX).exists());
    }

    #[test]
    fn exactly_at_the_threshold_is_still_an_argument() {
        let dir = temp("at");
        let cmd = "x".repeat(SHELL_CMD_MAX);
        assert_eq!(
            input(&cmd, &bash(), &dir).unwrap(),
            ShellInput::Argument(cmd.clone())
        );
    }

    #[test]
    fn one_byte_past_the_threshold_is_a_file() {
        let dir = temp("past");
        let cmd = "x".repeat(SHELL_CMD_MAX + 1);
        match input(&cmd, &bash(), &dir).unwrap() {
            ShellInput::Script(path) => {
                assert_eq!(path, dir.join(SCRIPT_NAME_UNIX));
                let body = std::fs::read_to_string(&path).unwrap();
                assert_eq!(body, format!("{cmd}\n"));
            }
            other => panic!("expected a script, got {other:?}"),
        }
    }

    #[test]
    fn a_shell_that_takes_no_command_switch_is_never_rewritten() {
        let dir = temp("other");
        let args = vec!["--eval".to_string()];
        let cmd = "x".repeat(SHELL_CMD_MAX + 100);
        assert_eq!(
            input(&cmd, &args, &dir).unwrap(),
            ShellInput::Argument(cmd.clone())
        );
    }

    #[test]
    fn cmd_exe_gets_a_bat_with_crlf() {
        let dir = temp("bat");
        let cmd = format!("echo {}\r\necho done", "y".repeat(SHELL_CMD_MAX));
        match input(&cmd, &cmd_exe(), &dir).unwrap() {
            ShellInput::Script(path) => {
                assert_eq!(path, dir.join(SCRIPT_NAME_CMD));
                let body = std::fs::read(&path).unwrap();
                assert_eq!(
                    body,
                    format!("echo {}\r\necho done\r\n", "y".repeat(SHELL_CMD_MAX)).as_bytes()
                );
                assert!(!String::from_utf8_lossy(&body).contains("\n\n"));
            }
            other => panic!("expected a script, got {other:?}"),
        }
    }

    #[test]
    fn a_unix_script_never_gets_crlf() {
        let dir = temp("crlf");
        let cmd = format!("echo a\r\necho {}\r\n", "z".repeat(SHELL_CMD_MAX));
        match input(&cmd, &bash(), &dir).unwrap() {
            ShellInput::Script(path) => {
                let body = std::fs::read(&path).unwrap();
                assert!(
                    !body.windows(2).any(|pair| pair == b"\r\n"),
                    "a cmd.sh with CRLF breaks a heredoc terminator"
                );
            }
            other => panic!("expected a script, got {other:?}"),
        }
    }

    #[test]
    fn the_script_round_trips_a_40_kb_heredoc_byte_for_byte() {
        let dir = temp("heredoc");
        let mut cmd = String::from("cat > big.txt <<'RUST'\n");
        for line in 0..1_200 {
            cmd.push_str(&format!("line {line} of a file written by a heredoc\n"));
        }
        cmd.push_str("RUST\nwc -l < big.txt\n");
        assert!(cmd.len() > 40_000, "the fixture is {} bytes", cmd.len());
        match input(&cmd, &bash(), &dir).unwrap() {
            ShellInput::Script(path) => {
                assert_eq!(std::fs::read_to_string(&path).unwrap(), cmd);
            }
            other => panic!("expected a script, got {other:?}"),
        }
    }
}
