//! Children as a CLI: the registry, the caps, the hand-off to a child process and the pieces of
//! `kelir run --child`, `kelir brief`, `kelir ls` and `kelir kill` that do not need the session.
//! There is no spawn tool.
//! A child is a `kelir` process the **session** starts on behalf of a `bash` call, so it lands in
//! the session's process group (Unix) or Job Object (Windows) and dies with it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::event::{EventBody, LogEvent};

pub const DEFAULT_MAX_DEPTH: u32 = 3;
pub const DEFAULT_MAX_CHILDREN: u32 = 10;
pub const DEFAULT_MAX_LIVE: u32 = 16;

/// The internal verb every child operation travels as (`op` says which).
pub const VERB: &str = "child";
/// The `from` of a request a session sends to its own loop.
pub const FROM_INTERNAL: &str = "child-internal";
pub const KIND_CHILD: &str = "child";
pub const KIND_BRIEF: &str = "brief";
/// `kelir run --child` exit code for a start a cap refused.
pub const EXIT_REFUSED: u8 = 3;
/// How long a foreground `kelir run --child` waits for its answer.
pub const FOREGROUND_WAIT: Duration = Duration::from_secs(24 * 60 * 60);
/// A child that was admitted but never reported a pid is reaped after this.
const NO_PID_GRACE: Duration = Duration::from_secs(120);
pub const RESULT_LIMIT: usize = 1024 * 1024;

// ------------------------------------------------------------------ lineage

/// Who this session is in the tree: exported to its tool children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lineage {
    pub root: String,
    pub parent: Option<String>,
    pub depth: u32,
    /// This session's own id (what a tool child's `KELIR_PARENT` is).
    pub me: String,
}

static LINEAGE: OnceLock<Lineage> = OnceLock::new();

/// Remember the lineage of the session in this process (once).
pub fn set_lineage(lineage: Lineage) {
    let _ = LINEAGE.set(lineage);
}

/// The environment a tool child gets, beside `KELIR_SOCK`/`KELIR_TOKEN`.
pub fn lineage_env() -> Vec<(&'static str, String)> {
    match LINEAGE.get() {
        Some(lineage) => vec![
            ("KELIR_ROOT", lineage.root.clone()),
            ("KELIR_PARENT", lineage.me.clone()),
            ("KELIR_DEPTH", lineage.depth.to_string()),
        ],
        None => Vec::new(),
    }
}

// --------------------------------------------------------------------- caps

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// The deepest a child may be (the root is 0).
    pub depth: u32,
    pub per_agent: u32,
    pub live: u32,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            depth: DEFAULT_MAX_DEPTH,
            per_agent: DEFAULT_MAX_CHILDREN,
            live: DEFAULT_MAX_LIVE,
        }
    }
}

/// Why a start was refused: the message says which cap and the count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Depth { wanted: u32, cap: u32 },
    PerAgent { have: usize, cap: u32 },
    Live { have: usize, cap: u32 },
    UnknownParent,
}

impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Refusal::Depth { wanted, cap } => format!(
                "refused: this child would be at depth {wanted}; the deepest allowed is {cap} \
                 (`max_child_depth`; the root is depth 0). Do the work in this agent instead"
            ),
            Refusal::PerAgent { have, cap } => format!(
                "refused: this agent already has {have} live children (limit {cap}, \
                 `max_children`). Wait for one to finish or stop one with `kelir kill`"
            ),
            Refusal::Live { have, cap } => format!(
                "refused: this tree already has {have} live descendants (limit {cap}, \
                 `max_live_descendants`). Wait for some to finish or stop one with `kelir kill`"
            ),
            Refusal::UnknownParent => {
                "refused: this agent is not in the root's registry (it may have settled)".into()
            }
        }
    }
}

// ----------------------------------------------------------------- registry

#[derive(Debug, Clone)]
pub struct Entry {
    pub child_id: String,
    pub session: String,
    pub parent: String,
    pub depth: u32,
    pub kind: String,
    pub background: bool,
    pub task: String,
    pub model: String,
    pub pid: Option<u32>,
    /// The child process's start time (ms since the epoch), set with the pid.
    pub started_ms: Option<u64>,
    pub outcome: Option<String>,
    /// A kill was aimed at it: the outcome it settles with.
    pub killed: bool,
    admitted: Instant,
    /// When the process was first seen gone without having been settled.
    dead_since: Option<Instant>,
}

impl Entry {
    pub fn live(&self) -> bool {
        self.outcome.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct Admission {
    pub child_id: String,
    pub session: String,
    pub depth: u32,
}

pub struct NewChild<'a> {
    pub parent: &'a str,
    pub kind: &'a str,
    pub background: bool,
    pub task: &'a str,
    pub model: &'a str,
    pub session: String,
}

/// The root's record of every descendant. Pure data: the session writes the
/// events, this decides.
#[derive(Debug)]
pub struct Registry {
    root: String,
    caps: Caps,
    entries: Vec<Entry>,
}

impl Registry {
    pub fn new(root: &str, caps: Caps) -> Self {
        Registry {
            root: root.to_string(),
            caps,
            entries: Vec::new(),
        }
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn caps(&self) -> Caps {
        self.caps
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The depth of a session in this tree: the root is 0, a descendant is its entry's depth.
    pub fn depth_of(&self, session: &str) -> Option<u32> {
        if session == self.root {
            return Some(0);
        }
        self.entries
            .iter()
            .find(|entry| entry.session == session && entry.live())
            .map(|entry| entry.depth)
    }

    pub fn live_descendants(&self) -> usize {
        self.entries.iter().filter(|entry| entry.live()).count()
    }

    pub fn live_children_of(&self, session: &str) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.live() && entry.parent == session)
            .count()
    }

    /// Check the caps and, if they allow it, take the slot. One call, so two
    /// starts racing for the last slot cannot both get it.
    pub fn admit(&mut self, new: NewChild<'_>) -> Result<Admission, Refusal> {
        let parent_depth = self.depth_of(new.parent).ok_or(Refusal::UnknownParent)?;
        let depth = parent_depth + 1;
        if depth > self.caps.depth {
            return Err(Refusal::Depth {
                wanted: depth,
                cap: self.caps.depth,
            });
        }
        let mine = self.live_children_of(new.parent);
        if mine >= self.caps.per_agent as usize {
            return Err(Refusal::PerAgent {
                have: mine,
                cap: self.caps.per_agent,
            });
        }
        let live = self.live_descendants();
        if live >= self.caps.live as usize {
            return Err(Refusal::Live {
                have: live,
                cap: self.caps.live,
            });
        }
        let child_id = format!("chd_{}", crate::trust::random_hex(16));
        self.entries.push(Entry {
            child_id: child_id.clone(),
            session: new.session.clone(),
            parent: new.parent.to_string(),
            depth,
            kind: new.kind.to_string(),
            background: new.background,
            task: new.task.to_string(),
            model: new.model.to_string(),
            pid: None,
            started_ms: None,
            outcome: None,
            killed: false,
            admitted: Instant::now(),
            dead_since: None,
        });
        Ok(Admission {
            child_id,
            session: new.session,
            depth,
        })
    }

    /// Record the child's pid and start time once its process exists.
    pub fn up(&mut self, child_id: &str, pid: u32, started_ms: Option<u64>) -> bool {
        match self.find_mut(child_id) {
            // Once: a child's pid is told by its starter, then it stays.
            Some(entry) if entry.live() && entry.pid.is_none() => {
                entry.pid = Some(pid);
                entry.started_ms = started_ms;
                true
            }
            _ => false,
        }
    }

    /// Settle a child (first time only). A child a kill was aimed at settles
    /// `killed` whatever its exit looked like.
    pub fn settle(&mut self, child_id: &str, outcome: &str) -> Option<Entry> {
        let entry = self.find_mut(child_id)?;
        if !entry.live() {
            return None;
        }
        let outcome = if entry.killed { "killed" } else { outcome };
        entry.outcome = Some(outcome.to_string());
        Some(entry.clone())
    }

    /// Settle every live child `alive` says is gone (or never got a pid in time): a crashed child
    /// must not leak its slot. Returns what was reaped.
    pub fn reap(&mut self, alive: &dyn Fn(&Entry) -> bool, grace: Duration) -> Vec<Entry> {
        let mut reaped = Vec::new();
        for entry in &mut self.entries {
            if !entry.live() {
                continue;
            }
            let gone = match entry.pid {
                Some(_) if alive(entry) => {
                    entry.dead_since = None;
                    false
                }
                Some(_) => {
                    let since = *entry.dead_since.get_or_insert_with(Instant::now);
                    entry.killed || since.elapsed() >= grace
                }
                None => entry.admitted.elapsed() > NO_PID_GRACE,
            };
            if gone {
                entry.outcome = Some(if entry.killed { "killed" } else { "lost" }.into());
                reaped.push(entry.clone());
            }
        }
        reaped
    }

    pub fn get(&self, child_id: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.child_id == child_id)
    }

    fn find_mut(&mut self, child_id: &str) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|entry| entry.child_id == child_id)
    }

    /// Everything below `session`, nearest first.
    pub fn descendants_of(&self, session: &str) -> Vec<&Entry> {
        let mut out: Vec<&Entry> = Vec::new();
        let mut frontier = vec![session.to_string()];
        while let Some(parent) = frontier.pop() {
            for entry in self.entries.iter().filter(|entry| entry.parent == parent) {
                if out.iter().any(|seen| seen.child_id == entry.child_id) {
                    continue;
                }
                out.push(entry);
                frontier.push(entry.session.clone());
            }
        }
        out
    }

    /// Is `target` a (live or settled) descendant of `ancestor`?
    pub fn is_descendant(&self, ancestor: &str, target: &str) -> bool {
        self.descendants_of(ancestor)
            .iter()
            .any(|entry| entry.session == target || entry.child_id == target)
    }

    pub fn mark_killed(&mut self, child_id: &str) -> Option<Entry> {
        let entry = self.find_mut(child_id)?;
        if !entry.live() {
            return None;
        }
        entry.killed = true;
        Some(entry.clone())
    }

    /// Rebuild the live entries a log still holds as unsettled, as `lost`: a resumed root cannot
    /// authenticate or reach what an earlier process started.
    pub fn from_log(root: &str, caps: Caps, events: &[LogEvent]) -> (Registry, Vec<Entry>) {
        let mut registry = Registry::new(root, caps);
        let mut open: Vec<Entry> = Vec::new();
        for event in events {
            match &event.body {
                EventBody::ChildStarted {
                    child_id,
                    session,
                    parent,
                    depth,
                    kind,
                    task,
                    background,
                    model,
                } => open.push(Entry {
                    child_id: child_id.clone(),
                    session: session.clone(),
                    parent: parent.clone(),
                    depth: *depth,
                    kind: kind.clone(),
                    background: *background,
                    task: task.inline_str().unwrap_or("").to_string(),
                    model: model.clone(),
                    pid: None,
                    started_ms: None,
                    outcome: None,
                    killed: false,
                    admitted: Instant::now(),
                    dead_since: None,
                }),
                EventBody::ChildSettled { child_id, .. } => {
                    open.retain(|entry| &entry.child_id != child_id);
                }
                _ => {}
            }
        }
        for entry in &mut open {
            entry.outcome = Some("lost".into());
        }
        registry.entries = open.clone();
        (registry, open)
    }
}

/// A registry several threads may use: every call takes the lock, so the cap
/// arithmetic is atomic.
#[derive(Debug, Clone)]
pub struct Shared(Arc<Mutex<Registry>>);

impl Shared {
    pub fn new(registry: Registry) -> Self {
        Shared(Arc::new(Mutex::new(registry)))
    }

    pub fn with<T>(&self, work: impl FnOnce(&mut Registry) -> T) -> T {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        work(&mut guard)
    }
}

// ----------------------------------------------------------------- liveness

/// Milliseconds since the epoch for a process start time.
pub fn millis(time: std::time::SystemTime) -> u64 {
    time.duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// The start time of `pid` in ms, when the platform can say.
pub fn start_ms(pid: u32) -> Option<u64> {
    crate::proc::start_time(pid).map(millis)
}

/// Is `pid` the process that started at `started_ms`, and still running? A
/// recycled pid (a different start time) and a zombie both read as gone.
pub fn process_alive(pid: u32, started_ms: Option<u64>) -> bool {
    if pid == 0 {
        return false;
    }
    if !crate::proc::alive(pid) {
        return false;
    }
    #[cfg(target_os = "linux")]
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        && let Some(after) = stat.rsplit_once(')').map(|(_, rest)| rest)
        && after.trim_start().starts_with('Z')
    {
        return false;
    }
    match (started_ms, start_ms(pid)) {
        (Some(was), Some(now)) => was.abs_diff(now) <= 2_000,
        // The platform cannot say (macOS) or the process is not ours to look
        // at: liveness is all there is.
        _ => true,
    }
}

/// Kill a registered child's process tree. The pid comes from the registry (the spawner
/// reported it), never from a caller, and its start time must still match.
pub fn kill_registered(pid: u32, started_ms: Option<u64>) -> bool {
    if !process_alive(pid, started_ms) {
        return false;
    }
    #[cfg(unix)]
    {
        // The child host leads its own process group (`ProcessGroup::configure`).
        // SAFETY: a group this session's family created.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
        true
    }
    #[cfg(windows)]
    {
        crate::proc::terminate_tree(pid) > 0
    }
}

// ----------------------------------------------------------------- hand-off

/// What a spawner tells a child host over its stdin pipe. Holds secrets
/// (`root_token`), so `Debug` leaves it out.
#[derive(Clone, Serialize, Deserialize)]
pub struct Handoff {
    pub kind: String,
    pub child_id: String,
    pub session: String,
    pub task: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    pub cwd: String,
    pub parent: String,
    pub parent_pid: u32,
    #[serde(default)]
    pub parent_start_ms: Option<u64>,
    pub depth: u32,
    pub root: String,
    pub root_sock: String,
    pub root_token: String,
    /// Brief: the session id whose provider cache the snapshot shares.
    #[serde(default)]
    pub cache_id: Option<String>,
}

impl std::fmt::Debug for Handoff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handoff")
            .field("kind", &self.kind)
            .field("child_id", &self.child_id)
            .field("session", &self.session)
            .field("parent", &self.parent)
            .field("depth", &self.depth)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

/// How a child host reaches the root: kept in memory, never exported.
#[derive(Clone)]
pub struct RootChannel {
    pub sock: String,
    pub token: String,
}

impl std::fmt::Debug for RootChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootChannel")
            .field("sock", &self.sock)
            .finish_non_exhaustive()
    }
}

/// A started child host: the process, its group, and the pipe its answer comes
/// back on.
pub struct Host {
    pub child: tokio::process::Child,
    pub group: crate::proc::ProcessGroup,
    pub pid: u32,
}

/// Start `kelir child-host` for `handoff` and give it the hand-off on stdin.
pub async fn spawn_host(handoff: &Handoff, stderr_to: &Path) -> Result<Host, String> {
    use tokio::io::AsyncWriteExt as _;
    let binary = crate::util::self_binary()?;
    let mut command = crate::sys::runner::tokio_command(binary);
    command
        .arg("child-host")
        .current_dir(&handoff.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true);
    // A tool call's lineage is not the child's: it is told its own.
    for name in [
        "KELIR_SOCK",
        "KELIR_TOKEN",
        "KELIR_ROOT",
        "KELIR_PARENT",
        "KELIR_DEPTH",
        "KELIR_CALL_ID",
        "KELIR_SESSION_DIR",
    ] {
        command.env_remove(name);
    }
    match std::fs::File::create(stderr_to) {
        Ok(file) => {
            command.stderr(std::process::Stdio::from(file));
        }
        Err(_) => {
            command.stderr(std::process::Stdio::null());
        }
    }
    #[cfg(windows)]
    let group = crate::sys::runner::job(&format!("kelir-child-{}", handoff.child_id))?;
    #[cfg(not(windows))]
    let group = crate::sys::runner::job("")?;
    let mut child = crate::sys::runner::start_with_group(&mut command, &group)
        .map_err(|error| format!("cannot start the child: {error}"))?;
    let Some(pid) = child.id() else {
        return Err("the child exited at once".into());
    };
    if let Err(error) = group.spawned(pid) {
        let _ = child.kill().await;
        return Err(error);
    }
    let mut stdin = child.stdin.take().ok_or("the child has no stdin")?;
    let mut line = serde_json::to_string(handoff)
        .map_err(|error| format!("cannot serialise the hand-off: {error}"))?;
    line.push('\n');
    if let Err(error) = stdin.write_all(line.as_bytes()).await {
        group.kill();
        return Err(format!("cannot hand the child its task: {error}"));
    }
    let _ = stdin.flush().await;
    drop(stdin);
    Ok(Host { child, group, pid })
}

pub fn read_handoff() -> Result<Handoff, String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|error| format!("cannot read the hand-off: {error}"))?;
    serde_json::from_str(line.trim()).map_err(|error| format!("bad hand-off: {error}"))
}

/// Is the parent still there? Polled by a child host: a hard-killed parent
/// cannot signal its children, so they leave when it does.
pub fn parent_alive(handoff: &Handoff) -> bool {
    process_alive(handoff.parent_pid, handoff.parent_start_ms)
}

// ----------------------------------------------------------------- snapshot

/// How many events of `events` a brief copies: everything before the first assistant message whose tool
/// calls have not all settled (the call that is running `kelir brief` itself, and its siblings).
pub fn snapshot_cut(events: &[LogEvent]) -> usize {
    let settled: std::collections::HashSet<&str> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::ToolSettled { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    for (index, event) in events.iter().enumerate() {
        if let EventBody::Assistant { tool_calls, .. } = &event.body
            && tool_calls
                .iter()
                .any(|call| !settled.contains(call.id.as_str()))
        {
            return index;
        }
    }
    events.len()
}

/// Index of the first event that is a brief's own: its question (the last `user` event from
/// `brief`). Everything before it is the parent's copied log.
pub fn own_start(events: &[LogEvent]) -> usize {
    events
        .iter()
        .rposition(|event| matches!(&event.body, EventBody::User { from, .. } if from == "brief"))
        .unwrap_or(usize::MAX)
}

/// Seed `dest` (a new session dir) with a by-value copy of `source`'s log up to the snapshot cut, made
/// to read as the child's own: its `session_started` names the child, its parent and its depth.
pub fn seed_snapshot(
    source: &Path,
    dest: &Path,
    events: &[LogEvent],
    session: &str,
    parent: &str,
    depth: u32,
) -> Result<usize, String> {
    let cut = snapshot_cut(events);
    crate::paths::create_private_dir(&dest.join("commands"))
        .map_err(|error| format!("cannot create {}: {error}", dest.display()))?;
    for name in ["blobs", "attachments"] {
        let from = source.join(name);
        if from.is_dir() {
            copy_dir(&from, &dest.join(name))
                .map_err(|error| format!("cannot copy {}: {error}", from.display()))?;
        }
    }
    let mut text = String::new();
    for event in &events[..cut] {
        let mut event = event.clone();
        if let EventBody::SessionStarted {
            id,
            parent: was_parent,
            depth: was_depth,
            key_origin,
            ..
        } = &mut event.body
        {
            *id = session.to_string();
            *was_parent = Some(parent.to_string());
            *was_depth = depth;
            *key_origin = None;
        }
        // Tool-call history that refers to the parent's sockets is only text;
        // the live ones are cut away above.
        let line = serde_json::to_string(&event)
            .map_err(|error| format!("cannot copy event {}: {error}", event.seq))?;
        text.push_str(&line);
        text.push('\n');
    }
    // The snapshot is a log that ended cleanly, so opening it with a new task
    // starts a fresh turn instead of continuing one.
    let last = events[..cut].last().map(|event| event.seq).unwrap_or(0);
    let now = crate::event::now_ts();
    for (offset, body) in [
        json!({"type": "turn_ended", "outcome": "done", "final_text": ""}),
        json!({"type": "session_ended", "reason": "snapshot"}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut object = body;
        object["seq"] = json!(last + 1 + offset as u64);
        object["ts"] = json!(now);
        text.push_str(&object.to_string());
        text.push('\n');
    }
    std::fs::write(dest.join("events.jsonl"), text)
        .map_err(|error| format!("cannot write the snapshot: {error}"))?;
    Ok(cut)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// A child of this session settled: queued as a [`crate::verbs::Verb::Child`]
/// and applied at the next boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildNote {
    pub child_id: String,
    pub session: String,
    pub parent: String,
    pub depth: u32,
    pub background: bool,
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub result: String,
}

// --------------------------------------------------------------- the answer

/// The answer to a `kelir run --child` / `kelir brief` call, as the session
/// sends it and the CLI prints it.
pub fn answer(child_id: &str, session: &str, outcome: &str, exit_code: i32, result: &str) -> Value {
    let (text, truncated) = crate::util::truncate_bytes_boundary(result, RESULT_LIMIT);
    json!({
        "child_id": child_id,
        "session": session,
        "outcome": outcome,
        "exit_code": exit_code,
        "result": text,
        "truncated": truncated,
    })
}

/// `text` cut to at most `max` bytes on a character boundary. `text` for a request line to
/// the root, which reads at most 64 KB of one: cut on a character boundary with a marker.
pub fn clip(text: &str, max: usize) -> String {
    let (kept, cut) = crate::util::truncate_bytes_boundary(text, max);
    if cut {
        format!("{kept} … [cut]")
    } else {
        kept.to_string()
    }
}

pub fn outcome_of(exit_code: i32, signalled: bool) -> &'static str {
    if signalled {
        "killed"
    } else if exit_code == 0 {
        "done"
    } else {
        "error"
    }
}

pub fn succeeded(outcome: &str) -> bool {
    matches!(outcome, "done" | "paused")
}

/// A short, one-line form of a task for `ls` and the events' readers.
/// The directory of a child's session.
pub fn session_dir(session: &str) -> PathBuf {
    crate::paths::session_dir(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new<'a>(parent: &'a str, session: &str) -> NewChild<'a> {
        NewChild {
            parent,
            kind: KIND_CHILD,
            background: true,
            task: "work",
            model: "m",
            session: session.to_string(),
        }
    }

    fn registry(caps: Caps) -> Registry {
        Registry::new("root", caps)
    }

    #[test]
    fn the_root_is_depth_zero_and_a_child_is_one_deeper() {
        let mut registry = registry(Caps::default());
        let first = registry.admit(new("root", "c1")).unwrap();
        assert_eq!(first.depth, 1);
        let second = registry.admit(new("c1", "c2")).unwrap();
        assert_eq!(second.depth, 2);
        let third = registry.admit(new("c2", "c3")).unwrap();
        assert_eq!(third.depth, 3);
    }

    #[test]
    fn depth_four_is_refused_with_the_cap_in_the_message() {
        let mut registry = registry(Caps::default());
        registry.admit(new("root", "c1")).unwrap();
        registry.admit(new("c1", "c2")).unwrap();
        registry.admit(new("c2", "c3")).unwrap();
        let refused = registry.admit(new("c3", "c4")).unwrap_err();
        assert_eq!(refused, Refusal::Depth { wanted: 4, cap: 3 });
        assert!(refused.message().contains("depth 4"));
        assert!(refused.message().contains("max_child_depth"));
        assert_eq!(registry.live_descendants(), 3, "a refusal takes no slot");
    }

    #[test]
    fn the_eleventh_live_child_of_one_agent_is_refused() {
        let mut registry = registry(Caps::default());
        for index in 0..10 {
            registry.admit(new("root", &format!("c{index}"))).unwrap();
        }
        let refused = registry.admit(new("root", "c10")).unwrap_err();
        assert_eq!(refused, Refusal::PerAgent { have: 10, cap: 10 });
        assert!(refused.message().contains("10 live children"));
        // Another agent has its own ten.
        registry.admit(new("c0", "d0")).unwrap();
    }

    #[test]
    fn the_seventeenth_live_descendant_of_a_root_is_refused() {
        let mut registry = registry(Caps::default());
        for index in 0..10 {
            registry.admit(new("root", &format!("c{index}"))).unwrap();
        }
        for index in 0..6 {
            registry.admit(new("c0", &format!("d{index}"))).unwrap();
        }
        assert_eq!(registry.live_descendants(), 16);
        let refused = registry.admit(new("c1", "x")).unwrap_err();
        assert_eq!(refused, Refusal::Live { have: 16, cap: 16 });
        assert!(refused.message().contains("16 live descendants"));
    }

    #[test]
    fn a_settled_child_frees_its_slot() {
        let mut registry = registry(Caps {
            depth: 3,
            per_agent: 1,
            live: 16,
        });
        let first = registry.admit(new("root", "c1")).unwrap();
        assert!(registry.admit(new("root", "c2")).is_err());
        assert!(registry.settle(&first.child_id, "done").is_some());
        assert!(
            registry.settle(&first.child_id, "done").is_none(),
            "a child settles once"
        );
        registry.admit(new("root", "c2")).unwrap();
    }

    #[test]
    fn a_settled_agent_starts_nothing() {
        let mut registry = registry(Caps::default());
        let first = registry.admit(new("root", "c1")).unwrap();
        registry.settle(&first.child_id, "done");
        assert_eq!(
            registry.admit(new("c1", "c2")).unwrap_err(),
            Refusal::UnknownParent
        );
        assert_eq!(
            registry.admit(new("nobody", "c2")).unwrap_err(),
            Refusal::UnknownParent
        );
    }

    #[test]
    fn a_crashed_child_is_reaped_by_liveness_and_frees_its_slot() {
        let mut registry = registry(Caps {
            depth: 3,
            per_agent: 1,
            live: 16,
        });
        let first = registry.admit(new("root", "c1")).unwrap();
        registry.up(&first.child_id, 4242, Some(1));
        assert!(registry.reap(&|_| true, Duration::ZERO).is_empty());
        // Seen gone, but not for long enough: its starter may still settle it.
        assert!(
            registry
                .reap(&|_| false, Duration::from_secs(60))
                .is_empty()
        );
        let reaped = registry.reap(&|_| false, Duration::ZERO);
        assert_eq!(reaped.len(), 1);
        assert_eq!(reaped[0].outcome.as_deref(), Some("lost"));
        registry.admit(new("root", "c2")).unwrap();
    }

    #[test]
    fn a_killed_child_is_reaped_at_once_but_a_finished_one_gets_its_grace() {
        let mut registry = registry(Caps::default());
        let finished = registry.admit(new("root", "c1")).unwrap();
        let killed = registry.admit(new("root", "c2")).unwrap();
        registry.up(&finished.child_id, 1, None);
        registry.up(&killed.child_id, 2, None);
        registry.mark_killed(&killed.child_id).unwrap();
        let reaped = registry.reap(&|_| false, Duration::from_secs(60));
        assert_eq!(reaped.len(), 1);
        assert_eq!(reaped[0].child_id, killed.child_id);
        assert_eq!(reaped[0].outcome.as_deref(), Some("killed"));
        // Its starter settles it with the real outcome inside the grace.
        assert_eq!(
            registry
                .settle(&finished.child_id, "done")
                .unwrap()
                .outcome
                .as_deref(),
            Some("done")
        );
    }

    #[test]
    fn a_kill_makes_the_outcome_killed() {
        let mut registry = registry(Caps::default());
        let first = registry.admit(new("root", "c1")).unwrap();
        registry.mark_killed(&first.child_id).unwrap();
        let settled = registry.settle(&first.child_id, "error").unwrap();
        assert_eq!(settled.outcome.as_deref(), Some("killed"));
    }

    #[test]
    fn descendants_are_found_by_session_or_child_id_and_only_below_the_asker() {
        let mut registry = registry(Caps::default());
        let a = registry.admit(new("root", "a")).unwrap();
        let b = registry.admit(new("a", "b")).unwrap();
        registry.admit(new("root", "z")).unwrap();
        assert!(registry.is_descendant("root", "b"));
        assert!(registry.is_descendant("a", "b"));
        assert!(registry.is_descendant("a", &b.child_id));
        assert!(!registry.is_descendant("a", "z"), "z is a's cousin");
        assert!(
            !registry.is_descendant("b", "a"),
            "up the tree is not below"
        );
        assert_eq!(registry.descendants_of("a").len(), 1);
        assert_eq!(registry.descendants_of("root").len(), 3);
        let _ = a;
    }

    #[test]
    fn concurrent_admissions_never_pass_the_cap() {
        let shared = Shared::new(registry(Caps {
            depth: 3,
            per_agent: 7,
            live: 16,
        }));
        let admitted = Arc::new(Mutex::new(0usize));
        let handles: Vec<_> = (0..16)
            .map(|index| {
                let shared = shared.clone();
                let admitted = admitted.clone();
                std::thread::spawn(move || {
                    let result =
                        shared.with(|registry| registry.admit(new("root", &format!("c{index}"))));
                    if result.is_ok() {
                        *admitted.lock().unwrap() += 1;
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(*admitted.lock().unwrap(), 7);
        assert_eq!(shared.with(|registry| registry.live_descendants()), 7);
    }

    #[test]
    fn a_resumed_root_finds_the_unsettled_children_lost() {
        let started = |child: &str, seq: u64| {
            LogEvent::new(
                seq,
                EventBody::ChildStarted {
                    child_id: child.into(),
                    session: format!("ses_{child}"),
                    parent: "root".into(),
                    depth: 1,
                    kind: KIND_CHILD.into(),
                    task: "t".into(),
                    background: true,
                    model: "m".into(),
                },
            )
        };
        let settled = LogEvent::new(
            3,
            EventBody::ChildSettled {
                child_id: "a".into(),
                session: "ses_a".into(),
                parent: "root".into(),
                depth: 1,
                outcome: "done".into(),
                exit_code: Some(0),
                result: "ok".into(),
            },
        );
        let events = vec![started("a", 1), started("b", 2), settled];
        let (registry, lost) = Registry::from_log("root", Caps::default(), &events);
        assert_eq!(lost.len(), 1);
        assert_eq!(lost[0].child_id, "b");
        assert_eq!(
            registry.live_descendants(),
            0,
            "nothing is live after a resume"
        );
    }

    fn user(seq: u64) -> LogEvent {
        LogEvent::new(
            seq,
            EventBody::User {
                text: "hi".into(),
                from: "cli".into(),
                delivery: "next".into(),
                attachments: Vec::new(),
            },
        )
    }

    fn assistant(seq: u64, calls: &[&str]) -> LogEvent {
        LogEvent::new(
            seq,
            EventBody::Assistant {
                step: 1,
                text: "".into(),
                reasoning: None,
                tool_calls: calls
                    .iter()
                    .map(|id| crate::event::LoggedToolCall {
                        id: (*id).into(),
                        name: "bash".into(),
                        args: crate::event::Payload::value(json!({"command": "x"})),
                    })
                    .collect(),
                finish_reason: None,
            },
        )
    }

    fn tool_settled(seq: u64, call: &str) -> LogEvent {
        LogEvent::new(
            seq,
            EventBody::ToolSettled {
                call_id: call.into(),
                outcome: "ok".into(),
                exit_code: Some(0),
                bytes: 0,
                lines: 0,
                out_path: String::new(),
                spilled: false,
                result: "".into(),
            },
        )
    }

    #[test]
    fn a_snapshot_stops_before_the_batch_that_is_still_running() {
        let events = vec![
            user(1),
            assistant(2, &["a"]),
            tool_settled(3, "a"),
            assistant(4, &["b", "c"]),
            tool_settled(5, "c"),
        ];
        assert_eq!(
            snapshot_cut(&events),
            3,
            "b never settled, so the batch is cut"
        );
        let all = vec![user(1), assistant(2, &["a"]), tool_settled(3, "a")];
        assert_eq!(snapshot_cut(&all), 3, "a settled history is copied whole");
        assert_eq!(snapshot_cut(&[]), 0);
    }

    fn session_started(id: &str) -> LogEvent {
        LogEvent::new(
            1,
            EventBody::SessionStarted {
                id: id.into(),
                parent: None,
                depth: 0,
                cwd: "/tmp".into(),
                os: "linux".into(),
                kelir_version: "0.1.0".into(),
                provider: "openrouter".into(),
                model: "m".into(),
                system: crate::event::SystemInfo {
                    text: "SYS".into(),
                    hash: "h".into(),
                },
                sources: vec![],
                tools: vec!["bash".into(), "edit".into(), "compact".into()],
                reasoning_effort: None,
                key_origin: Some("env KEY".into()),
            },
        )
    }

    /// A snapshot is a copy by value: the child's log folds to the parent's prefix exactly, names
    /// the child and its parent, and changing either log afterwards touches only that log.
    #[test]
    fn a_snapshot_folds_to_the_parents_prefix_and_is_a_copy() {
        let base = std::env::temp_dir().join(format!("kelir-snap-{}", ulid::Ulid::generate()));
        let (source, dest) = (base.join("parent"), base.join("child"));
        std::fs::create_dir_all(source.join("blobs")).unwrap();
        std::fs::write(source.join("blobs").join("1-x"), "\"blob\"").unwrap();
        let mut events = vec![session_started("ses_parent")];
        let mut next = user(2);
        next.seq = 2;
        events.push(next);
        events.push(assistant(3, &["a"]));
        events.push(tool_settled(4, "a"));
        events.push(assistant(5, &["b"]));
        let copied = seed_snapshot(&source, &dest, &events, "ses_child", "ses_parent", 1).unwrap();
        assert_eq!(copied, 4, "the unsettled batch is cut");

        let text = std::fs::read_to_string(dest.join("events.jsonl")).unwrap();
        let child: Vec<LogEvent> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        match &child[0].body {
            EventBody::SessionStarted {
                id,
                parent,
                depth,
                key_origin,
                ..
            } => {
                assert_eq!(id, "ses_child");
                assert_eq!(parent.as_deref(), Some("ses_parent"));
                assert_eq!(*depth, 1);
                assert!(
                    key_origin.is_none(),
                    "the parent's key origin is not copied"
                );
            }
            other => panic!("{other:?}"),
        }
        let folded_parent = crate::fold::fold(&events[..4], &source).unwrap();
        let folded_child = crate::fold::fold(&child, &dest).unwrap();
        assert_eq!(folded_child.system, folded_parent.system);
        assert_eq!(
            serde_json::to_value(&folded_child.messages).unwrap(),
            serde_json::to_value(&folded_parent.messages).unwrap(),
            "the child's prefix is the parent's, byte for byte"
        );
        assert!(dest.join("blobs").join("1-x").is_file(), "blobs are copied");
        // By value: the parent's log growing later changes nothing here.
        let before = std::fs::read_to_string(dest.join("events.jsonl")).unwrap();
        events.push(tool_settled(6, "b"));
        assert_eq!(
            before,
            std::fs::read_to_string(dest.join("events.jsonl")).unwrap()
        );
        // Closed cleanly, so opening it with a task starts a fresh turn.
        assert!(matches!(
            child.last().map(|event| &event.body),
            Some(EventBody::SessionEnded { .. })
        ));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_handoff_debug_output_has_no_secret() {
        let handoff = Handoff {
            kind: KIND_CHILD.into(),
            child_id: "chd_1".into(),
            session: "ses_1".into(),
            task: "t".into(),
            model: None,
            provider: None,
            cwd: ".".into(),
            parent: "ses_0".into(),
            parent_pid: 1,
            parent_start_ms: None,
            depth: 1,
            root: "ses_0".into(),
            root_sock: "sock".into(),
            root_token: "CANARY-ROOT-TOKEN".into(),
            cache_id: None,
        };
        let shown = format!(
            "{handoff:?} {:?}",
            RootChannel {
                sock: "s".into(),
                token: "CANARY-ROOT-TOKEN".into(),
            }
        );
        assert!(!shown.contains("CANARY"), "{shown}");
    }

    #[test]
    fn this_process_is_alive_and_a_wrong_start_time_is_not_it() {
        let me = std::process::id();
        let started = start_ms(me);
        assert!(process_alive(me, started));
        if let Some(started) = started {
            assert!(!process_alive(me, Some(started + 60_000)), "a recycled pid");
        }
        assert!(!process_alive(0, None));
    }

    #[test]
    fn outcomes_follow_the_exit() {
        assert_eq!(outcome_of(0, false), "done");
        assert_eq!(outcome_of(1, false), "error");
        assert_eq!(outcome_of(0, true), "killed");
        assert!(succeeded("done") && succeeded("paused") && !succeeded("error"));
    }

    fn usage_event(seq: u64) -> LogEvent {
        LogEvent::new(
            seq,
            EventBody::Usage {
                step: 1,
                prompt_tokens: 1000,
                completion_tokens: 10,
                cached_tokens: 500,
                cache_write_tokens: 0,
                allowance: None,
                kind: None,
            },
        )
    }

    /// A brief's log starts with its parent's. `ls` names it a
    /// brief with its own question, and its tokens are only what it added.
    #[test]
    fn a_brief_is_listed_as_a_brief_and_counts_only_its_own_tokens() {
        let mut events = vec![session_started("ses_brief"), user(2), usage_event(3)];
        let plain = crate::childcli::node_from("ses_brief", &events, false).unwrap();
        assert_eq!(plain.kind, "session");
        assert_eq!(own_start(&events), usize::MAX);

        events.push(LogEvent::new(
            4,
            EventBody::User {
                text: "BRIEFQ why?".into(),
                from: "brief".into(),
                delivery: "next".into(),
                attachments: Vec::new(),
            },
        ));
        events.push(usage_event(5));
        assert_eq!(own_start(&events), 3);
        let node = crate::childcli::node_from("ses_brief", &events, false).unwrap();
        assert_eq!(node.kind, KIND_BRIEF);
        assert_eq!(node.task, "BRIEFQ why?");
        let tally = crate::usage::Tally::from_events(&events, Default::default());
        assert_eq!(tally.requests, 1);
        assert_eq!(tally.last.as_ref().map(|last| last.prompt), Some(1000));
    }

    /// A child's pid is told once; a second `up` cannot re-point it.
    #[test]
    fn up_is_once_only() {
        let mut registry = registry(Caps::default());
        let id = registry.admit(new("root", "ses_a")).unwrap().child_id;
        assert!(registry.up(&id, 100, Some(1)));
        assert!(!registry.up(&id, 200, Some(2)));
        assert_eq!(registry.get(&id).unwrap().pid, Some(100));
    }
}
