//! Background jobs: the shim, the status files, and the `kelir bg` CLI.
//!
//! A job is one `kelir bg-shim` process (the same binary) that spawns the
//! command, hands it the output file directly and writes `bg/<id>.status`
//! whenever its state changes. The shim owns the command's process tree: on
//! Unix both are in the shim's process group (`setsid` for a job), on Windows
//! the command is assigned to a named Job Object the shim keeps open. That is
//! what lets a job outlive the session — and what lets `cancel` take the whole
//! tree down from another process.
//!
//! The status file is written **atomically** (`<id>.status.tmp` then a rename),
//! so a reader never sees half a JSON object, and a torn file on disk (a crash
//! from an older writer, a hand-edited file) is skipped by `ls`/the watcher
//! instead of breaking them.
//!
//! Waking the agent is the session's business: it watches `bg/*.status`, and a
//! job that settles while the session runs becomes a `bg_settled` event plus a
//! `user` message from `job` at the next boundary ([`wake_text`]). A job that
//! settles while nothing runs is folded in by the next `resume`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cli::{BgCommand, BgOutArgs, BgRunArgs, BgShimArgs, BgWaitArgs};
use crate::event::Payload;
use crate::util::path_string;

/// How often the shim looks at its command, its markers and its owner.
pub const POLL: Duration = Duration::from_millis(25);
/// How often the session looks for jobs that changed state.
pub const WATCH: Duration = Duration::from_millis(250);
/// How long a promotion waits for the shim to acknowledge the marker before the
/// session lets go of the tree anyway.
pub const PROMOTE_ACK: Duration = Duration::from_secs(5);
/// How long `cancel` waits for the killed tree to be gone before it records
/// `cancelled`, so the shim cannot write a status after it.
pub const CANCEL_WAIT: Duration = Duration::from_secs(2);

pub const STATE_RUNNING: &str = "running";
pub const STATE_EXITED: &str = "exited";
pub const STATE_CANCELLED: &str = "cancelled";
/// The shim is gone and nothing will ever write this status again (`taskkill
/// /F`, an OOM kill, a reboot). Readers report it instead of a job that runs for
/// ever (M4 review F4); the command's own outcome is unknown.
pub const STATE_LOST: &str = "lost";

/// How far a status file's `started` may be from the process's own start time
/// and still be the same process. Both come from the same clock; the tolerance
/// is for the millisecond between `CreateProcess` and the status write, and for
/// a clock that steps (a VM resume).
pub const START_TOLERANCE: Duration = Duration::from_secs(120);

/// An explicit `kelir bg run` job.
pub const KIND_JOB: &str = "job";
/// A bash call: it has a status file from the moment it starts, and it only
/// becomes a job the model is told about when it is promoted.
pub const KIND_CALL: &str = "call";

/// `bg/<id>.status`: the shim's whole state, in one atomically replaced file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub id: String,
    /// `running`, `exited` or `cancelled`.
    pub state: String,
    /// The command's pid (absent before it started).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// The shim's pid: on Unix it is the process group to signal, on Windows
    /// the process whose job object holds the tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shim: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub started: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<String>,
    #[serde(default)]
    pub cmd: String,
    /// The output file, absolute, forward slashes.
    #[serde(default)]
    pub out: String,
    /// `job` or `call`.
    #[serde(default)]
    pub kind: String,
    /// A promoted call the model was told about.
    #[serde(default)]
    pub promoted: bool,
    /// The provider call id of the bash call that started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    /// The Windows job object, so `cancel` can open it by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    /// Why the command could not be started, when it could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Status {
    pub fn new(id: &str, kind: &str, cmd: &str, out: &Path) -> Self {
        Self {
            id: id.to_string(),
            state: STATE_RUNNING.to_string(),
            pid: None,
            shim: Some(std::process::id()),
            exit_code: None,
            started: crate::event::now_ts(),
            finished: None,
            cmd: cmd.to_string(),
            out: path_string(out),
            kind: kind.to_string(),
            promoted: false,
            call: None,
            job: None,
            error: None,
        }
    }

    pub fn settled(&self) -> bool {
        self.state != STATE_RUNNING
    }

    /// The status as a reader should understand it: a `running` job whose shim
    /// is gone is [`STATE_LOST`], because nothing will ever write it again. The
    /// file on disk is left alone — a reader never writes another process's
    /// status (M4 review F4).
    pub fn observed(&self) -> Status {
        if self.state == STATE_RUNNING && !shim_alive(self) {
            let mut lost = self.clone();
            lost.state = STATE_LOST.into();
            lost.finished = Some(self.finished.clone().unwrap_or_else(crate::event::now_ts));
            lost.error = Some("the job's shim is gone; its outcome is unknown".into());
            return lost;
        }
        self.clone()
    }

    /// A job the model knows about: an explicit one, or a promoted call.
    pub fn is_job(&self) -> bool {
        self.kind == KIND_JOB || self.promoted
    }

    /// How long it ran, when both timestamps parse.
    pub fn elapsed_secs(&self) -> Option<f64> {
        let started = parse_ts(&self.started)?;
        let finished = parse_ts(self.finished.as_deref()?)?;
        let secs = (finished - started).as_seconds_f64();
        Some(secs.max(0.0))
    }

    /// Write the file atomically: a fresh temp file next to it, then a rename
    /// over it. A reader sees the old file or the new one, never half of each.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut tmp = path.as_os_str().to_os_string();
        tmp.push(format!(".{}.tmp", std::process::id()));
        let tmp = PathBuf::from(tmp);
        let text = serde_json::to_string(self).unwrap_or_default();
        std::fs::write(&tmp, text.as_bytes())?;
        // `rename` replaces the destination on both Unix and Windows.
        match std::fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = std::fs::remove_file(&tmp);
                Err(error)
            }
        }
    }

    /// Read one status file. A missing or unparseable file is `None`: a reader
    /// (`ls`, the watcher) skips it instead of failing.
    pub fn read(path: &Path) -> Option<Status> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }
}

fn parse_ts(text: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()
}

/// Is the process named by `status` still the shim that wrote it?
///
/// A status file outlives its shim and its pid can be recycled: before anything
/// signals that pid (or trusts a `running` state), it is compared with when the
/// process really started. When the platform cannot say, plain liveness is the
/// best answer available. A status with no `shim` at all is nobody's.
pub fn shim_alive(status: &Status) -> bool {
    let Some(shim) = status.shim else {
        return false;
    };
    if !process_alive(shim) {
        return false;
    }
    match (parse_ts(&status.started), crate::proc::start_time(shim)) {
        (Some(started), Some(actual)) => {
            let Ok(since) = actual.duration_since(std::time::UNIX_EPOCH) else {
                return false;
            };
            let actual = since.as_secs() as i64;
            (started.unix_timestamp() - actual).unsigned_abs() <= START_TOLERANCE.as_secs()
        }
        _ => true,
    }
}

/// Is a call due to be promoted? `after` zero disables promotion; the caller
/// passes the elapsed time, so the timer is testable without a clock.
pub fn promotion_due(after: Duration, elapsed: Duration) -> bool {
    after > Duration::ZERO && elapsed >= after
}

/// `bg_<ulid>`: unique across processes, and the same shape as a session id.
pub fn new_id() -> String {
    format!("bg_{}", ulid::Ulid::new().to_string().to_lowercase())
}

/// Where jobs live: this session's `bg/` inside a bash call, otherwise the
/// state dir's own `bg/`.
pub fn root() -> PathBuf {
    if let Some(dir) = std::env::var_os("KELIR_SESSION_DIR")
        .map(PathBuf::from)
        .filter(|dir| !dir.as_os_str().is_empty())
    {
        return dir.join("bg");
    }
    crate::paths::state_dir().join("bg")
}

pub fn status_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.status"))
}

pub fn out_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.out"))
}

pub fn promote_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.promote"))
}

/// The name of the Job Object a call's tree lives in. Unique per session and
/// call, so two sessions never share one.
pub fn job_name(session: &str, dir_name: &str) -> String {
    let mut name = String::from("kelir-bg-");
    for ch in session
        .chars()
        .chain(std::iter::once('-'))
        .chain(dir_name.chars())
    {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            name.push(ch);
        } else {
            name.push('_');
        }
    }
    name
}

/// Every status file in `dir`, newest first, skipping temp files and anything
/// that does not parse. A status whose shim is gone comes back as `lost`
/// ([`Status::observed`]), so a listing never shows a job that runs for ever.
pub fn list(dir: &Path) -> Vec<Status> {
    let mut out: Vec<Status> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("status"))
        .filter_map(|path| Status::read(&path))
        .map(|status| status.observed())
        .collect();
    out.sort_by(|a, b| b.started.cmp(&a.started).then(a.id.cmp(&b.id)));
    out
}

/// Find a job by id: the session's own `bg/` first, then any session's (so
/// `kelir bg out <id>` works after kelir exited, with no session env at all).
pub fn find(id: &str) -> Option<(PathBuf, Status)> {
    let id = id.trim();
    if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") {
        return None;
    }
    let dir = root();
    if let Some(status) = Status::read(&status_path(&dir, id)) {
        return Some((dir, status));
    }
    for session in crate::paths::sessions_newest_first() {
        let dir = crate::paths::session_dir(&session).join("bg");
        if let Some(status) = Status::read(&status_path(&dir, id)) {
            return Some((dir, status));
        }
    }
    None
}

/// Ask for a promotion: an empty marker file the shim polls for.
pub fn mark_promoted(dir: &Path, id: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(promote_path(dir, id), b"promoted\n")
}

// ------------------------------------------------------------- how to launch

/// What the session (or `kelir bg run`) tells the shim.
pub struct Launch<'a> {
    /// The `bg/` directory the status file lives in.
    pub dir: PathBuf,
    /// The job id (the call's output dir name, or `bg_<ulid>`).
    pub id: String,
    /// The file the command's stdout and stderr go to.
    pub out: PathBuf,
    /// The command, run by `shell` (`shell[0]` plus its args, then the command).
    pub cmd: String,
    pub shell: Vec<String>,
    pub kind: &'a str,
    /// The session's pid: if it dies before a promotion, the shim takes the
    /// tree down with it.
    pub owner: Option<u32>,
    pub call: Option<String>,
    /// The Windows job object the session created for this call.
    pub job: Option<String>,
    /// The session socket, exported to the command as `KELIR_SOCK`.
    pub sock: Option<String>,
    /// The directory the command runs in. The shim is started there and the
    /// command inherits it, so a `--cwd` session (and a resumed session's
    /// recorded cwd) is the directory its commands see (M4 review F2).
    pub cwd: Option<PathBuf>,
}

/// The command that runs one shim. The caller may add platform grouping flags
/// (`ProcessGroup::configure`) or detach it ([`detach`]).
pub fn shim_command(launch: &Launch) -> Result<tokio::process::Command, String> {
    let binary = std::env::current_exe()
        .map_err(|error| format!("cannot find the kelir binary: {error}"))?;
    let mut command = tokio::process::Command::new(binary);
    command
        .arg("bg-shim")
        .arg("--dir")
        .arg(&launch.dir)
        .arg("--id")
        .arg(&launch.id)
        .arg("--out")
        .arg(&launch.out)
        .arg("--cmd")
        .arg(&launch.cmd)
        .arg("--kind")
        .arg(launch.kind);
    if let Some(shell) = launch.shell.first() {
        command.arg("--shell").arg(shell);
    }
    for arg in launch.shell.iter().skip(1) {
        command.arg("--shell-arg").arg(arg);
    }
    if let Some(owner) = launch.owner {
        command.arg("--owner").arg(owner.to_string());
    }
    if let Some(call) = &launch.call {
        command.arg("--call").arg(call);
    }
    if let Some(job) = &launch.job {
        command.arg("--job").arg(job);
    }
    if let Some(sock) = &launch.sock {
        command.arg("--sock").arg(sock);
    }
    // The command inherits this directory; the shim sets nothing of its own.
    if let Some(cwd) = &launch.cwd {
        command.current_dir(cwd);
    }
    // The shim needs no stdio of its own: the command writes straight to the
    // output file, and errors go into the status file.
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    Ok(command)
}

/// Detach a job's shim from this process, this process group and (on Windows)
/// any enclosing Job Object, so it outlives the session that started it.
#[cfg(unix)]
pub fn detach(command: &mut tokio::process::Command) {
    use std::os::unix::process::CommandExt as _;
    // `setsid` fails if the caller already leads a group; the fork/exec child
    // never does, so this is the spec's `setsid` and nothing else.
    unsafe {
        command.as_std_mut().pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
pub fn detach(command: &mut tokio::process::Command) {
    use std::os::windows::process::CommandExt as _;
    command.as_std_mut().creation_flags(
        crate::proc::win::DETACHED_PROCESS
            | crate::proc::win::CREATE_NEW_PROCESS_GROUP
            | crate::proc::win::CREATE_BREAKAWAY_FROM_JOB,
    );
}

#[cfg(not(any(unix, windows)))]
pub fn detach(command: &mut tokio::process::Command) {
    let _ = command;
}

/// What a `kelir bg run` inside a call asks its session to do on Windows: start
/// the shim **outside** the call's Job Object. The job forbids breakaway — it
/// has to, or the call's tree could not be cancelled (M4 review F1) — so a shim
/// spawned from inside it would be trapped, and a job is supposed to outlive the
/// session. The session is outside the job, so it spawns the shim itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub dir: String,
    pub id: String,
    pub out: String,
    pub cmd: String,
    pub shell: Vec<String>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sock: Option<String>,
    /// The directory the client was in: a job started inside a call runs where
    /// the call does, and the session is not there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

impl SpawnRequest {
    pub fn of(launch: &Launch) -> Self {
        Self {
            dir: path_string(&launch.dir),
            id: launch.id.clone(),
            out: path_string(&launch.out),
            cmd: launch.cmd.clone(),
            shell: launch.shell.clone(),
            kind: launch.kind.to_string(),
            call: launch.call.clone(),
            job: launch.job.clone(),
            sock: launch.sock.clone(),
            cwd: launch.cwd.as_ref().map(|cwd| path_string(cwd)),
        }
    }

    pub fn launch(&self) -> Launch<'_> {
        Launch {
            dir: PathBuf::from(&self.dir),
            id: self.id.clone(),
            out: PathBuf::from(&self.out),
            cmd: self.cmd.clone(),
            shell: self.shell.clone(),
            kind: &self.kind,
            // A job belongs to nobody: it outlives whatever started it.
            owner: None,
            call: self.call.clone(),
            job: self.job.clone(),
            sock: self.sock.clone(),
            cwd: self.cwd.as_ref().map(PathBuf::from),
        }
    }
}

/// Start the shim for `kelir bg run`.
pub async fn start_shim(launch: &Launch<'_>) -> Result<(), String> {
    #[cfg(windows)]
    {
        if let Err(denied) = spawn_shim(launch, true) {
            // The one case that can refuse a detached child: an enclosing Job
            // Object without breakaway, which is a call's job. Ask the session,
            // which is outside it.
            if let Some(sock) = launch.sock.clone().filter(|sock| !sock.trim().is_empty()) {
                let args = serde_json::to_value(SpawnRequest::of(launch))
                    .map_err(|error| format!("cannot serialise the spawn request: {error}"))?;
                let request = crate::ipc::Request::new("bg-spawn", args, "job");
                if let Ok(answer) = crate::ipc::request_value(sock.trim(), &request).await
                    && answer.ok
                {
                    return Ok(());
                }
            }
            // No session to help (a bare shell inside some other job): the job
            // runs inside whatever job the caller had, which is the best that
            // can be done — and the caller's job is not a call's.
            spawn_shim(launch, false).map_err(|error| format!("{error} ({denied})"))?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        spawn_shim(launch, true)
    }
}

/// Start one shim. `escape` asks for a process outside every enclosing job
/// (`setsid` on Unix, `CREATE_BREAKAWAY_FROM_JOB` on Windows); a Job Object that
/// does not allow breakaway refuses it, which is exactly what a call's job does
/// (M4 review F1) and what a harness's may do.
fn spawn_shim(launch: &Launch<'_>, escape: bool) -> Result<(), String> {
    let mut command = shim_command(launch)?;
    #[cfg(unix)]
    {
        let _ = escape;
        detach(&mut command);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        let flags = crate::proc::win::DETACHED_PROCESS | crate::proc::win::CREATE_NEW_PROCESS_GROUP;
        command.as_std_mut().creation_flags(if escape {
            flags | crate::proc::win::CREATE_BREAKAWAY_FROM_JOB
        } else {
            flags
        });
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = escape;
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("cannot start the shim: {error}"))
}

/// The session's half of [`SpawnRequest`]: start the shim for a client that
/// cannot start it itself, and answer at once (`kelir bg run` waits for the
/// status file; the session must not block on a job).
pub fn spawn_shim_for_client(args: &serde_json::Value) -> Result<String, String> {
    let request: SpawnRequest = serde_json::from_value(args.clone())
        .map_err(|error| format!("bg-spawn: not a spawn request: {error}"))?;
    let launch = request.launch();
    // The session can itself be inside a job that forbids breakaway (a test
    // harness, a scheduler) while the client is in a call's job. Escape it if
    // the OS allows; otherwise spawn into the session's own job, which is still
    // outside the *call's* job — which is what this route is for.
    if spawn_shim(&launch, true).is_err() {
        spawn_shim(&launch, false).map_err(|error| format!("bg-spawn: {error}"))?;
    }
    Ok(launch.id.clone())
}

// ------------------------------------------------------------- what the model sees

/// The `user` message that wakes the agent when a job settles. It is a message,
/// not a tool result: a job settles whenever it wants, not inside a call.
pub fn wake_text(status: &Status) -> String {
    let took = match status.elapsed_secs() {
        Some(secs) => format!(" after {secs:.1}s"),
        None => String::new(),
    };
    let headline = match status.state.as_str() {
        STATE_CANCELLED => format!("[bg job {} was cancelled{took}]", status.id),
        STATE_LOST => format!(
            "[bg job {} was lost{took}: its shim is gone, so nothing can report the result]",
            status.id
        ),
        _ => match status.exit_code {
            Some(code) => format!("[bg job {} exited {code}{took}]", status.id),
            None => format!("[bg job {} ended{took}]", status.id),
        },
    };
    let mut text = headline;
    if !status.cmd.trim().is_empty() {
        text.push_str(&format!(
            "\ncommand: {}",
            crate::util::truncate_chars(status.cmd.trim(), 300)
        ));
    }
    let out = PathBuf::from(&status.out);
    text.push_str(&format!("\noutput: {}", status.out));
    if let Ok(partial) = crate::spill::partial(&out) {
        text.push_str(&format!(
            " ({} bytes, {} lines)",
            partial.bytes, partial.lines
        ));
        if !partial.tail.trim().is_empty() {
            text.push_str("\n--- tail ---\n");
            text.push_str(partial.tail.trim_end());
            text.push('\n');
        }
    }
    text.push_str(&format!(
        "read it with `kelir bg out {}`; the log records it as `bg_settled`",
        status.id
    ));
    text
}

/// What a bash call's result says when its command is promoted: the job id, how
/// to read it, and what it had already printed.
pub fn promoted_text(
    id: &str,
    limit: Duration,
    out: &Path,
    bytes: u64,
    lines: u64,
    tail: &str,
) -> String {
    let mut text = format!(
        "promoted to a background job after running past the {}s limit: bg job `{id}`\n\
         the job keeps running when the session ends.\n\
         read its output: kelir bg out {id} [--follow]\n\
         wait for it:     kelir bg wait {id}\n\
         cancel it:       kelir bg cancel {id}\n\
         output so far:   {bytes} bytes, {lines} lines at {}",
        limit.as_secs(),
        path_string(out)
    );
    if !tail.trim().is_empty() {
        text.push_str("\n--- tail ---\n");
        text.push_str(tail.trim_end());
        text.push('\n');
    }
    text.push_str(
        "you will get a message when it settles (a headless run that has already \
         exited delivers it on `kelir resume`).",
    );
    text
}

// ------------------------------------------------------------------ the shim

/// `kelir bg-shim`: spawn the command, write the status file, and stay alive
/// until the command exits (or the tree is killed).
pub async fn shim_main(args: BgShimArgs) -> u8 {
    let dir = args.dir.clone();
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!("kelir bg-shim: cannot create {}: {error}", dir.display());
        return 1;
    }
    let mut status = Status::new(&args.id, &args.kind, &args.cmd, &args.out);
    status.call = args.call.clone();
    status.job = args.job.clone();

    // The Job Object: for a call the session created it and the shim only
    // opens it (so the tree is in the call's job before the command runs);
    // for a `kelir bg run` job the shim creates it. Either way the shim keeps
    // its own handle, which is what lets a promoted job survive the session.
    #[cfg(windows)]
    let job = {
        let Some(name) = args.job.clone() else {
            fail(&dir, &status, "no job object name was given");
            return 1;
        };
        // A job the shim creates must outlive everything: no kill-on-close.
        let job = crate::proc::win::Job::open(&name)
            .or_else(|| crate::proc::win::Job::create_named(&name, false));
        match job {
            Some(job) => Some(job),
            None => {
                fail(&dir, &status, "cannot create or open the call's job object");
                return 1;
            }
        }
    };

    let out_file = match std::fs::File::create(&args.out) {
        Ok(file) => file,
        Err(error) => {
            fail(
                &dir,
                &status,
                &format!("cannot create {}: {error}", args.out.display()),
            );
            return 1;
        }
    };
    let out_for_err = match out_file.try_clone() {
        Ok(file) => file,
        Err(error) => {
            fail(
                &dir,
                &status,
                &format!("cannot duplicate the output handle: {error}"),
            );
            return 1;
        }
    };
    if args.shell.is_empty() {
        fail(&dir, &status, "no shell was given");
        return 1;
    }
    let argv: Vec<String> = std::iter::once(args.shell.clone())
        .chain(args.shell_args.iter().cloned())
        .collect();
    let mut command = tokio::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .arg(&args.cmd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(out_file))
        .stderr(std::process::Stdio::from(out_for_err));
    // A bash call's children must find `kelir bg` and their own call id.
    if let Some(session_dir) = dir.parent() {
        command.env("KELIR_SESSION_DIR", session_dir);
    }
    if let Ok(binary) = std::env::current_exe() {
        // The same binary this shim is: `kelir bg run` inside a call works even
        // when kelir is not on PATH.
        command.env("KELIR_BIN", binary);
    }
    if let Some(call) = &args.call {
        command.env("KELIR_CALL_ID", call);
    }
    if let Some(sock) = &args.sock {
        command.env("KELIR_SOCK", sock);
    }
    // The command inherits the shim's process group on Unix, so the whole tree
    // dies with one `killpg`; on Windows it is created suspended and assigned
    // to the job before it can run any of the user's code (M0 review N2).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.as_std_mut().creation_flags(
            crate::proc::win::CREATE_SUSPENDED | crate::proc::win::CREATE_NEW_PROCESS_GROUP,
        );
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            fail(&dir, &status, &format!("cannot start {}: {error}", argv[0]));
            return 1;
        }
    };
    let Some(pid) = child.id() else {
        fail(&dir, &status, "the command started without a pid");
        return 1;
    };
    #[cfg(windows)]
    if let Some(job) = &job {
        let assigned = job
            .assign(pid)
            .and_then(|()| crate::proc::win::resume(pid).map_err(|error| error.to_string()));
        if let Err(error) = assigned {
            // A call outside its job cannot be stopped: refuse to run it.
            crate::proc::win::terminate(pid);
            fail(
                &dir,
                &status,
                &format!("cannot control the command's tree: {error}"),
            );
            return 1;
        }
    }
    status.pid = Some(pid);
    if let Err(error) = status.write(&status_path(&dir, &args.id)) {
        eprintln!("kelir bg-shim: cannot write the status file: {error}");
    }

    // The owner watch: a session that dies without promoting this call must
    // not leave the tree behind. Promotion sets the marker and the shim keeps
    // running on its own.
    let owner = Owner::new(args.owner);
    let mut promoted = false;
    let status_file = status_path(&dir, &args.id);
    let marker = promote_path(&dir, &args.id);
    loop {
        if let Ok(Some(exit)) = child.try_wait() {
            status.state = STATE_EXITED.into();
            status.exit_code = exit.code();
            status.finished = Some(crate::event::now_ts());
            if let Err(error) = status.write(&status_file) {
                eprintln!("kelir bg-shim: cannot write the status file: {error}");
            }
            if !promoted {
                // The command is gone, but it may have left a child behind
                // (`cmd &`). On Unix the shim *is* the tree's group leader, so
                // staying until the group is empty is what lets the session's
                // death — even a `SIGKILL` — take those children with it (M4
                // review F6). A promoted job belongs to nobody and is left
                // alone; on Windows the job object's kill-on-close does the
                // same work with no process to keep.
                linger(&owner);
            }
            return 0;
        }
        if !promoted && marker.exists() {
            promoted = true;
            status.promoted = true;
            // The marker is only meaningful before the session dies; recording
            // it in the status is the acknowledgement promotion waits for.
            if let Err(error) = status.write(&status_file) {
                eprintln!("kelir bg-shim: cannot write the status file: {error}");
            }
        }
        if !promoted && owner.gone() {
            // The session that owns this call is gone and never promoted it:
            // take the tree down, as the session would have. The status goes
            // first, because on Unix the kill includes this process.
            status.state = STATE_CANCELLED.into();
            status.finished = Some(crate::event::now_ts());
            status.error = Some("the session exited".into());
            let _ = status.write(&status_file);
            kill_tree(std::process::id(), &args.job);
            return 1;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The command has exited; stay as the tree's owner while the tree still has a
/// process in it, so the owner's death can take those leftovers down with one
/// `killpg`. Exits immediately when nothing is left (the common case), and on
/// platforms where the group cannot be counted it does not linger at all.
#[cfg(target_os = "linux")]
fn linger(owner: &Owner) {
    // A `kelir bg run` job has no owner to watch: nothing to protect it from,
    // so the shim exits with the command.
    if owner.pid.is_none() {
        return;
    }
    loop {
        match crate::proc::group_others(std::process::id()) {
            // Nothing but this shim is left in the group: done.
            Some(0) => return,
            Some(_) => {}
            // The platform cannot say: do not hold a process for nothing.
            None => return,
        }
        if owner.gone() {
            kill_tree(std::process::id(), &None);
            return;
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(not(target_os = "linux"))]
fn linger(_owner: &Owner) {}

/// The session a shim answers to. Unix asks the kernel who its parent is (an
/// orphan is reparented the instant the session dies); Windows waits on the
/// session's process handle, so there is no polling lag at all.
struct Owner {
    pid: Option<u32>,
    #[cfg(windows)]
    watch: Option<crate::proc::win::OwnerWatch>,
}

impl Owner {
    fn new(pid: Option<u32>) -> Self {
        #[cfg(windows)]
        let watch = pid.and_then(crate::proc::win::OwnerWatch::open);
        Self {
            pid,
            #[cfg(windows)]
            watch,
        }
    }

    fn gone(&self) -> bool {
        let Some(pid) = self.pid else {
            return false;
        };
        #[cfg(unix)]
        {
            // The kernel reparents an orphan immediately, so the parent pid is
            // the honest answer.
            unsafe { libc::getppid() != pid as libc::pid_t }
        }
        #[cfg(windows)]
        {
            let _ = pid;
            match &self.watch {
                Some(watch) => watch.gone(),
                // The handle could not be opened: the process is already gone.
                None => true,
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = pid;
            false
        }
    }
}

/// Record a failure the model can read, and give up.
fn fail(dir: &Path, status: &Status, message: &str) {
    let mut status = status.clone();
    status.state = STATE_EXITED.into();
    status.exit_code = None;
    status.finished = Some(crate::event::now_ts());
    status.error = Some(message.to_string());
    let _ = status.write(&status_path(dir, &status.id));
    eprintln!("kelir bg-shim: {message}");
}

/// Take a tree down from inside the shim (the session died) or from `cancel`.
#[cfg(unix)]
fn kill_tree(pgid: u32, _job: &Option<String>) {
    if pgid == 0 {
        // `kill(0, …)` would signal the caller's own process group.
        return;
    }
    // The shim leads the group; the command inherited it, so this is the whole
    // tree — including the shim itself.
    unsafe {
        libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
        libc::kill(pgid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn kill_tree(pid: u32, job: &Option<String>) {
    if let Some(name) = job
        && let Some(job) = crate::proc::win::Job::open(name)
    {
        job.terminate_job();
        return;
    }
    if pid != 0 {
        crate::proc::win::terminate(pid);
    }
}

#[cfg(not(any(unix, windows)))]
fn kill_tree(pid: u32, _job: &Option<String>) {
    let _ = pid;
}

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
    let id = new_id();
    let out = out_path(&dir, &id);
    let call = std::env::var("KELIR_CALL_ID")
        .ok()
        .map(|call| call.trim().to_string())
        .filter(|call| !call.is_empty());
    let launch = Launch {
        dir: dir.clone(),
        id: id.clone(),
        out: out.clone(),
        cmd: command.clone(),
        shell,
        kind: KIND_JOB,
        // A job belongs to nobody: it outlives whatever started it.
        owner: None,
        call,
        job: Some(job_name("job", &id)),
        // A job started from inside a call inherits `KELIR_SOCK` from the CLI's
        // own environment; a job started by hand has no session to talk to. The
        // shim passes it on to the command, and the session uses it when the
        // job has to be started from outside a call's Job Object.
        sock: std::env::var("KELIR_SOCK")
            .ok()
            .map(|sock| sock.trim().to_string())
            .filter(|sock| !sock.is_empty()),
        // A job runs where the caller was, exactly like a call does (M4 review
        // F2): explicit, because on Windows the session may start the shim from
        // its own directory.
        cwd: std::env::current_dir().ok(),
    };
    if let Err(error) = start_shim(&launch).await {
        eprintln!("kelir: bg run: {error}");
        return 1;
    }

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
            // Tell the session at once (even when it started the shim itself):
            // the watcher would find the job on its next 250 ms poll, but the
            // report is what puts `bg_started` in the log before the call that
            // asked for the job has even settled.
            notify(&status).await;
            if status.error.is_some() { 1 } else { 0 }
        }
        None => {
            eprintln!("kelir: bg run: the job did not start (no status file)");
            1
        }
    }
}

/// Tell the session about a job over its socket, so it records `bg_started`
/// straight away instead of noticing the file on its next poll. Best effort:
/// a session that is gone, busy or not there is what the poller and `resume`
/// are for.
async fn notify(status: &Status) {
    let Ok(sock) = std::env::var("KELIR_SOCK") else {
        return;
    };
    if sock.trim().is_empty() {
        return;
    }
    let args = match serde_json::to_value(status) {
        Ok(args) => args,
        Err(_) => return,
    };
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

/// The command line the shell is given, from the words on `kelir bg run`'s own
/// command line (M4 review F3).
///
/// One word is a command line as written: it may hold shell syntax
/// (`kelir bg run 'sleep 5 && echo done'`). Several words are **literal
/// arguments**, each quoted, so the boundaries the caller wrote survive:
/// `kelir bg run printf "[%s]" "a b" c` runs `printf '[%s]' 'a b' c` and prints
/// `[a b][c]`, not `[a][b][c]`. Shell syntax belongs in one quoted word, which
/// is how every other tool that takes a command line works.
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
        follow(&path, &status_path(&dir, &status.id))
    }
}

/// `--follow`: print the file as it grows, and stop once the job settled and
/// nothing new arrived. Only the bytes past the offset are read, so following a
/// job that writes a lot costs the same as following a quiet one (M4 review N1).
fn follow(path: &Path, status_file: &Path) -> u8 {
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
                .map(|status| status.observed().settled())
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
    let mut status = status.observed();
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
            // (M4 review F4).
            Some(status) => status.observed(),
            None => {
                eprintln!("kelir: bg wait: the status file of {} vanished", args.id);
                return 1;
            }
        };
    }
}

/// `cancel`: take the job's whole tree down and record it. The shim is the only
/// writer of a status file while it lives; it is confirmed gone (or already
/// finished) before `cancelled` is written, so the two can never interleave.
///
/// Nothing is signalled on the strength of a status file alone: the pid is
/// checked against when the process started, so a status that outlived its shim
/// (a reboot, `taskkill /F`) cannot make kelir kill whatever inherited the pid
/// (M4 review F5).
async fn cancel(id: &str) -> u8 {
    let Some((dir, status)) = find(id) else {
        eprintln!("kelir: no background job `{id}`");
        return 2;
    };
    let status = status.observed();
    if status.settled() {
        // A `lost` job has no shim left to report it, but on Windows the job
        // object keeps its name and holds the tree: taking it down here is what
        // keeps a dead shim from leaving an orphan nobody can reach. Nothing is
        // ever signalled by pid alone: that one can belong to anything by now
        // (M4 review F5).
        if status.state == STATE_LOST && status.job.is_some() {
            kill_tree(0, &status.job);
        }
        println!("{} {} (nothing to cancel)", status.id, status.state);
        return 0;
    }
    kill_tree(status.shim.or(status.pid).unwrap_or(0), &status.job);
    #[cfg(windows)]
    if let Some(shim) = status.shim {
        crate::proc::win::terminate(shim);
    }
    let deadline = std::time::Instant::now() + CANCEL_WAIT;
    while let Some(shim) = status.shim {
        if !process_alive(shim) || std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    let mut final_status = Status::read(&status_path(&dir, &status.id)).unwrap_or(status.clone());
    if final_status.settled() {
        // It finished on its own before the kill landed; that is the truth.
        println!(
            "{} {} (it had already finished)",
            final_status.id, final_status.state
        );
        return 0;
    }
    final_status.state = STATE_CANCELLED.into();
    final_status.finished = Some(crate::event::now_ts());
    match final_status.write(&status_path(&dir, &final_status.id)) {
        Ok(()) => {
            println!("{} cancelled", final_status.id);
            0
        }
        Err(error) => {
            eprintln!("kelir: bg cancel: cannot write the status file: {error}");
            1
        }
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // `kill(0, …)` signals the caller's own process group, so pid 0 is not a
    // process here: no status names the shim as 0, and asking must not answer
    // "alive" for "my own group".
    if pid == 0 {
        return false;
    }
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    match crate::proc::win::OwnerWatch::open(pid) {
        Some(watch) => !watch.gone(),
        None => false,
    }
}

#[cfg(not(any(unix, windows)))]
fn process_alive(_pid: u32) -> bool {
    false
}

/// The `bg_started`/`bg_settled` payloads for the log, from a status file.
pub fn started_body(status: &Status) -> crate::event::EventBody {
    crate::event::EventBody::BgStarted {
        bg_id: status.id.clone(),
        call_id: status.call.clone(),
        cmd: Payload::text(status.cmd.clone()),
        pid: status.pid,
        out_path: status.out.clone(),
    }
}

pub fn settled_body(status: &Status) -> crate::event::EventBody {
    crate::event::EventBody::BgSettled {
        bg_id: status.id.clone(),
        call_id: status.call.clone(),
        cmd: Payload::text(status.cmd.clone()),
        exit_code: status.exit_code,
        state: status.state.clone(),
        out_path: status.out.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kelir-bg-{}-{name}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample() -> Status {
        Status {
            id: new_id(),
            state: STATE_RUNNING.into(),
            pid: Some(4242),
            shim: Some(4241),
            exit_code: None,
            started: "2026-11-19T10:00:00Z".into(),
            finished: None,
            cmd: "sleep 60".into(),
            out: "bg/bg_1.out".into(),
            kind: KIND_JOB.into(),
            promoted: false,
            call: None,
            job: Some("kelir-bg-job-bg_1".into()),
            error: None,
        }
    }

    #[test]
    fn ids_are_prefixed_lowercase_ulids() {
        let id = new_id();
        assert!(id.starts_with("bg_"), "{id}");
        let body = &id["bg_".len()..];
        assert_eq!(body.len(), 26);
        assert!(
            body.chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        );
        assert_ne!(new_id(), new_id());
    }

    #[test]
    fn a_status_file_round_trips_and_replaces_atomically() {
        let dir = temp_dir("write");
        let path = status_path(&dir, "bg_1");
        let mut status = sample();
        status.write(&path).unwrap();
        assert_eq!(Status::read(&path).unwrap(), status);

        // A second write replaces it whole, and leaves no temp file behind.
        status.state = STATE_EXITED.into();
        status.exit_code = Some(0);
        status.finished = Some("2026-11-19T10:00:03Z".into());
        status.write(&path).unwrap();
        assert_eq!(Status::read(&path).unwrap(), status);
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with('{') && text.ends_with('}'), "{text}");
        assert_eq!(status.elapsed_secs(), Some(3.0));
    }

    #[test]
    fn a_torn_or_missing_status_file_is_skipped_not_fatal() {
        let dir = temp_dir("torn");
        let path = status_path(&dir, "bg_torn");
        std::fs::write(&path, "{\"id\": \"bg_torn\", \"state\": \"run").unwrap();
        assert!(Status::read(&path).is_none());
        // The listing skips it, and skips a leftover temp file too.
        std::fs::write(dir.join("bg_other.status.123.tmp"), "{}").unwrap();
        assert!(list(&dir).is_empty());
        // A good neighbour is still listed.
        let mut good = sample();
        good.id = "bg_good".into();
        good.write(&status_path(&dir, "bg_good")).unwrap();
        let listed = list(&dir);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "bg_good");
        assert!(Status::read(&dir.join("nope.status")).is_none());
    }

    #[test]
    fn only_jobs_and_promoted_calls_are_jobs() {
        let mut status = sample();
        assert!(status.is_job(), "an explicit job");
        status.kind = KIND_CALL.into();
        assert!(!status.is_job(), "an ordinary call is not a job");
        status.promoted = true;
        assert!(status.is_job(), "a promoted call is");
        status.state = STATE_EXITED.into();
        assert!(status.settled());
        assert!(!sample().settled());
    }

    #[test]
    fn promotion_is_due_only_after_the_configured_time_and_never_at_zero() {
        let after = Duration::from_secs(600);
        assert!(!promotion_due(after, Duration::from_secs(599)));
        assert!(promotion_due(after, Duration::from_secs(600)));
        assert!(promotion_due(after, Duration::from_secs(601)));
        assert!(
            !promotion_due(Duration::ZERO, Duration::from_secs(10_000)),
            "zero disables promotion"
        );
        assert!(promotion_due(
            Duration::from_secs(1),
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn the_wake_text_names_the_job_its_state_and_its_output() {
        let dir = temp_dir("wake");
        let out = out_path(&dir, "bg_1");
        std::fs::write(&out, "hello from the job\n").unwrap();
        let mut status = sample();
        status.id = "bg_1".into();
        status.out = path_string(&out);
        status.state = STATE_EXITED.into();
        status.exit_code = Some(0);
        status.finished = Some("2026-11-19T10:00:02Z".into());
        let text = wake_text(&status);
        assert!(
            text.starts_with("[bg job bg_1 exited 0 after 2.0s]"),
            "{text}"
        );
        assert!(text.contains("command: sleep 60"), "{text}");
        assert!(text.contains("hello from the job"), "{text}");
        assert!(text.contains("kelir bg out bg_1"), "{text}");

        status.state = STATE_CANCELLED.into();
        status.exit_code = None;
        let text = wake_text(&status);
        assert!(text.contains("was cancelled"), "{text}");
    }

    #[test]
    fn the_promotion_text_says_how_to_read_the_job() {
        let text = promoted_text(
            "11-2-1",
            Duration::from_secs(600),
            Path::new("s/commands/11-2-1/out"),
            12,
            2,
            "hi\n",
        );
        assert!(text.contains("bg job `11-2-1`"), "{text}");
        assert!(text.contains("600s limit"), "{text}");
        assert!(text.contains("kelir bg out 11-2-1"), "{text}");
        assert!(text.contains("kelir bg wait 11-2-1"), "{text}");
        assert!(text.contains("kelir bg cancel 11-2-1"), "{text}");
        assert!(text.contains("12 bytes, 2 lines"), "{text}");
        assert!(text.contains("s/commands/11-2-1/out"), "{text}");
        assert!(text.contains("--- tail ---\nhi"), "{text}");
        assert!(
            text.ends_with("delivers it on `kelir resume`)."),
            "a headless run must say when the wake arrives: {text}"
        );
        assert!(
            text.contains("you will get a message when it settles"),
            "{text}"
        );
    }

    #[test]
    fn job_names_are_filesystem_and_kernel_safe() {
        let name = job_name("ses_01ABCD", "11-2-1");
        assert_eq!(name, "kelir-bg-ses_01ABCD-11-2-1");
        assert!(!name.contains('\\'));
        assert!(!name.contains('/'));
        assert_eq!(job_name("s/x", "a.b"), "kelir-bg-s_x-a_b");
    }

    #[test]
    fn find_looks_in_the_root_and_then_in_the_sessions() {
        let dir = temp_dir("find");
        let mut status = sample();
        status.id = "bg_found".into();
        status.write(&status_path(&dir, "bg_found")).unwrap();
        // The root is asked first; a job is only found there when the id is.
        assert!(Status::read(&status_path(&dir, "bg_found")).is_some());
        // Path traversal is refused rather than resolved.
        assert!(find("../escape").is_none());
        assert!(find("").is_none());
    }

    /// A status file with no pid must not turn `cancel` into "signal my own
    /// process group" (`kill(0, …)` is every process in it, including the test).
    #[cfg(unix)]
    #[test]
    fn killing_group_zero_does_nothing() {
        kill_tree(0, &None);
    }

    #[test]
    fn a_promotion_marker_is_written_and_seen() {
        let dir = temp_dir("promote");
        let marker = promote_path(&dir, "11-2-1");
        assert!(!marker.exists());
        mark_promoted(&dir, "11-2-1").unwrap();
        assert!(marker.exists());
    }

    /// One word is a command line as written; several are literal arguments
    /// whose boundaries survive (M4 review F3).
    #[test]
    fn bg_run_keeps_the_boundaries_the_caller_wrote() {
        let line =
            |words: &[&str]| command_line(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>());
        assert_eq!(line(&["sleep 5; echo done"]), "sleep 5; echo done");
        assert_eq!(
            line(&["printf", "[%s]", "a b", "c"]),
            "printf '[%s]' 'a b' c"
        );
        assert_eq!(
            line(&["bash", "-c", "echo one; echo two"]),
            "bash -c 'echo one; echo two'"
        );
        assert_eq!(line(&["echo", "it's"]), r"echo 'it'\''s'");
        assert_eq!(line(&["echo", ""]), "echo ''");
        assert_eq!(line(&["nc", "-l", "8080"]), "nc -l 8080");
    }

    /// A job whose shim is gone is never reported as running for ever: the
    /// status file is left alone and the reader says `lost` (M4 review F4).
    #[test]
    fn a_running_status_with_no_shim_is_lost() {
        let mut status = sample();
        // A pid that cannot exist (0 is never a process) stands in for a shim
        // that is gone.
        status.shim = Some(0);
        let observed = status.observed();
        assert_eq!(observed.state, STATE_LOST, "{observed:?}");
        assert!(observed.settled());
        assert!(observed.error.unwrap_or_default().contains("shim"));
        // A settled status is never reinterpreted.
        status.state = STATE_EXITED.into();
        status.exit_code = Some(0);
        assert_eq!(status.observed().state, STATE_EXITED);
    }

    /// The identity check itself: the status of this very process is alive, a
    /// status whose `started` is years away is somebody else's pid.
    #[test]
    fn a_status_is_matched_with_its_own_process_not_just_its_pid() {
        let me = std::process::id();
        let mut status = sample();
        status.shim = Some(me);
        status.started = crate::event::now_ts();
        assert!(shim_alive(&status), "this process is its own shim");
        if crate::proc::start_time(me).is_some() {
            status.started = "2020-01-01T00:00:00Z".into();
            assert!(
                !shim_alive(&status),
                "a pid whose process started years after the status was written is not the shim"
            );
        }
        // No shim at all is nobody's.
        status.shim = None;
        assert!(!shim_alive(&status));
    }

    #[test]
    fn the_log_bodies_carry_what_the_model_needs() {
        let mut status = sample();
        status.id = "bg_7".into();
        status.state = STATE_EXITED.into();
        status.exit_code = Some(3);
        status.call = Some("call_9".into());
        let started = started_body(&status);
        match &started {
            crate::event::EventBody::BgStarted {
                bg_id,
                call_id,
                pid,
                ..
            } => {
                assert_eq!(bg_id, "bg_7");
                assert_eq!(call_id.as_deref(), Some("call_9"));
                assert_eq!(*pid, Some(4242));
            }
            other => panic!("unexpected {other:?}"),
        }
        let settled = settled_body(&status);
        match &settled {
            crate::event::EventBody::BgSettled {
                exit_code, state, ..
            } => {
                assert_eq!(*exit_code, Some(3));
                assert_eq!(state, "exited");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
