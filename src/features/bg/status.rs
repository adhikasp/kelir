//! Background jobs: the shim, the status files, and the `kelir bg` CLI.
//! A job is one `kelir bg-shim` process (the same binary) that spawns the command, hands it the
//! output file directly and writes `bg/<id>.status` whenever its state changes.
//! The shim owns the command's process tree: on Unix both are in the shim's process group
//! (`setsid` for a job), on Windows the command is assigned to a named Job Object the shim keeps

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::util::path_string;

/// How often the shim looks at its command, its markers and its owner.
pub const POLL: Duration = Duration::from_millis(25);
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
/// The shim is gone and nothing will ever write this status again (`taskkill /F`, an OOM kill, a
/// reboot).
pub const STATE_LOST: &str = "lost";

/// How far a recorded start time may be from the process's own and still be the same process.
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
    /// When the shim itself started, as the shim read it from the OS at launch (`proc::start_time`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shim_started: Option<String>,
    /// When the command started, read from the OS right after the spawn: the
    /// proof `cancel` needs to reach a lost job's tree by the command's pid
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_started: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub started: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<String>,
    #[serde(default)]
    pub cmd: String,
    /// Where a long command was written instead of being passed as an argument (`cmd.sh`/`cmd.bat` in the
    /// job's directory).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd_path: Option<String>,
    #[serde(default)]
    pub out: String,
    /// `job` or `call`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub promoted: bool,
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
            shim_started: None,
            pid_started: None,
            exit_code: None,
            started: crate::log::event::now_ts(),
            finished: None,
            cmd: cmd.to_string(),
            cmd_path: None,
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

    /// The status as a reader should understand it: a `running` job whose shim is gone — or whose shim
    /// cannot be proved to be the process holding its pid — is [`STATE_LOST`], because nothing will ever
    pub fn observed(&self) -> Status {
        if self.state != STATE_RUNNING {
            return self.clone();
        }
        let error = match shim_identity(self) {
            // Alive, and its own start time is the recorded one (or the platform cannot report start times at all:
            // liveness is the best answer available there, and signalling is refused instead).
            ShimIdentity::Same | ShimIdentity::NoPlatform => return self.clone(),
            ShimIdentity::Different => "the job's shim is gone; its outcome is unknown",
            ShimIdentity::NoRecord => {
                "the job's status records no shim start time, so the shim's identity cannot be \
                 checked; its outcome is unknown"
            }
        };
        let mut lost = self.clone();
        lost.state = STATE_LOST.into();
        lost.finished = Some(
            self.finished
                .clone()
                .unwrap_or_else(crate::log::event::now_ts),
        );
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

/// Is the process holding `status.shim`'s pid really the shim that wrote the status?
pub fn shim_identity(status: &Status) -> ShimIdentity {
    identity(status.shim, status.shim_started.as_deref())
}

/// The same question for the command's own pid (`pid_started`): only `cancel` on Windows
/// walks a lost job's tree by it, and the unit tests ask on every platform.
#[cfg(any(windows, test))]
pub(crate) fn pid_identity(status: &Status) -> ShimIdentity {
    identity(status.pid, status.pid_started.as_deref())
}

fn identity(pid: Option<u32>, recorded: Option<&str>) -> ShimIdentity {
    let Some(pid) = pid.filter(|pid| *pid != 0) else {
        return ShimIdentity::NoRecord;
    };
    // Liveness first: a killed process keeps its creation time until the
    // kernel object is reaped, so a start time alone would call it alive.
    if !crate::sys::proc::alive(pid) {
        return ShimIdentity::Different;
    }
    // Alive, but its start time is not ours to read (macOS).
    let Some(actual) = crate::sys::proc::start_time(pid) else {
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

/// A start time as the status file writes it: RFC 3339, or `None` where the platform cannot say (macOS)
/// — in which case nothing is ever signalled on the strength of a pid.
pub fn promotion_due(after: Duration, elapsed: Duration) -> bool {
    after > Duration::ZERO && elapsed >= after
}

/// `bg_<ulid>`: unique across processes, and the same shape as a session id.
pub fn new_id() -> String {
    format!("bg_{}", ulid::Ulid::generate().to_string().to_lowercase())
}

/// Where jobs live: this session's `calls/` inside a bash call, otherwise the
/// global `calls/` for a job with no session. Both are one directory per call.
pub fn root() -> PathBuf {
    if let Some(dir) = crate::envs::var_os("KELIR_SESSION_DIR")
        .map(PathBuf::from)
        .filter(|dir| !dir.as_os_str().is_empty())
    {
        return crate::paths::calls_dir(&dir);
    }
    crate::paths::global_calls_dir()
}

/// Every job owns exactly one directory: `<root>/<id>/`. Its status, output,
/// cancel marker and command script all live there. No reader looks anywhere else.
pub fn job_dir(root: &Path, id: &str) -> PathBuf {
    root.join(id)
}

/// Where a job's final record is also kept: `<id>.final`, written by the shim
/// (or the session's `cancel`) when the job settles.
pub fn final_path(dir: &Path, id: &str) -> PathBuf {
    status_path(dir, id).with_extension("final")
}

pub fn status_path(dir: &Path, id: &str) -> PathBuf {
    job_dir(dir, id).join(format!("{id}.status"))
}

pub fn out_path(dir: &Path, id: &str) -> PathBuf {
    job_dir(dir, id).join(format!("{id}.out"))
}

pub fn promote_path(dir: &Path, id: &str) -> PathBuf {
    job_dir(dir, id).join(format!("{id}.promote"))
}

/// How long a `cancel` marker is believed. A cancel takes at most
/// [`CANCEL_WAIT`]; the rest is slack for a slow box, not for a stale file.
pub const CANCEL_MARKER_TTL: Duration = Duration::from_secs(30);

/// The marker a `cancel` writes before it kills anything.
pub fn cancel_path(dir: &Path, id: &str) -> PathBuf {
    job_dir(dir, id).join(format!("{id}.cancel"))
}

/// Is a `cancel` in flight for this job?
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

/// A status as a reader should understand it, given the job root it was read from:
/// [`Status::observed`], except that a job whose `cancel` marker is present is still `running` (it is
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

/// Every status file in `dir`, newest first, skipping temp files and anything that does not parse.
pub fn list(dir: &Path) -> Vec<Status> {
    let mut out: Vec<Status> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| {
            let id = path.file_name()?.to_str()?.to_string();
            Status::read(&status_path(dir, &id))
        })
        .map(|status| observed_in(dir, &status))
        .collect();
    out.sort_by(|a, b| b.started.cmp(&a.started).then(a.id.cmp(&b.id)));
    out
}

/// Find a job by id: this session's calls root first, then any session's (so
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
        let dir = crate::paths::calls_dir(&crate::paths::session_dir(&session));
        if let Some(status) = Status::read(&status_path(&dir, id)) {
            return Some((dir, status));
        }
    }
    None
}

/// Ask for a promotion: an empty marker file the shim polls for.
pub fn mark_promoted(dir: &Path, id: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(job_dir(dir, id))?;
    std::fs::write(promote_path(dir, id), b"promoted\n")
}
