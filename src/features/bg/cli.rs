//! Background jobs: the shim, the status files, and the `kelir bg` CLI.
//! A job is one `kelir bg-shim` process (the same binary) that spawns the command, hands it the
//! output file directly and writes `bg/<id>.status` whenever its state changes.
//! The shim owns the command's process tree: on Unix both are in the shim's process group
//! (`setsid` for a job), on Windows the command is assigned to a named Job Object the shim keeps

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::log::event::Payload;
use crate::main_cli::{BgCommand, BgOutArgs, BgRunArgs, BgWaitArgs};
use crate::util::path_string;

use super::shim::kill_tree;
use super::*;

// --------------------------------------------------------------------- the CLI

pub async fn cli(command: BgCommand) -> u8 {
    match command {
        BgCommand::Run(args) => run(args).await,
        BgCommand::Ls => ls(),
        BgCommand::Out(args) => out(args),
        BgCommand::Wait(args) => wait(args).await,
        BgCommand::Cancel(args) => cancel(&args.id).await,
    }
}

async fn run(args: BgRunArgs) -> u8 {
    let command = command_line(&args.command);
    if command.trim().is_empty() {
        eprintln!("kelir: bg run: no command");
        return 2;
    }
    // The same shell the session uses, from the same config.
    let shell = match crate::config::load(None) {
        Ok(cfg) => cfg.shell,
        Err(error) => {
            eprintln!("kelir: bg run: {error}");
            return 2;
        }
    };
    let dir = root();
    // With a session, the session starts the shim and mints everything the job is trusted with (id,
    // socket and token): none of it is ever returned to this process, which is a tool child.
    let id = if let Some(sock) = session_socket() {
        let spawn = SpawnRequest {
            dir: path_string(&dir),
            id: None,
            out: String::new(),
            cmd: command.clone(),
            shell,
            kind: KIND_JOB.to_string(),
            call: crate::envs::var("KELIR_CALL_ID")
                .map(|call| call.trim().to_string())
                .filter(|call| !call.is_empty()),
            job: None,
            sock: Some(sock.clone()),
            token: crate::sys::trust::token_from_env(),
            cwd: std::env::current_dir().ok().map(|cwd| path_string(&cwd)),
            env: crate::envs::all(),
        };
        match spawned_by_session(&sock, &spawn).await {
            Ok(id) => id,
            Err(error) => {
                eprintln!("kelir: bg run: {error}");
                return 1;
            }
        }
    } else {
        let id = new_id();
        let out = out_path(&dir, &id);
        let shell_args: Vec<String> = shell.iter().skip(1).cloned().collect();
        let script_dir = job_dir(&dir, &id);
        let cmd_file = match crate::sys::shell::input(&command, &shell_args, &script_dir, &id) {
            Ok(crate::sys::shell::ShellInput::Argument(_)) => None,
            Ok(crate::sys::shell::ShellInput::Script { path, .. }) => Some(path),
            Err(error) => {
                eprintln!("kelir: bg run: {error}");
                return 1;
            }
        };
        let launch = Launch {
            dir: dir.clone(),
            id: id.clone(),
            out,
            cmd: command.clone(),
            cmd_file,
            shell,
            kind: KIND_JOB,
            // A job belongs to nobody: it outlives whatever started it.
            owner: None,
            call: None,
            job: Some(job_name("job", &id)),
            sock: None,
            // No session: the global `bg/` is not under one, so pass any session dir explicitly.
            session_dir: crate::envs::var_os("KELIR_SESSION_DIR")
                .map(PathBuf::from)
                .filter(|dir| !dir.as_os_str().is_empty()),
            token: None,
            env: None,
            cwd: std::env::current_dir().ok(),
        };
        if let Err(error) = start_shim(&launch).await {
            eprintln!("kelir: bg run: {error}");
            return 1;
        }
        id
    };

    // Wait for the shim to say the command is running (or already failed), so
    // the id and the pid are real when the model reads them.
    let status = await_first(&dir, &id, Duration::from_secs(10)).await;
    match status {
        Some(status) => {
            println!(
                "{} {}{}{}",
                status.id,
                status.state,
                status
                    .pid
                    .map(|pid| format!(" pid {pid}"))
                    .unwrap_or_default(),
                status
                    .exit_code
                    .map(|code| format!(" exit {code}"))
                    .unwrap_or_default()
            );
            if let Some(error) = &status.error {
                println!("error: {error}");
            }
            println!("out: {}", status.out);
            println!(
                "read: kelir bg out {} | wait: kelir bg wait {} | cancel: kelir bg cancel {}",
                status.id, status.id, status.id
            );
            if status.error.is_some() { 1 } else { 0 }
        }
        None => {
            eprintln!("kelir: bg run: the job did not start (no status file)");
            1
        }
    }
}

/// Ask the session to start this job's shim. The answer is the job's id and nothing else.
async fn spawned_by_session(sock: &str, spawn: &SpawnRequest) -> Result<String, String> {
    let args = serde_json::to_value(spawn)
        .map_err(|error| format!("cannot serialise the spawn request: {error}"))?;
    let request = crate::ipc::Request::new("bg-spawn", args, "job");
    let answer = crate::ipc::request_value(sock.trim(), &request)
        .await
        .map_err(|error| format!("cannot reach the session: {error}"))?;
    if !answer.ok {
        return Err(answer.error.unwrap_or_else(|| "the session refused".into()));
    }
    answer
        .state
        .as_ref()
        .and_then(|state| state.get("id"))
        .and_then(|id| id.as_str())
        .map(str::to_string)
        .ok_or_else(|| "the session answered without a job id".to_string())
}

/// The session socket this process inherited, if any.
fn session_socket() -> Option<String> {
    crate::envs::var("KELIR_SOCK")
        .map(|sock| sock.trim().to_string())
        .filter(|sock| !sock.is_empty())
}

/// noticing the file on its next poll.
pub(crate) async fn notify(status: &Status, sock: &str) {
    let args = match serde_json::to_value(status) {
        Ok(args) => args,
        Err(_) => return,
    };
    // `request_value` adds `KELIR_TOKEN` when the request has none: a report without it is refused by the
    // session's socket.
    let request = crate::ipc::Request::new("bg", args, "job");
    let _ = crate::ipc::request_value(sock.trim(), &request).await;
}

async fn await_first(dir: &Path, id: &str, timeout: Duration) -> Option<Status> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = Status::read(&status_path(dir, id)) {
            return Some(status);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The command line the shell is given, from the words on `kelir bg run`'s own command line.
pub fn command_line(words: &[String]) -> String {
    if words.len() == 1 {
        return words[0].clone();
    }
    words
        .iter()
        .map(|word| quote_word(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One word for a POSIX shell: bare when that is unambiguous, single-quoted
/// otherwise (`'` becomes `'\''`).
fn quote_word(word: &str) -> String {
    let safe = !word.is_empty()
        && word.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '_' | '-' | '.' | '/' | ':' | '@' | '%' | '+' | ',' | '='
                )
        });
    if safe {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

fn ls() -> u8 {
    let dir = root();
    let jobs: Vec<Status> = list(&dir)
        .into_iter()
        .filter(|status| status.is_job())
        .collect();
    if jobs.is_empty() {
        println!("no background jobs in {}", path_string(&dir));
        return 0;
    }
    for status in jobs {
        println!(
            "{}  {}  pid {}{}  {}{}",
            status.id,
            status.state,
            status
                .pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".into()),
            status
                .exit_code
                .map(|code| format!(" exit {code}"))
                .unwrap_or_default(),
            crate::util::truncate_chars(status.cmd.trim(), 60),
            if status.settled() {
                format!("  {}", status.finished.clone().unwrap_or_default())
            } else {
                String::new()
            }
        );
    }
    0
}

fn out(args: BgOutArgs) -> u8 {
    let Some((dir, status)) = find(&args.id) else {
        eprintln!("kelir: no background job `{}`", args.id);
        return 2;
    };
    let path = PathBuf::from(&status.out);
    if !args.follow {
        match std::fs::read(&path) {
            Ok(bytes) => {
                print!("{}", String::from_utf8_lossy(&bytes));
                0
            }
            Err(_) => 0,
        }
    } else {
        follow(&path, &status_path(&dir, &status.id), &dir)
    }
}

/// `--follow`: print the file as it grows, and stop once the job settled and nothing new arrived.
fn follow(path: &Path, status_file: &Path, dir: &Path) -> u8 {
    use std::io::{Read as _, Seek as _, Write as _};
    let mut offset = 0u64;
    let mut quiet = 0u32;
    loop {
        let mut wrote = false;
        if let Ok(mut file) = std::fs::File::open(path) {
            // A file that shrank (a truncated or replaced output) starts over
            // instead of seeking past its end and printing nothing for ever.
            if std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0) < offset {
                offset = 0;
            }
            if file.seek(std::io::SeekFrom::Start(offset)).is_ok() {
                let mut bytes = Vec::new();
                if file.read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                    let _ = std::io::stdout().write_all(&bytes);
                    let _ = std::io::stdout().flush();
                    offset += bytes.len() as u64;
                    wrote = true;
                }
            }
        }
        if wrote {
            quiet = 0;
        } else {
            // Once the job settled and the file stopped growing for a few
            // polls, `--follow` is done — like `tail -f` with an end.
            let settled = Status::read(status_file)
                .map(|status| observed_in(dir, &status).settled())
                .unwrap_or(true);
            if settled {
                quiet += 1;
                if quiet > 4 {
                    return 0;
                }
            } else {
                quiet = 0;
            }
        }
        std::thread::sleep(POLL);
    }
}

async fn wait(args: BgWaitArgs) -> u8 {
    let Some((dir, status)) = find(&args.id) else {
        eprintln!("kelir: no background job `{}`", args.id);
        return 2;
    };
    let deadline = args
        .timeout
        .map(|secs| std::time::Instant::now() + Duration::from_secs(secs));
    let mut status = observed_in(&dir, &status);
    loop {
        if status.settled() {
            println!(
                "{} {} exit {}{}{}",
                status.id,
                status.state,
                status
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "-".into()),
                status
                    .elapsed_secs()
                    .map(|secs| format!(" after {secs:.1}s"))
                    .unwrap_or_default(),
                // Why it ended if it did not end on its own (`lost`, a command
                // that never started).
                match status.state.as_str() {
                    STATE_LOST => status
                        .error
                        .as_ref()
                        .map(|error| format!(" ({error})"))
                        .unwrap_or_default(),
                    _ => String::new(),
                }
            );
            return 0;
        }
        if let Some(deadline) = deadline
            && std::time::Instant::now() >= deadline
        {
            eprintln!("kelir: bg wait: {} is still {}", status.id, status.state);
            return 1;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        status = match Status::read(&status_path(&dir, &args.id)) {
            // A shim that died while we waited is not a job that runs for ever
            // — unless a `cancel` is in flight for it.
            Some(status) => observed_in(&dir, &status),
            None => {
                eprintln!("kelir: bg wait: the status file of {} vanished", args.id);
                return 1;
            }
        };
    }
}

/// `cancel`: take the job's whole tree down and record it.
pub async fn cancel(id: &str) -> u8 {
    let Some((dir, _status)) = find(id) else {
        eprintln!("kelir: no background job `{id}`");
        return 2;
    };
    let mut out = Console;
    cancel_at(&dir, id, &mut out).await
}

/// Where `cancel`'s sentences go: a human's terminal (`kelir bg cancel`), or the
/// session's reply (`cancel` as a verb,).
pub(crate) trait CancelOut {
    fn line(&mut self, text: String);
    fn fail(&mut self, text: String);
}

struct Console;

impl CancelOut for Console {
    fn line(&mut self, text: String) {
        println!("{text}");
    }

    fn fail(&mut self, text: String) {
        eprintln!("kelir: bg cancel: {text}");
    }
}

/// The kill itself, for one job in one job root.
async fn cancel_at(dir: &Path, id: &str, out: &mut dyn CancelOut) -> u8 {
    let Some(status) = Status::read(&status_path(dir, id)) else {
        out.fail(format!("no background job `{id}`"));
        return 2;
    };
    let status = observed_in(dir, &status);
    if status.settled() {
        if status.state == STATE_LOST {
            return stop_lost(&status, out);
        }
        out.line(format!(
            "{} {} (nothing to cancel)",
            status.id, status.state
        ));
        return 0;
    }
    // Only a pid that is provably the shim is signalled.
    if shim_identity(&status) != ShimIdentity::Same {
        out.fail(format!(
            "{}: this platform cannot report process start times, so pid {} cannot be proved to \
             be the job's shim; nothing was signalled",
            status.id,
            status.shim.map(|pid| pid.to_string()).unwrap_or_default()
        ));
        return 1;
    }
    // From here until the `cancelled` status is written the status file still says `running` with a dead
    // shim; any reader in that window would call the job `lost` (the session's watcher did, and the log
    let _marker = CancelMarker::write(dir, &status.id);
    kill_tree(status.shim.or(status.pid).unwrap_or(0), &status.job);
    #[cfg(windows)]
    if let Some(shim) = status.shim {
        crate::sys::proc::win::terminate(shim);
    }
    let deadline = std::time::Instant::now() + CANCEL_WAIT;
    while let Some(shim) = status.shim {
        if !crate::sys::proc::alive(shim) || std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    let mut final_status = Status::read(&status_path(dir, &status.id)).unwrap_or(status.clone());
    if final_status.settled() {
        // It finished on its own before the kill landed; that is the truth.
        out.line(format!(
            "{} {} (it had already finished)",
            final_status.id, final_status.state
        ));
        return 0;
    }
    final_status.state = STATE_CANCELLED.into();
    final_status.finished = Some(crate::log::event::now_ts());
    match final_status.write(&status_path(dir, &final_status.id)) {
        Ok(()) => {
            out.line(format!("{} cancelled", final_status.id));
            0
        }
        Err(error) => {
            out.fail(format!("cannot write the status file: {error}"));
            1
        }
    }
}

/// The `cancel` marker, removed when this value goes away: every path out of
/// [`cancel_at`] clears it, including the ones that kill nothing.
pub(crate) struct CancelMarker {
    path: std::path::PathBuf,
}

impl CancelMarker {
    pub(crate) fn write(dir: &Path, id: &str) -> Self {
        let path = cancel_path(dir, id);
        let _ = std::fs::create_dir_all(path.parent().expect("a job dir"));
        let _ = std::fs::write(&path, b"cancelling\n");
        Self { path }
    }
}

impl Drop for CancelMarker {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The session's `cancel` verb: the same kill, in *this session's* `bg/`, with the answer as a sentence
/// (the client's `kelir ctl` prints it).
pub async fn cancel_in(dir: &Path, id: &str) -> Result<String, String> {
    if !crate::sys::trust::valid_job_id(id) {
        return Err(format!("`{id}` is not a job id"));
    }
    if Status::read(&status_path(dir, id)).is_none() {
        return Err(format!("this session has no background job `{id}`"));
    }
    let mut sink = Sink::default();
    let code = cancel_at(dir, id, &mut sink).await;
    if code == 0 {
        Ok(sink.text())
    } else {
        Err(sink.text())
    }
}

/// The text [`cancel`] prints, collected rather than written: the session runs
/// inside a TUI, where stdout is the terminal's.
#[derive(Debug, Default)]
struct Sink {
    lines: std::cell::RefCell<Vec<String>>,
}

impl Sink {
    fn text(&self) -> String {
        self.lines.borrow().join("\n")
    }

    fn push(&self, text: impl Into<String>) {
        self.lines.borrow_mut().push(text.into());
    }
}

impl CancelOut for Sink {
    fn line(&mut self, text: String) {
        self.push(text);
    }

    fn fail(&mut self, text: String) {
        self.push(text);
    }
}

/// A job whose shim is gone is `lost`: its status file will never be written again, but its tree may
/// still be running (the shim was killed with the job mid-flight).
#[cfg(unix)]
fn stop_lost(status: &Status, out: &mut dyn CancelOut) -> u8 {
    let Some(shim) = status.shim.filter(|pid| *pid != 0) else {
        out.fail(format!(
            "{} is lost and records no shim pid; there is nothing to reach its tree by",
            status.id
        ));
        return 1;
    };
    // A live process with the shim's pid that is not the shim is a recycled
    // pid: its group is somebody else's.
    if crate::sys::proc::alive(shim) && shim_identity(status) != ShimIdentity::Same {
        out.fail(format!(
            "{} is lost and pid {shim} now belongs to another process; nothing was signalled",
            status.id
        ));
        return 1;
    }
    match crate::sys::proc::group_members(shim) {
        Some(members) if !members.is_empty() => {
            // The group is led by the dead shim's pid; a pid number cannot be reused while a group with
            // that number still has a member, so this group is the job's tree and nothing else's.
            kill_tree(shim, &status.job);
            out.line(format!(
                "{} lost (its shim is gone; stopped the {} process(es) still in its group)",
                status.id,
                members.len()
            ));
            0
        }
        // Empty, or a platform that cannot list the group.
        Some(_) => {
            out.line(format!("{} lost (nothing to cancel)", status.id));
            0
        }
        None => {
            out.fail(format!(
                "{} is lost and this platform cannot list its process group; nothing was \
                 signalled",
                status.id
            ));
            1
        }
    }
}

#[cfg(windows)]
fn stop_lost(status: &Status, out: &mut dyn CancelOut) -> u8 {
    // First the job object: a live handle somewhere (another process in the
    // tree) keeps the name, and terminating the job is exact.
    if let Some(name) = status.job.as_deref()
        && let Some(job) = crate::sys::proc::win::Job::open(name)
    {
        job.terminate_job();
        out.line(format!(
            "{} lost (its shim is gone; terminated the job object holding its tree)",
            status.id
        ));
        return 0;
    }
    // Then the command's own pid, proved by the start time the shim recorded: the job object's name died
    // with its last handle, but the tree is still there and its parent links say which processes it holds.
    match pid_identity(status) {
        ShimIdentity::Same => {
            let pid = status.pid.unwrap_or(0);
            let killed = crate::sys::proc::terminate_tree(pid);
            out.line(format!(
                "{} lost (its shim is gone; stopped {killed} process(es) from its recorded pid)",
                status.id
            ));
            0
        }
        ShimIdentity::Different => {
            out.fail(format!(
                "{} is lost and its command's pid {} now belongs to another process; nothing was \
                 signalled",
                status.id,
                status.pid.map(|pid| pid.to_string()).unwrap_or_default()
            ));
            1
        }
        ShimIdentity::NoRecord => {
            out.fail(format!(
                "{} is lost and records no command pid and start time, so there is nothing to \
                 prove its tree by; nothing was signalled",
                status.id
            ));
            1
        }
        ShimIdentity::NoPlatform => {
            out.fail(format!(
                "{} is lost and this platform cannot report start times, so its tree cannot be \
                 proved; nothing was signalled",
                status.id
            ));
            1
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn stop_lost(status: &Status, out: &mut dyn CancelOut) -> u8 {
    out.fail(format!(
        "{} is lost and this platform cannot reach a job's tree; nothing was signalled",
        status.id
    ));
    1
}

/// The `call_started`/`call_settled` payloads for a background job, from a status file.
pub fn started_body(status: &Status) -> crate::log::event::EventBody {
    crate::log::event::call_start(
        &status.id,
        "bg",
        crate::log::event::CallInfo::bg(Payload::text(status.cmd.clone())),
    )
}

pub fn settled_body(status: &Status) -> crate::log::event::EventBody {
    crate::log::event::call_settle(
        &status.id,
        "bg",
        "exited",
        crate::log::event::CallSettle::default()
            .exit_code(status.exit_code)
            .state(&status.state)
            .out_path(&status.out),
    )
}
