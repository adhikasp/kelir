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
//! There is a second, quieter problem with the same root, and it has no
//! threshold: on Windows the command crosses two argument parsers (the Win32
//! command line, then msys' argv rewriting for `bash.exe`), and every run of
//! backslashes comes out **halved**: `a\\b` arrives as `a\b`, `printf 'a\\nb'`
//! prints a newline, and a regex or a Windows path in a short command means
//! something else by the time the shell sees it (STEER-6 review S1, reproduced
//! on the baseline binary).
//!
//! So on Windows the command **always** travels as a file: it is written to
//! `<call dir>/cmd.sh` (`cmd.bat` for `cmd.exe`, `cmd.ps1` for PowerShell) and
//! the shell is handed the path. One route, no threshold, so a command's meaning
//! cannot change at a length boundary — backslashes, quotes, `$`, unicode and
//! CRLF terminators survive byte-exactly at every length, and the file that ran
//! stays readable in the call's own directory, which is what makes a truncated
//! result diagnosable at all.
//!
//! On Unix the argument route has no msys layer, so a short command keeps the
//! cheaper `-c`; the file route there is the same code, exercised by the same
//! tests, and it is what a long command has always used.
//!
//! The file is not a temporary. It lives in the call's own directory, and the
//! last one written for a call is the one that ran.

use std::path::{Path, PathBuf};

/// A command longer than this goes through a file instead of `-c` on Unix too.
///
/// The cut is at 8,191 characters (the Windows command-line limit after the
/// program name and quoting), and it is a *cliff*, not a slope: 8,160 works,
/// 8,190 is cut. 6,000 leaves room for the shell's own argv and for the
/// argument-count limit a caller's command line already spent. On Windows this
/// threshold no longer decides anything (every command goes through a file);
/// it is kept because `bg run`'s own argv and the Unix path still use it.
pub const SHELL_CMD_MAX: usize = 6_000;

/// The file a command is written to, inside the directory kelir owns for the
/// call (`<session>/commands/<id>/`, or `bg/` for a job). A job prefixes the
/// name with its own id (`bg/<id>.sh`), because one directory holds them all.
pub const SCRIPT_EXT_UNIX: &str = ".sh";
pub const SCRIPT_EXT_CMD: &str = ".bat";
pub const SCRIPT_EXT_PS1: &str = ".ps1";
pub const SCRIPT_NAME_UNIX: &str = "cmd.sh";
pub const SCRIPT_NAME_CMD: &str = "cmd.bat";
pub const SCRIPT_NAME_PS1: &str = "cmd.ps1";

/// What the shim hands the shell.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellInput {
    /// `shell <shell_args…> <command>`: the short Unix route.
    Argument(String),
    /// `shell <shell_args minus the switch…> <path>`: the command was written
    /// to `path`. `keep_switch` is set for a shell that needs its switch even
    /// with a file (`cmd.exe /C <script>`, `pwsh -File <script>`): dropping it
    /// starts an interactive shell that runs nothing and exits 0 (STEER-6
    /// review B1).
    Script {
        path: PathBuf,
        keep_switch: Option<String>,
    },
}

/// Which shell family a `shell` argv belongs to: the switch it uses to take a
/// command, and therefore how a command becomes a file for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// `-c`: `bash`, `sh`, `zsh`. The file is run as a script, no switch.
    Posix,
    /// `/C`: `cmd.exe`. The switch stays: `cmd.exe /C <script>`, else it opens
    /// an interactive shell and executes nothing.
    CmdExe,
    /// `-Command`: `pwsh`/`powershell`. Becomes `-File <script>`; `-Command
    /// <path>` would treat the path as PowerShell source (STEER-6 review N2).
    PowerShell,
}

/// The switch a shell takes its command with, when kelir knows the shell well
/// enough to rewrite the call; `None` leaves the configured argv exactly as it
/// is (a shell whose argv is something else is not kelir's to reinterpret).
fn command_switch(shell_args: &[String]) -> Option<&str> {
    let last = shell_args.last()?.as_str();
    match last {
        "-c" => Some("-c"),
        "/C" => Some("/C"),
        "/c" => Some("/c"),
        "-Command" => Some("-Command"),
        "-command" => Some("-command"),
        _ => None,
    }
}

fn family(shell_args: &[String]) -> Option<Family> {
    match command_switch(shell_args) {
        Some("-c") => Some(Family::Posix),
        Some("/C") | Some("/c") => Some(Family::CmdExe),
        Some("-Command") | Some("-command") => Some(Family::PowerShell),
        _ => None,
    }
}

/// The name of the script a command is written to, by shell, for a call (whose
/// directory is its own) or a job (`bg/<id>.cmd.sh`, so two jobs running at
/// once cannot overwrite each other's script).
pub fn script_name(shell_args: &[String], stem: &str) -> String {
    let ext = match family(shell_args) {
        Some(Family::CmdExe) => SCRIPT_EXT_CMD,
        Some(Family::PowerShell) => SCRIPT_EXT_PS1,
        Some(Family::Posix) | None => SCRIPT_EXT_UNIX,
    };
    if stem.is_empty() {
        return format!("cmd{ext}");
    }
    format!("{stem}{ext}")
}

/// Every shell goes through a file on Windows (see the module docs): one route,
/// no length threshold, no msys argv rewriting to halve backslashes.
pub fn always_script() -> bool {
    cfg!(windows)
}

/// Write `cmd` to `dir/<cmd.sh|cmd.bat|cmd.ps1>` and return the path, with the
/// shell args the shim should run (§[`ShellInput::Script`]).
///
/// Line endings, and the one documented normalisation (STEER-6 review N4):
/// POSIX shells get LF — a CRLF `cmd.sh` puts a `\r` at the end of every line
/// and breaks a heredoc terminator outright — so the command's CRLF line
/// terminators become LF. `cmd.exe` and PowerShell get CRLF, which is what
/// their parsers expect. Other control characters, including a lone `\r`
/// inside a heredoc payload (the way to write a literal CR to a file), are
/// untouched. `KELIR_SHELL_RAW=cr` sends the POSIX script byte-for-byte instead
/// (CRLF terminators and all), for the rare caller that wants the raw bytes.
pub fn write_script(
    cmd: &str,
    shell_args: &[String],
    dir: &Path,
    stem: &str,
) -> Result<ShellInput, String> {
    let name = script_name(shell_args, stem);
    let path = dir.join(&name);
    let body = script_body(cmd, family(shell_args) == Some(Family::Posix), raw_cr());
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    std::fs::write(&path, body.as_bytes())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    Ok(ShellInput::Script {
        path,
        keep_switch: match family(shell_args) {
            Some(Family::CmdExe) => command_switch(shell_args).map(str::to_string),
            Some(Family::PowerShell) => Some("-File".to_string()),
            Some(Family::Posix) | None => None,
        },
    })
}

/// `KELIR_SHELL_RAW=cr`: send a POSIX script raw, keeping CRLF terminators.
fn raw_cr() -> bool {
    std::env::var_os("KELIR_SHELL_RAW").is_some_and(|value| value == "cr")
}

/// The bytes a script file gets from a command (see [`write_script`]): LF for a
/// POSIX shell unless `raw_cr`, CRLF for `cmd.exe`/PowerShell, and always one
/// trailing terminator.
pub fn script_body(cmd: &str, posix: bool, raw_cr: bool) -> String {
    let body = if posix && !raw_cr {
        cmd.replace("\r\n", "\n")
    } else {
        cmd.to_string()
    };
    let mut body = body;
    if !body.ends_with('\n') {
        body.push('\n');
    }
    if !posix {
        body = body.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    body
}

/// The decision, and the write when the answer is a file. `dir` must exist or
/// be creatable; it is the call's own directory, never a temp dir.
///
/// The rule: a shell kelir knows how to hand a file to takes the file route
/// when the command is long (`SHELL_CMD_MAX`) or when the platform is Windows
/// (always, because of the msys backslash rewriting); otherwise the command is
/// the shell's last argument, exactly as before.
pub fn input(
    cmd: &str,
    shell_args: &[String],
    dir: &Path,
    stem: &str,
) -> Result<ShellInput, String> {
    decide(cmd, shell_args, dir, stem, always_script())
}

/// [`input`] with the route made explicit, so both branches are testable on
/// every platform (the Windows-only one is otherwise never exercised on Linux
/// and vice versa).
pub fn decide(
    cmd: &str,
    shell_args: &[String],
    dir: &Path,
    stem: &str,
    always_file: bool,
) -> Result<ShellInput, String> {
    if command_switch(shell_args).is_none() {
        return Ok(ShellInput::Argument(cmd.to_string()));
    }
    if !always_file && cmd.len() <= SHELL_CMD_MAX {
        return Ok(ShellInput::Argument(cmd.to_string()));
    }
    // An empty command is not worth a file, and `bash cmd.sh` on a file with
    // one newline is not the same as `bash -c ""` (a trailing blank line is
    // harmless, a file that is only a newline still is); keep the argument.
    if cmd.trim().is_empty() {
        return Ok(ShellInput::Argument(cmd.to_string()));
    }
    write_script(cmd, shell_args, dir, stem)
}

/// The decision for a script a caller already wrote (the bash call and
/// `bg run` write it before the shim exists, to keep the command off the shim's
/// own argv). The switch follows from the shell, not from the file.
pub fn script_input(path: &Path, shell_args: &[String]) -> Result<ShellInput, String> {
    if command_switch(shell_args).is_none() {
        return Err(format!(
            "a script was written for a shell that takes no command switch: {}",
            path.display()
        ));
    }
    let keep_switch = match family(shell_args) {
        Some(Family::CmdExe) => command_switch(shell_args).map(str::to_string),
        Some(Family::PowerShell) => Some("-File".to_string()),
        Some(Family::Posix) | None => None,
    };
    Ok(ShellInput::Script {
        path: path.to_path_buf(),
        keep_switch,
    })
}

/// The argv the shim runs for a script: the shell's own args without the switch,
/// then whatever the family needs, then the path.
pub fn script_args(shell_args: &[String], keep_switch: Option<&str>, path: &Path) -> Vec<String> {
    let mut argv: Vec<String> = shell_args
        .iter()
        .rev()
        .skip(1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .cloned()
        .collect();
    if let Some(switch) = keep_switch {
        argv.push(switch.to_string());
    }
    argv.push(crate::util::path_string(path));
    argv
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

    fn pwsh() -> Vec<String> {
        vec!["-NoProfile".to_string(), "-Command".to_string()]
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kelir-shellcmd-{}-{name}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn on_unix_a_short_command_stays_an_argument() {
        let dir = temp("short");
        assert_eq!(
            decide("ls -la", &bash(), &dir, "", false).unwrap(),
            ShellInput::Argument("ls -la".into())
        );
        assert!(!dir.join(SCRIPT_NAME_UNIX).exists());
    }

    #[test]
    fn the_platform_route_is_one_route_on_windows_and_cheap_on_unix() {
        let cmd = "printf 'a\\\\b'";
        let dir = temp("route");
        let input = input(cmd, &bash(), &dir, "").unwrap();
        if always_script() {
            match input {
                ShellInput::Script { path, keep_switch } => {
                    assert_eq!(path, dir.join(SCRIPT_NAME_UNIX));
                    assert_eq!(keep_switch, None, "a POSIX shell reads its argv");
                    assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{cmd}\n"));
                }
                other => panic!("Windows must always hand the shell a file, got {other:?}"),
            }
        } else {
            assert_eq!(input, ShellInput::Argument(cmd.to_string()));
        }
    }

    #[test]
    fn exactly_at_the_threshold_is_still_an_argument_on_unix() {
        let dir = temp("at");
        let cmd = "x".repeat(SHELL_CMD_MAX);
        assert_eq!(
            decide(&cmd, &bash(), &dir, "", false).unwrap(),
            ShellInput::Argument(cmd.clone())
        );
    }

    #[test]
    fn one_byte_past_the_threshold_is_a_file() {
        let dir = temp("past");
        let cmd = "x".repeat(SHELL_CMD_MAX + 1);
        match decide(&cmd, &bash(), &dir, "", false).unwrap() {
            ShellInput::Script { path, keep_switch } => {
                assert_eq!(path, dir.join(SCRIPT_NAME_UNIX));
                assert_eq!(keep_switch, None);
                let body = std::fs::read_to_string(&path).unwrap();
                assert_eq!(body, format!("{cmd}\n"));
            }
            other => panic!("expected a script, got {other:?}"),
        }
    }

    /// The property the whole route exists for: at **every** length, with
    /// backslash runs, quotes, `$`, unicode and `%`, the bytes the shell will
    /// read are the bytes the model sent (line endings aside on POSIX, where a
    /// CRLF terminator is normalised to LF — see the module docs).
    #[test]
    fn every_length_keeps_the_command_byte_exact() {
        // Backslash runs, quotes, `$`, backticks, unicode and `%`: exactly what
        // the Windows argument route alters. The transform is pure, so the
        // whole range is cheap and both platforms check the same bytes.
        let payload = "a\\b \\\\ c:\\dir $HOME `x` \"q\" 'r' é 日本語 🎉 %PATH% !b";
        for length in 1..=9_000usize {
            // A backslash run that grows and shrinks with the length, so the
            // payload is never all filler; characters go in whole, because a
            // byte offset would split a unicode character.
            let slashes = "\\".repeat(1 + (length % 4));
            let mut cmd = String::new();
            for ch in slashes.chars() {
                if cmd.len() + ch.len_utf8() > length {
                    break;
                }
                cmd.push(ch);
            }
            for ch in payload.chars() {
                if cmd.len() + ch.len_utf8() > length {
                    break;
                }
                cmd.push(ch);
            }
            while cmd.len() < length {
                cmd.push('y');
            }
            assert_eq!(cmd.len(), length, "the fixture is exact");
            let body = script_body(&cmd, true, false);
            let expected = if cmd.ends_with('\n') {
                cmd.clone()
            } else {
                format!("{cmd}\n")
            };
            // The writer terminates the last line and does nothing else: no
            // CRLF rewriting inside a payload, no backslash touched.
            assert_eq!(body, expected, "length {length} was altered");
            if cmd.contains("\\r\\n") {
                assert!(!body.contains("\\r\\n"), "length {length}: CRLF kept");
            }
            // `cmd.exe` gets the same command with CRLF terminators, and the
            // payload's own bytes otherwise.
            let bat = script_body(&cmd, false, false);
            assert_eq!(
                bat.replace("\r\n", "\n"),
                expected,
                "length {length}: the .bat differs when CRLF is folded"
            );
            assert!(!bat.contains("\r\r"));
            assert!(!bat.contains("\n\n"));
        }
    }

    #[test]
    fn a_shell_that_takes_no_command_switch_is_never_rewritten() {
        let dir = temp("other");
        let args = vec!["--eval".to_string()];
        let cmd = "x".repeat(SHELL_CMD_MAX + 100);
        assert_eq!(
            decide(&cmd, &args, &dir, "", true).unwrap(),
            ShellInput::Argument(cmd.clone())
        );
    }

    /// B1: `cmd.exe` without `/C` starts an interactive shell that runs nothing
    /// and exits 0. The switch has to survive the file route.
    #[test]
    fn cmd_exe_keeps_its_c_on_the_file_route() {
        let dir = temp("bat");
        let cmd = format!("echo {}\r\necho done", "y".repeat(SHELL_CMD_MAX));
        match decide(&cmd, &cmd_exe(), &dir, "", true).unwrap() {
            ShellInput::Script { path, keep_switch } => {
                assert_eq!(path, dir.join(SCRIPT_NAME_CMD));
                assert_eq!(keep_switch.as_deref(), Some("/C"));
                let body = std::fs::read(&path).unwrap();
                assert_eq!(
                    body,
                    format!("echo {}\r\necho done\r\n", "y".repeat(SHELL_CMD_MAX)).as_bytes()
                );
                assert!(!String::from_utf8_lossy(&body).contains("\n\n"));
            }
            other => panic!("expected a script, got {other:?}"),
        }
        // The argv the shim builds: the switch, then the path.
        let argv = script_args(&cmd_exe(), Some("/C"), &dir.join(SCRIPT_NAME_CMD));
        assert_eq!(
            argv,
            vec![
                "/C".to_string(),
                crate::util::path_string(&dir.join(SCRIPT_NAME_CMD))
            ]
        );
    }

    /// B1 with the switch other than last: `cmd.exe /D /C cmd.bat`.
    #[test]
    fn cmd_exe_keeps_the_args_before_its_switch() {
        let args = vec!["/D".to_string(), "/C".to_string()];
        let argv = script_args(&args, Some("/C"), Path::new("C:/x/cmd.bat"));
        assert_eq!(
            argv,
            vec!["/D".to_string(), "/C".into(), "C:/x/cmd.bat".into()]
        );
    }

    /// N2: PowerShell gets `-File`, not `-Command <path>` (which would run the
    /// path as PowerShell source) and not nothing (which reads stdin).
    #[test]
    fn powershell_gets_minus_file_and_a_ps1() {
        let dir = temp("ps1");
        match decide("Write-Output hi", &pwsh(), &dir, "", true).unwrap() {
            ShellInput::Script { path, keep_switch } => {
                assert_eq!(path, dir.join(SCRIPT_NAME_PS1));
                assert_eq!(keep_switch.as_deref(), Some("-File"));
                assert_eq!(
                    std::fs::read_to_string(&path).unwrap(),
                    "Write-Output hi\r\n",
                    "PowerShell wants CRLF and gets one terminator"
                );
            }
            other => panic!("expected a script, got {other:?}"),
        }
        let argv = script_args(&pwsh(), Some("-File"), Path::new("C:/x/cmd.ps1"));
        assert_eq!(
            argv,
            vec![
                "-NoProfile".to_string(),
                "-File".into(),
                "C:/x/cmd.ps1".into()
            ]
        );
    }

    #[test]
    fn a_unix_script_never_gets_crlf() {
        let dir = temp("crlf");
        let cmd = format!("echo a\r\necho {}\r\n", "z".repeat(SHELL_CMD_MAX));
        match decide(&cmd, &bash(), &dir, "", true).unwrap() {
            ShellInput::Script { path, .. } => {
                let body = std::fs::read(&path).unwrap();
                assert!(
                    !body.windows(2).any(|pair| pair == b"\r\n"),
                    "a cmd.sh with CRLF breaks a heredoc terminator"
                );
            }
            other => panic!("expected a script, got {other:?}"),
        }
    }

    /// N4: the normalisation is CRLF terminators only. A lone CR in a payload
    /// (how a heredoc writes a literal CR to a file) survives.
    #[test]
    fn a_lone_cr_in_a_payload_survives_the_unix_route() {
        let dir = temp("lonecr");
        let cmd = "printf 'a\rb' > cr.txt\n".to_string();
        match decide(&cmd, &bash(), &dir, "", true).unwrap() {
            ShellInput::Script { path, .. } => {
                assert_eq!(std::fs::read_to_string(&path).unwrap(), cmd);
            }
            other => panic!("expected a script, got {other:?}"),
        }
    }

    #[test]
    fn a_job_script_is_named_after_its_job() {
        assert_eq!(script_name(&bash(), "k1a"), "k1a.sh");
        assert_eq!(script_name(&cmd_exe(), "k1a"), "k1a.bat");
        assert_eq!(script_name(&pwsh(), "k1a"), "k1a.ps1");
        assert_eq!(script_name(&bash(), ""), SCRIPT_NAME_UNIX);
        let dir = temp("job");
        match decide("echo hi", &bash(), &dir, "k1a", true).unwrap() {
            ShellInput::Script { path, .. } => assert_eq!(path, dir.join("k1a.sh")),
            other => panic!("expected a script, got {other:?}"),
        }
    }

    #[test]
    fn a_script_a_caller_wrote_gets_the_switch_its_shell_needs() {
        let path = Path::new("C:/x/cmd.bat");
        match script_input(path, &cmd_exe()).unwrap() {
            ShellInput::Script {
                path: got,
                keep_switch,
            } => {
                assert_eq!(got, path);
                assert_eq!(keep_switch.as_deref(), Some("/C"));
            }
            other => panic!("expected a script, got {other:?}"),
        }
        match script_input(Path::new("/x/cmd.sh"), &bash()).unwrap() {
            ShellInput::Script { keep_switch, .. } => assert_eq!(keep_switch, None),
            other => panic!("expected a script, got {other:?}"),
        }
        assert!(script_input(Path::new("/x/a"), &["--eval".to_string()]).is_err());
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
        match decide(&cmd, &bash(), &dir, "", false).unwrap() {
            ShellInput::Script { path, .. } => {
                assert_eq!(std::fs::read_to_string(&path).unwrap(), cmd);
            }
            other => panic!("expected a script, got {other:?}"),
        }
    }
}
