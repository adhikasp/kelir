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

/// How far a recorded start time may be from the process's own and still be the
/// same process. Both are the OS's answer to the same question, so this only
/// covers serialisation rounding between the shim's write and the reader's
/// parse (a Linux start time is truncated to the second), never a pid that was
/// recycled: a recycled process started at another time, and G1 of the round-2
/// review is that two minutes of slack let it pass for the shim.
pub const START_SLACK: Duration = Duration::from_secs(1);

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
    /// When the shim itself started, as the shim read it from the OS at launch
    /// (`proc::start_time`). A reader compares this **exact** value before it
    /// trusts a pid, so a pid recycled after the job started is never mistaken
    /// for the shim (M4 review round 2, G1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shim_started: Option<String>,
    /// When the command started, read from the OS right after the spawn: the
    /// proof `cancel` needs to reach a lost job's tree by the command's pid
    /// (M4 review round 2, G2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_started: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub started: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<String>,
    #[serde(default)]
    pub cmd: String,
    /// Where a long command was written instead of being passed as an
    /// argument (`cmd.sh`/`cmd.bat` in the job's directory). `cmd` is then a
    /// short reference, and this file holds the exact bytes that ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd_path: Option<String>,
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
    /// Ed25519 signature (hex) over the rest of the record, made with the job's
    /// private key (held by the session and that job's shim, never by a tool
    /// child). A reader ignores a record whose signature does not verify under
    /// the job's public key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
    /// A counter that only goes up, bound into the signature together with the
    /// job id: every signed write takes a higher one than any before it, so a
    /// reader that has seen a record refuses an older one, and a final record
    /// (exited, cancelled, lost) is terminal (STEER-7 review round 3).
    #[serde(default)]
    pub seq: u64,
}

impl Status {
    pub fn new(id: &str, kind: &str, cmd: &str, out: &Path) -> Self {
        Self {
            id: id.to_string(),
            state: STATE_RUNNING.to_string(),
            pid: None,
            shim: Some(std::process::id()),
            shim_started: None,
            pid_started: None,
            exit_code: None,
            started: crate::event::now_ts(),
            finished: None,
            cmd: cmd.to_string(),
            cmd_path: None,
            out: path_string(out),
            kind: kind.to_string(),
            promoted: false,
            call: None,
            job: None,
            error: None,
            sig: None,
            seq: 0,
        }
    }

    /// The bytes the signature covers: the record without its `sig`, in the
    /// struct's own field order (so a rewritten file's key order is irrelevant).
    fn sealed_bytes(&self) -> Vec<u8> {
        let mut bare = self.clone();
        bare.sig = None;
        serde_json::to_vec(&bare).unwrap_or_default()
    }

    /// Sign the record in place.
    pub fn sign(&mut self, secret: &str) {
        self.sig = crate::trust::sign(secret, &self.sealed_bytes());
    }

    /// Does the record carry a valid signature under the job's public key?
    pub fn verifies(&self, public: &str) -> bool {
        match &self.sig {
            Some(sig) if !public.is_empty() => {
                crate::trust::verify(public, &self.sealed_bytes(), sig)
            }
            _ => false,
        }
    }

    /// Write the record signed with `key` (unsigned when there is none: a job
    /// started by hand has no session to authenticate to).
    ///
    /// A signed write takes the next sequence number, and a final record is
    /// also written beside the status file ([`final_path`]) so that putting an
    /// older record back over the status file cannot un-settle the job.
    pub fn write_with(&self, path: &Path, key: Option<&str>) -> std::io::Result<()> {
        match key.filter(|key| !key.is_empty()) {
            Some(key) => {
                let mut signed = self.clone();
                signed.seq = next_seq(self.seq);
                signed.sign(key);
                if signed.settled() {
                    // Final first: if the status write is cut short, the
                    // terminal record still exists.
                    let _ = signed.write(&path.with_extension("final"));
                }
                signed.write(path)
            }
            None => self.write(path),
        }
    }

    pub fn settled(&self) -> bool {
        self.state != STATE_RUNNING
    }

    /// The status as a reader should understand it: a `running` job whose shim
    /// is gone — or whose shim cannot be proved to be the process holding its
    /// pid — is [`STATE_LOST`], because nothing will ever write it again. The
    /// file on disk is left alone — a reader never writes another process's
    /// status (M4 review F4).
    pub fn observed(&self) -> Status {
        if self.state != STATE_RUNNING {
            return self.clone();
        }
        let error = match shim_identity(self) {
            // Alive, and its own start time is the recorded one (or the
            // platform cannot report start times at all: liveness is the best
            // answer available there, and signalling is refused instead).
            ShimIdentity::Same | ShimIdentity::NoPlatform => return self.clone(),
            ShimIdentity::Different => "the job's shim is gone; its outcome is unknown",
            ShimIdentity::NoRecord => {
                "the job's status records no shim start time, so the shim's identity cannot be \
                 checked; its outcome is unknown"
            }
        };
        let mut lost = self.clone();
        lost.state = STATE_LOST.into();
        lost.finished = Some(self.finished.clone().unwrap_or_else(crate::event::now_ts));
        lost.error = Some(error.into());
        lost
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

/// Whether the process holding a pid is the process a status file recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimIdentity {
    /// The pid is alive and its own start time is the recorded one.
    Same,
    /// The pid is gone, or it now belongs to a process that started at another
    /// time: a recycled pid.
    Different,
    /// The status does not record a start time, so its pid proves nothing.
    NoRecord,
    /// The platform cannot report start times; plain liveness is all there is.
    NoPlatform,
}

/// Is the process holding `status.shim`'s pid really the shim that wrote the
/// status? A status outlives its shim and a pid is reused within seconds, so
/// the answer comes from comparing the start time the shim recorded for itself
/// with the one the OS reports for that pid now — not from the job's `started`
/// (four minutes of job lifetime can pass before a shim is lost).
pub fn shim_identity(status: &Status) -> ShimIdentity {
    identity(status.shim, status.shim_started.as_deref())
}

/// The same question for the command's own pid (`pid_started`): only `cancel`
/// on Windows walks a lost job's tree by it, and the unit tests ask on every
/// platform.
#[cfg(any(windows, test))]
fn pid_identity(status: &Status) -> ShimIdentity {
    identity(status.pid, status.pid_started.as_deref())
}

fn identity(pid: Option<u32>, recorded: Option<&str>) -> ShimIdentity {
    let Some(pid) = pid.filter(|pid| *pid != 0) else {
        return ShimIdentity::NoRecord;
    };
    // Liveness first: a killed process keeps its creation time until the
    // kernel object is reaped, so a start time alone would call it alive.
    if !process_alive(pid) {
        return ShimIdentity::Different;
    }
    // Alive, but its start time is not ours to read (macOS).
    let Some(actual) = crate::proc::start_time(pid) else {
        return ShimIdentity::NoPlatform;
    };
    let Some(recorded) = recorded.and_then(parse_ts) else {
        return ShimIdentity::NoRecord;
    };
    let actual = time::OffsetDateTime::from(actual);
    if (recorded - actual).whole_seconds().unsigned_abs() <= START_SLACK.as_secs() {
        ShimIdentity::Same
    } else {
        ShimIdentity::Different
    }
}

/// A start time as the status file writes it: RFC 3339, or `None` where the
/// platform cannot say (macOS) — in which case nothing is ever signalled on the
/// strength of a pid.
fn start_time_text(pid: u32) -> Option<String> {
    let at = crate::proc::start_time(pid)?;
    time::OffsetDateTime::from(at)
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
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
    if let Some(dir) = crate::envs::var_os("KELIR_SESSION_DIR")
        .map(PathBuf::from)
        .filter(|dir| !dir.as_os_str().is_empty())
    {
        return dir.join("bg");
    }
    crate::paths::state_dir().join("bg")
}

/// The next signed-record sequence number: strictly above `floor` and above
/// every one this process handed out, and (the wall clock in nanoseconds) above
/// whatever an earlier process of the same job used.
fn next_seq(floor: u64) -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    let want = now.max(floor.saturating_add(1));
    let mut last = LAST.load(Ordering::Relaxed);
    loop {
        let next = want.max(last.saturating_add(1));
        match LAST.compare_exchange(last, next, Ordering::SeqCst, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(seen) => last = seen,
        }
    }
}

/// Where a job's final record is also kept: `<id>.final`, written by the shim
/// (or the session's `cancel`) when the job settles.
pub fn final_path(dir: &Path, id: &str) -> PathBuf {
    status_path(dir, id).with_extension("final")
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

/// How long a `cancel` marker is believed. A cancel takes at most
/// [`CANCEL_WAIT`]; the rest is slack for a slow box, not for a stale file.
pub const CANCEL_MARKER_TTL: Duration = Duration::from_secs(30);

/// The marker a `cancel` writes before it kills anything (STEER-7). Between the
/// kill and the final `cancelled` status (up to [`CANCEL_WAIT`]) the status file
/// still says `running` and its shim is already dead, which readers would report
/// as `lost` — the session's own watcher recorded exactly that, so a `cancel`
/// shown in the log as "the job was lost". The marker tells every reader "a
/// cancel is in flight; keep waiting", and only `cancel` writes or removes it.
pub fn cancel_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.cancel"))
}

/// Is a `cancel` in flight for this job?
///
/// A marker only counts while it is young: a `cancel` that was itself killed
/// between the kill and the status write leaves the file behind, and a marker
/// that never expired would keep a dead job `running` for ever — the exact
/// failure [`Status::observed`] exists to prevent. Past [`CANCEL_MARKER_TTL`]
/// the marker is ignored and the job is judged by its shim, as before.
pub fn cancelling(dir: &Path, id: &str) -> bool {
    let path = cancel_path(dir, id);
    let Ok(metadata) = std::fs::metadata(&path) else {
        return false;
    };
    match metadata.modified().ok().and_then(|at| at.elapsed().ok()) {
        Some(age) => age <= CANCEL_MARKER_TTL,
        // A clock that will not say is no proof a cancel is happening.
        None => false,
    }
}

/// A status as a reader should understand it, given the `bg/` directory it was
/// read from: [`Status::observed`], except that a job whose `cancel` marker is
/// present is still `running` (it is about to be, not lost).
pub fn observed_in(dir: &Path, status: &Status) -> Status {
    if status.state == STATE_RUNNING && cancelling(dir, &status.id) {
        return status.clone();
    }
    status.observed()
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
/// ([`Status::observed`]), so a listing never shows a job that runs for ever —
/// unless a `cancel` is in flight, when it is still running ([`observed_in`]).
pub fn list(dir: &Path) -> Vec<Status> {
    let mut out: Vec<Status> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("status"))
        .filter_map(|path| Status::read(&path))
        .map(|status| observed_in(dir, &status))
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
    /// A file holding the command, when it is too long to pass on a command
    /// line. The shim reads it and does not take `cmd` (a 40 KB argument past
    /// the Windows limit does not even reach the shim: `spawn` fails with
    /// `os error 206`).
    pub cmd_file: Option<PathBuf>,
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
    /// The session's socket token, exported to the command as `KELIR_TOKEN`
    /// (STEER-7): a job that reports itself over the socket must carry it.
    pub token: Option<String>,
    /// This job's status key (and, for an explicit job, its capability). It
    /// reaches the shim over an inherited pipe and nowhere else: not argv, not
    /// the environment (a same-user child reads both), not a file. The shim
    /// does not pass it on to the command it runs.
    pub key: Option<String>,
    /// This job's capability, the second line of the same pipe: the shim
    /// reports itself to the session with it. Never argv, never environment.
    pub capability: Option<String>,
    /// The environment the shim and its command get, when the starter is the
    /// session acting for a client: exactly the client's own (the session's
    /// environment is not the client's and must not leak into its job).
    pub env: Option<Vec<(String, String)>>,
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
    if let Some(env) = &launch.env {
        command.env_clear();
        command.envs(env.iter().map(|(name, value)| (name, value)));
    }
    command
        .arg("bg-shim")
        .arg("--dir")
        .arg(&launch.dir)
        .arg("--id")
        .arg(&launch.id)
        .arg("--out")
        .arg(&launch.out);
    match &launch.cmd_file {
        // The command is a file: nothing long on this arg list either, so the
        // shim can be started whatever the command's size.
        Some(path) => {
            command.arg("--cmd-file").arg(path);
        }
        None => {
            command.arg("--cmd").arg(&launch.cmd);
        }
    }
    command.arg("--kind").arg(launch.kind);
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
    // The token goes through the environment, never the argv (STEER-7): a
    // command line is world-readable on Linux (`/proc/<pid>/cmdline`) and a
    // token in it would be a token anyone could read. The shim passes its own
    // environment on to the command it starts, so nothing else is needed.
    if let Some(token) = &launch.token {
        command.env(crate::trust::TOKEN_ENV, token);
    }
    // The status key goes down a pipe: written here (a few dozen bytes, far
    // below the pipe's buffer, so nothing blocks) and closed before the shim
    // exists. An environment variable or an argv would be readable by any
    // process of the same user (`/proc/<pid>/environ`, the PEB); the pipe's
    // contents are not (steer7b review S-A).
    let mut stdin = std::process::Stdio::null();
    if let Some(key) = launch.key.as_deref().filter(|key| !key.is_empty()) {
        use std::io::Write as _;
        let capability = launch.capability.as_deref().unwrap_or("");
        let (reader, mut writer) =
            std::io::pipe().map_err(|error| format!("cannot make the key pipe: {error}"))?;
        writer
            .write_all(
                format!(
                    "{key}
{capability}
"
                )
                .as_bytes(),
            )
            .map_err(|error| format!("cannot hand the shim its key: {error}"))?;
        drop(writer);
        command.arg("--key-stdin");
        stdin = std::process::Stdio::from(reader);
    }
    // The command inherits this directory; the shim sets nothing of its own.
    if let Some(cwd) = &launch.cwd {
        command.current_dir(cwd);
    }
    // The shim needs no stdio of its own: the command writes straight to the
    // output file, and errors go into the status file.
    command
        .stdin(stdin)
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
        crate::proc::win::CREATE_NO_WINDOW
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
    /// The job id, when the caller already has one (a re-spawn). A first spawn
    /// leaves it out: the **session** mints the id, and the answer carries it
    /// back (STEER-7 review B1: a child must not be able to pick a job id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
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
    /// The token the client holds; the session compares it with its own before
    /// it starts anything (STEER-7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The directory the client was in: a job started inside a call runs where
    /// the call does, and the session is not there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The client's environment: the job runs in it, not in the session's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
}

impl SpawnRequest {
    pub fn of(launch: &Launch) -> Self {
        Self {
            dir: path_string(&launch.dir),
            id: Some(launch.id.clone()),
            out: path_string(&launch.out),
            cmd: launch.cmd.clone(),
            shell: launch.shell.clone(),
            kind: launch.kind.to_string(),
            call: launch.call.clone(),
            job: launch.job.clone(),
            sock: launch.sock.clone(),
            token: launch.token.clone(),
            cwd: launch.cwd.as_ref().map(|cwd| path_string(cwd)),
            env: crate::envs::all(),
        }
    }

    pub fn launch(&self, id: &str) -> Launch<'_> {
        // A spawn request never carries a capability in: the session mints the
        // job's capability itself and starts the shim with it
        // ([`spawn_shim_for_client`]). The id is the session's own, not the
        // caller's: an omitted one was minted when the spawn was answered.
        Launch {
            dir: PathBuf::from(&self.dir),
            id: id.to_string(),
            out: PathBuf::from(&self.out),
            cmd: self.cmd.clone(),
            cmd_file: None,
            shell: self.shell.clone(),
            kind: &self.kind,
            // A job belongs to nobody: it outlives whatever started it.
            owner: None,
            call: self.call.clone(),
            job: self.job.clone(),
            sock: self.sock.clone(),
            token: self.token.clone(),
            key: None,
            capability: None,
            env: Some(self.env.clone()),
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
        // `CREATE_NO_WINDOW`, not `DETACHED_PROCESS`: the shim is a console
        // program, and a detached one leaves its console-subsystem child
        // (`bash.exe`) to allocate a fresh console — a new `conhost.exe` per
        // job, which under 40 concurrent starts took seconds instead of a tenth
        // of one (M4 review round 2, G3). With `CREATE_NO_WINDOW` the shim gets
        // a console with no window and the child inherits it; the shim is still
        // outside the session's console and, with the breakaway flag, outside
        // its Job Object, so it outlives the session exactly as before.
        let flags = crate::proc::win::CREATE_NO_WINDOW | crate::proc::win::CREATE_NEW_PROCESS_GROUP;
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

/// How the session answers a spawn: the id it minted, and nothing else. The
/// job's capability and signing key go from the session to the shim it starts
/// (over the shim's stdin pipe) and to nobody else: no verb returns either, so
/// no tool child, which holds the session token, can ever obtain one (STEER-7
/// review round 3, B1/B2).
#[derive(Debug, Clone, Serialize)]
pub struct Spawned {
    pub id: String,
}

/// The session's half of [`SpawnRequest`]: start the shim for a client and
/// answer at once (`kelir bg run` waits for the status file; the session must
/// not block on a job).
///
/// The session mints the id, the capability and the key, always: a request
/// that names an id is refused by `trust::check_spawn`, so there is no way to
/// ask for an existing job's secrets. Where the job lives (`bg/`, its output
/// file) is the session's, never the request's. The secrets reach the shim over
/// an inherited pipe, never an argv or the environment.
pub fn spawn_shim_for_client(
    trust: &crate::trust::Trust,
    spawn: &SpawnRequest,
) -> Result<Spawned, String> {
    if spawn.id.as_deref().is_some_and(|id| !id.trim().is_empty()) {
        return Err("bg-spawn: the session mints job ids; a request cannot name one".to_string());
    }
    let (id, capability) = trust.claim(spawn.call.as_deref())?;
    let dir = trust.bg_dir();
    let shell_args: Vec<String> = spawn.shell.iter().skip(1).cloned().collect();
    let cmd_file = match crate::shellcmd::input(&spawn.cmd, &shell_args, &dir, &id) {
        Ok(crate::shellcmd::ShellInput::Argument(_)) => None,
        Ok(crate::shellcmd::ShellInput::Script { path, .. }) => Some(path),
        Err(error) => {
            trust.forget(&id);
            return Err(format!("bg-spawn: {error}"));
        }
    };
    let mut launch = spawn.launch(&id);
    launch.out = out_path(&dir, &id);
    launch.dir = dir;
    launch.cmd_file = cmd_file;
    launch.job = Some(job_name("job", &id));
    launch.key = trust.key(&id);
    launch.capability = Some(capability);
    // The session can itself be inside a job that forbids breakaway (a test
    // harness, a scheduler) while the client is in a call's job. Escape it if
    // the OS allows; otherwise spawn into the session's own job, which is still
    // outside the *call's* job — which is what this route is for.
    let spawned = match spawn_shim(&launch, true) {
        Ok(()) => Ok(()),
        Err(_) => spawn_shim(&launch, false).map_err(|error| format!("bg-spawn: {error}")),
    };
    if let Err(error) = spawned {
        // Nothing was started: do not leave a job nobody can report for.
        trust.forget(&id);
        return Err(error);
    }
    Ok(Spawned { id })
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
    // The status key, read once from the pipe the starter wrote it to; it stays
    // in this process's memory and is never handed to the command.
    let secrets = if args.key_stdin {
        read_secrets()
    } else {
        Secrets::default()
    };
    let key = secrets.key.as_deref();
    // A job's shim holds the signing key for as long as the job lives: close
    // the process to its own user's ordinary memory reads (Windows; on Unix
    // `/proc/<pid>/mem` is closed to a non-ptrace-capable sibling by the
    // kernel's ptrace rules where Yama is on, and by dumpable below).
    if key.is_some() && args.kind == KIND_JOB {
        harden_process();
    }
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!("kelir bg-shim: cannot create {}: {error}", dir.display());
        return 1;
    }
    // A command that came as a file (it is too long for an argument list) is
    // read here, in the shim's own memory, and never put on a command line.
    let script = args.cmd_file.clone();
    let args = match script.as_ref() {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => BgShimArgs { cmd: text, ..args },
            Err(error) => {
                eprintln!("kelir bg-shim: cannot read {}: {error}", path.display());
                return 1;
            }
        },
        None => args,
    };
    let mut status = Status::new(&args.id, &args.kind, &args.cmd, &args.out);
    status.call = args.call.clone();
    status.job = args.job.clone();
    // The shim reads *its own* start time from the OS, in this process: a
    // reader in another process compares that exact value with what the OS says
    // about the pid before it signals anything, which is what makes a recycled
    // pid impossible to mistake for the shim (M4 review round 2, G1).
    status.shim_started = start_time_text(std::process::id());

    // The Job Object: for a call the session created it and the shim only
    // opens it (so the tree is in the call's job before the command runs);
    // for a `kelir bg run` job the shim creates it. Either way the shim keeps
    // its own handle, which is what lets a promoted job survive the session.
    #[cfg(windows)]
    let job = {
        let Some(name) = args.job.clone() else {
            fail(&dir, &status, key, "no job object name was given");
            return 1;
        };
        // A job the shim creates must outlive everything: no kill-on-close.
        let job = crate::proc::win::Job::open(&name)
            .or_else(|| crate::proc::win::Job::create_named(&name, false));
        match job {
            Some(job) => Some(job),
            None => {
                fail(
                    &dir,
                    &status,
                    key,
                    "cannot create or open the call's job object",
                );
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
                key,
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
                key,
                &format!("cannot duplicate the output handle: {error}"),
            );
            return 1;
        }
    };
    if args.shell.is_empty() {
        fail(&dir, &status, key, "no shell was given");
        return 1;
    }
    let argv: Vec<String> = std::iter::once(args.shell.clone())
        .chain(args.shell_args.iter().cloned())
        .collect();
    // A long command is written to a file and handed to the shell by name: a
    // command line past ~8 KB is silently cut on Windows (`shellcmd`), and the
    // cut looks like success. `cmd` is what the shell actually runs.
    let script_dir = args
        .out
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dir.clone());
    // A caller that already wrote the script (the bash call, which had to keep
    // the shim's own argv short) decided it; otherwise the shim decides from
    // the command and the shell.
    let shell_input = match &script {
        Some(path) => match crate::shellcmd::script_input(path, &args.shell_args) {
            Ok(input) => input,
            Err(error) => {
                fail(&dir, &status, key, &error);
                return 1;
            }
        },
        None => match crate::shellcmd::input(&args.cmd, &args.shell_args, &script_dir, "") {
            Ok(input) => input,
            Err(error) => {
                fail(&dir, &status, key, &error);
                return 1;
            }
        },
    };
    let mut command = tokio::process::Command::new(&argv[0]);
    match &shell_input {
        crate::shellcmd::ShellInput::Argument(cmd) => {
            command.args(&argv[1..]).arg(cmd);
        }
        crate::shellcmd::ShellInput::Script { path, keep_switch } => {
            // The switch is kept for a shell that needs one even with a file
            // (`cmd.exe /C <script>`, `pwsh -File <script>`): dropping `/C`
            // starts an interactive cmd.exe, which runs nothing and exits 0
            // (STEER-6 review B1). A POSIX shell reads the file from its own
            // argv, so its `-c` goes.
            let rest = crate::shellcmd::script_args(&args.shell_args, keep_switch.as_deref(), path);
            command.args(&rest);
            // A reader of the status file (and the model, in a `bg` report)
            // gets a reference, never a 40 KB command; `cmd.sh` has the bytes.
            // Set here and written after the spawn like every other field: an
            // early write would be the status `bg run` reads first, and it has
            // no pid yet (a test caught that as "the shim never started").
            status.cmd = crate::util::truncate_chars(args.cmd.trim(), 300);
            status.cmd_path = Some(path_string(path));
        }
    }
    command
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
    // `KELIR_TOKEN` is not copied here: the shim was started with it in its
    // own environment and the command inherits that environment, so the token
    // is never in an argv and never written to a status file (STEER-7).
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
            fail(
                &dir,
                &status,
                key,
                &format!("cannot start {}: {error}", argv[0]),
            );
            return 1;
        }
    };
    let Some(pid) = child.id() else {
        fail(&dir, &status, key, "the command started without a pid");
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
                key,
                &format!("cannot control the command's tree: {error}"),
            );
            return 1;
        }
        // A `kelir bg run` job outlives everything by definition: let the
        // command hold the job handle too, so the job's name survives the shim
        // and `cancel` can still open it after the shim is killed (M4 review
        // round 2, G2). A call gets the same handle at promotion, never before:
        // until then its job must stay kill-on-close (M4 review F6).
        if args.kind == KIND_JOB
            && let Err(error) = job.lend_to(pid)
        {
            eprintln!("kelir bg-shim: {error}");
        }
    }
    status.pid = Some(pid);
    // The command's own start time, read here once: `cancel` uses it to prove
    // that the pid it walks a lost job's tree from is the command, not its
    // recycled pid (M4 review round 2, G2). It is read while the command is
    // still suspended on Windows, which the kernel answers like any other.
    status.pid_started = start_time_text(pid);
    if let Err(error) = status.write_with(&status_path(&dir, &args.id), key) {
        eprintln!("kelir bg-shim: cannot write the status file: {error}");
    }
    // Report to the session ourselves: the capability is ours alone (it came
    // down the pipe), so this is the report only this job's shim can make.
    if let (Some(sock), Some(capability)) = (args.sock.as_deref(), secrets.capability.as_deref())
        && args.kind == KIND_JOB
    {
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            notify(&status, sock, Some(capability)),
        )
        .await;
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
            if let Err(error) = status.write_with(&status_file, key) {
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
            if let Err(error) = status.write_with(&status_file, key) {
                eprintln!("kelir bg-shim: cannot write the status file: {error}");
            }
            // The tree is ours now. Let the command hold the job handle so the
            // job's name outlives this shim: a lost job is only reachable by
            // name, and the name dies with its last handle (M4 review round 2,
            // G2).
            #[cfg(windows)]
            if let Some(job) = &job
                && let Err(error) = job.lend_to(pid)
            {
                eprintln!("kelir bg-shim: {error}");
            }
            #[cfg(not(windows))]
            let _ = pid;
        }
        if !promoted && owner.gone() {
            // The session that owns this call is gone and never promoted it:
            // take the tree down, as the session would have. The status goes
            // first, because on Unix the kill includes this process.
            status.state = STATE_CANCELLED.into();
            status.finished = Some(crate::event::now_ts());
            status.error = Some("the session exited".into());
            let _ = status.write_with(&status_file, key);
            kill_tree(std::process::id(), &args.job);
            return 1;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Make this process's memory unreadable to other processes of the same user,
/// as far as the platform allows. Best effort: a failure is a note on stderr,
/// not a reason to not run the job.
fn harden_process() {
    #[cfg(windows)]
    if let Err(error) = crate::proc::win::deny_same_user_memory_access() {
        eprintln!("kelir bg-shim: {error}");
    }
    #[cfg(target_os = "linux")]
    // SAFETY: prctl(PR_SET_DUMPABLE, 0) only changes this process's flag.
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
}

/// What the starter wrote down the shim's stdin pipe: the signing key and the
/// capability, one per line. Held only in this process; overwritten when the
/// shim ends. (A process of the same user that can read this process's memory
/// can read them while the shim lives: see SPEC, "what the boundary is".)
#[derive(Default)]
struct Secrets {
    key: Option<String>,
    capability: Option<String>,
}

impl Drop for Secrets {
    fn drop(&mut self) {
        for secret in [self.key.take(), self.capability.take()]
            .into_iter()
            .flatten()
        {
            let mut bytes = secret.into_bytes();
            bytes.fill(0);
            std::hint::black_box(&bytes);
        }
    }
}

fn read_secrets() -> Secrets {
    use std::io::BufRead as _;
    let mut input = std::io::stdin().lock();
    let mut line = || {
        let mut text = String::new();
        input.read_line(&mut text).ok()?;
        let trimmed = text.trim().to_string();
        let kept = (!trimmed.is_empty()).then(|| trimmed.clone());
        let mut bytes = text.into_bytes();
        bytes.fill(0);
        std::hint::black_box(&bytes);
        kept
    };
    let key = line();
    let capability = line();
    Secrets { key, capability }
}

/// The command has exited; stay as the tree's owner while the tree still has a
/// process in it, so the owner's death can take those leftovers down with one
/// `killpg`. Exits immediately when nothing is left (the common case), and on
/// platforms where the group cannot be counted it does not linger at all.
///
/// The owner check is a `getppid()` (one cheap syscall, kept at [`POLL`]); the
/// group count is a walk of `/proc` (one `stat` read per process) and is what
/// used to cost a quarter of a core per lingering shim, so it backs off from
/// 25 ms to [`LINGER_MAX`] while the group stays non-empty (M4 review round 2,
/// G4). A session killed at any moment is still noticed within [`POLL`].
#[cfg(target_os = "linux")]
fn linger(owner: &Owner) {
    // A `kelir bg run` job has no owner to watch: nothing to protect it from,
    // so the shim exits with the command.
    if owner.pid.is_none() {
        return;
    }
    let mut interval = POLL;
    let mut scan_at = std::time::Instant::now();
    loop {
        if owner.gone() {
            kill_tree(std::process::id(), &None);
            return;
        }
        if std::time::Instant::now() >= scan_at {
            match crate::proc::group_others(std::process::id()) {
                // Nothing but this shim is left in the group: done.
                Some(0) => return,
                Some(_) => {
                    interval = (interval * 2).min(LINGER_MAX);
                    scan_at = std::time::Instant::now() + interval;
                }
                // The platform cannot say: do not hold a process for nothing.
                None => return,
            }
        }
        let wait = scan_at
            .saturating_duration_since(std::time::Instant::now())
            .min(POLL);
        std::thread::sleep(wait);
    }
}

/// The longest a lingering shim waits between looks at its process group.
#[cfg(target_os = "linux")]
pub const LINGER_MAX: Duration = Duration::from_secs(1);

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
fn fail(dir: &Path, status: &Status, key: Option<&str>, message: &str) {
    let mut status = status.clone();
    status.state = STATE_EXITED.into();
    status.exit_code = None;
    status.finished = Some(crate::event::now_ts());
    status.error = Some(message.to_string());
    let _ = status.write_with(&status_path(dir, &status.id), key);
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
    // With a session, the session starts the shim and mints everything the
    // job is trusted with (id, capability, signing key): none of it is ever
    // returned to this process, which is a tool child (STEER-7 review round 3).
    // Without one (a bare shell) the job is a local record and this process
    // starts the shim itself.
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
            token: crate::trust::token_from_env(),
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
        let cmd_file = match crate::shellcmd::input(&command, &shell_args, &dir, &id) {
            Ok(crate::shellcmd::ShellInput::Argument(_)) => None,
            Ok(crate::shellcmd::ShellInput::Script { path, .. }) => Some(path),
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
            token: None,
            key: None,
            capability: None,
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

/// Ask the session to start this job's shim. The answer is the job's id and
/// nothing else: the session keeps the capability and the signing key and gives
/// them to the shim it starts.
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

/// Tell the session about a job over its socket, so it records `bg_started`
/// straight away instead of noticing the file on its next poll. Best effort:
/// a session that is gone, busy or not there is what the poller and `resume`
/// are for.
async fn notify(status: &Status, sock: &str, capability: Option<&str>) {
    let args = match serde_json::to_value(status) {
        Ok(args) => args,
        Err(_) => return,
    };
    // `request_value` adds `KELIR_TOKEN` when the request has none (STEER-7):
    // a report without it is refused by the session's socket. The capability is
    // this job's own — the one the session minted for it a moment ago — and is
    // the half that proves the report is this job's shim's and nobody else's.
    let capability = capability.map(str::to_string);
    let request = crate::ipc::Request::new("bg", args, "job").with_capability(capability);
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
        follow(&path, &status_path(&dir, &status.id), &dir)
    }
}

/// `--follow`: print the file as it grows, and stop once the job settled and
/// nothing new arrived. Only the bytes past the offset are read, so following a
/// job that writes a lot costs the same as following a quiet one (M4 review N1).
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
            // (M4 review F4) — unless a `cancel` is in flight for it.
            Some(status) => observed_in(&dir, &status),
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
/// checked against the start time the *shim recorded for itself*, so a status
/// that outlived its shim (a reboot, `taskkill /F`) cannot make kelir kill
/// whatever inherited the pid (M4 review F5, and G1 of the round-2 review: the
/// old check compared against the job's `started`, with two minutes of slack).
/// A job whose shim is gone is `lost`, and [`stop_lost`] still reaches its tree.
pub async fn cancel(id: &str) -> u8 {
    let Some((dir, _status)) = find(id) else {
        eprintln!("kelir: no background job `{id}`");
        return 2;
    };
    let mut out = Console;
    cancel_at(&dir, id, None, &mut out).await
}

/// Where `cancel`'s sentences go: a human's terminal (`kelir bg cancel`), or the
/// session's reply (`cancel` as a verb, STEER-7).
trait CancelOut {
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

/// The kill itself, for one job in one `bg/` directory.
async fn cancel_at(dir: &Path, id: &str, key: Option<&str>, out: &mut dyn CancelOut) -> u8 {
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
    // Only a pid that is provably the shim is signalled. `observed` has already
    // turned every other answer into `lost`; this is the platform that cannot
    // prove anything, and it is refused rather than guessed (G1).
    if shim_identity(&status) != ShimIdentity::Same {
        out.fail(format!(
            "{}: this platform cannot report process start times, so pid {} cannot be proved to \
             be the job's shim; nothing was signalled",
            status.id,
            status.shim.map(|pid| pid.to_string()).unwrap_or_default()
        ));
        return 1;
    }
    // From here until the `cancelled` status is written the status file still
    // says `running` with a dead shim; any reader in that window would call the
    // job `lost` (the session's watcher did, and the log said so — STEER-7).
    // The marker says "a cancel is in flight", and the guard removes it on
    // every way out, including the ones that change nothing.
    let _marker = CancelMarker::write(dir, &status.id);
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
    final_status.finished = Some(crate::event::now_ts());
    match final_status.write_with(&status_path(dir, &final_status.id), key) {
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
struct CancelMarker {
    path: std::path::PathBuf,
}

impl CancelMarker {
    fn write(dir: &Path, id: &str) -> Self {
        let path = cancel_path(dir, id);
        let _ = std::fs::write(&path, b"cancelling\n");
        Self { path }
    }
}

impl Drop for CancelMarker {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The session's `cancel` verb: the same kill, in *this session's* `bg/`, with
/// the answer as a sentence (the client's `kelir ctl` prints it). Never a
/// search across sessions: the caller has already proved the job is this
/// session's own (STEER-7), so only its own `bg/` is looked at.
pub async fn cancel_in(dir: &Path, id: &str, key: Option<&str>) -> Result<String, String> {
    if !crate::trust::valid_job_id(id) {
        return Err(format!("`{id}` is not a job id"));
    }
    if Status::read(&status_path(dir, id)).is_none() {
        return Err(format!("this session has no background job `{id}`"));
    }
    let mut sink = Sink::default();
    let code = cancel_at(dir, id, key, &mut sink).await;
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

/// A job whose shim is gone is `lost`: its status file will never be written
/// again, but its tree may still be running (the shim was killed with the job
/// mid-flight). `cancel` reaches it by what the status *proves*: the shim's
/// process group on Unix, the job object's name or a walk from the command's
/// own pid on Windows. When nothing can be proved it says exactly what it could
/// not do, and signals nothing (M4 review round 2, G2).
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
    if process_alive(shim) && shim_identity(status) != ShimIdentity::Same {
        out.fail(format!(
            "{} is lost and pid {shim} now belongs to another process; nothing was signalled",
            status.id
        ));
        return 1;
    }
    match crate::proc::group_members(shim) {
        Some(members) if !members.is_empty() => {
            // The group is led by the dead shim's pid; a pid number cannot be
            // reused while a group with that number still has a member, so this
            // group is the job's tree and nothing else's.
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
        && let Some(job) = crate::proc::win::Job::open(name)
    {
        job.terminate_job();
        out.line(format!(
            "{} lost (its shim is gone; terminated the job object holding its tree)",
            status.id
        ));
        return 0;
    }
    // Then the command's own pid, proved by the start time the shim recorded:
    // the job object's name died with its last handle, but the tree is still
    // there and its parent links say which processes it holds.
    match pid_identity(status) {
        ShimIdentity::Same => {
            let pid = status.pid.unwrap_or(0);
            let killed = crate::proc::terminate_tree(pid);
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
        pubkey: None,
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
            shim_started: None,
            pid_started: None,
            exit_code: None,
            started: "2026-11-19T10:00:00Z".into(),
            finished: None,
            cmd: "sleep 60".into(),
            cmd_path: None,
            out: "bg/bg_1.out".into(),
            kind: KIND_JOB.into(),
            promoted: false,
            call: None,
            job: Some("kelir-bg-job-bg_1".into()),
            error: None,
            sig: None,
            seq: 0,
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

    /// A `cancel` in flight is not a lost job (STEER-7): between the kill and
    /// the `cancelled` status the shim is dead and the file still says
    /// `running`, so every reader has to be told what is happening. The
    /// session's own watcher recorded `lost` for a job a test had just
    /// cancelled, which is how this was found.
    #[test]
    fn a_cancel_in_flight_is_not_a_lost_job() {
        let dir = temp_dir("cancelling");
        let status = sample();
        // No shim at all, so `observed` alone would call it lost.
        assert_ne!(status.observed().state, STATE_RUNNING);
        assert_ne!(observed_in(&dir, &status).state, STATE_RUNNING);

        let marker = CancelMarker::write(&dir, &status.id);
        assert!(cancelling(&dir, &status.id));
        assert_eq!(observed_in(&dir, &status).state, STATE_RUNNING);
        // Every way out of `cancel_at` clears the marker, including a refusal.
        std::fs::remove_file(status_path(&dir, &status.id)).ok();
        drop(marker);
        assert!(!cancelling(&dir, &status.id));
        assert_ne!(observed_in(&dir, &status).state, STATE_RUNNING);

        // A settled job stays settled whatever a stale marker says.
        let mut settled = sample();
        settled.state = STATE_CANCELLED.into();
        let marker = CancelMarker::write(&dir, &settled.id);
        assert_eq!(observed_in(&dir, &settled).state, STATE_CANCELLED);
        drop(marker);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A marker left behind by a `cancel` that was itself killed must not keep a
    /// dead job `running` for ever: past its time to live it is ignored.
    #[test]
    fn a_stale_cancel_marker_is_ignored() {
        let dir = temp_dir("stale-cancel");
        let status = sample();
        let marker = CancelMarker::write(&dir, &status.id);
        assert!(cancelling(&dir, &status.id));
        // Backdate the marker past its life, the way a killed cancel would.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(cancel_path(&dir, &status.id))
            .unwrap();
        file.set_modified(std::time::SystemTime::now() - CANCEL_MARKER_TTL * 2)
            .unwrap();
        assert!(!cancelling(&dir, &status.id));
        assert_ne!(observed_in(&dir, &status).state, STATE_RUNNING);
        drop(marker);
        std::fs::remove_dir_all(&dir).ok();
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

    /// The identity check itself: a status is only the shim's when it records
    /// the shim's **own** start time. A pid whose process started at another
    /// time is somebody else, however close the job's `started` is.
    #[test]
    fn a_status_is_matched_with_its_own_process_not_just_its_pid() {
        let me = std::process::id();
        let mine = start_time_text(me);
        let mut status = sample();
        status.shim = Some(me);
        status.started = crate::event::now_ts();
        status.shim_started = mine.clone();
        assert_eq!(shim_identity(&status), ShimIdentity::Same);
        assert_eq!(status.observed().state, STATE_RUNNING);

        // The process is ours, but the status does not say when it started.
        status.shim_started = None;
        assert_eq!(shim_identity(&status), ShimIdentity::NoRecord);
        assert_eq!(
            status.observed().state,
            if crate::proc::start_time(me).is_some() {
                // The platform can tell, so "no record" is not proof of life.
                STATE_LOST
            } else {
                STATE_RUNNING
            }
        );

        if mine.is_some() {
            // A pid whose process started years after the recorded time is a
            // recycled pid, even though the job's own `started` is close.
            status.shim_started = Some("2020-01-01T00:00:00Z".into());
            status.started = crate::event::now_ts();
            assert_eq!(shim_identity(&status), ShimIdentity::Different);
            assert_eq!(status.observed().state, STATE_LOST);
            // Five minutes of job lifetime used to be inside the tolerance:
            // the answer must not depend on `started` at all.
            status.started = (time::OffsetDateTime::now_utc() - time::Duration::seconds(45))
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap();
            assert_eq!(shim_identity(&status), ShimIdentity::Different);
        }

        // No shim at all is nobody's.
        status.shim = None;
        assert_eq!(shim_identity(&status), ShimIdentity::NoRecord);
        assert_ne!(status.observed().state, STATE_RUNNING);
    }

    /// The command's own record is what `cancel` proves a lost job's tree by:
    /// without it, nothing is signalled (M4 review round 2, G1/G2).
    #[test]
    fn a_command_pid_is_only_trusted_with_its_own_start_time() {
        let me = std::process::id();
        let mut status = sample();
        status.pid = Some(me);
        status.pid_started = start_time_text(me);
        assert_eq!(pid_identity(&status), ShimIdentity::Same);
        status.pid_started = Some("2020-01-01T00:00:00Z".into());
        assert_eq!(pid_identity(&status), ShimIdentity::Different);
        status.pid_started = None;
        assert_eq!(pid_identity(&status), ShimIdentity::NoRecord);
        status.pid = Some(0);
        assert_eq!(pid_identity(&status), ShimIdentity::NoRecord);
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
