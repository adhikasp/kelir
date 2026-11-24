//! Tool-child process grouping and platform process-tree helpers.
#[cfg(windows)]
use crate::sys::proc::win;
use std::collections::HashMap;
#[cfg(windows)]
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::process::Command;

/// Is `pid` still a process? Pid 0 is never a process we own: on Unix it means
/// the caller's whole process group, so asking must not answer "alive" for it.
/// A process owned by another user is still alive: `kill(pid, 0)` reports EPERM
/// for it, and liveness must not depend on our right to signal it.
pub fn alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks that the process exists.
        (unsafe { libc::kill(pid as libc::pid_t, 0) }) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        match win::OwnerWatch::open(pid) {
            Some(watch) => !watch.gone(),
            None => false,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/// A start time as the status file writes it: RFC 3339, or `None` where the platform cannot
/// say (macOS) — in which case nothing is signalled on the strength of a pid.
pub fn start_time_text(pid: u32) -> Option<String> {
    time::OffsetDateTime::from(start_time(pid)?)
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

#[derive(Debug, Clone)]
pub struct ProcessGroup {
    state: Arc<GroupState>,
}

#[derive(Debug)]
struct GroupState {
    killed: AtomicBool,
    /// The tree was promoted to a background job: the session no longer owns
    /// it, so its `Drop`/`kill` must not take it down.
    detached: AtomicBool,
    /// Unix: the process group to signal. Windows: the shim's pid, whose job
    /// object holds the tree.
    pid: AtomicU32,
    #[cfg(windows)]
    job: Mutex<Option<win::Job>>,
}

/// A synchronous child's grouping (credential helpers): Unix process group, Windows job object.
pub struct StdGroup {
    pid: std::sync::atomic::AtomicU32,
    #[cfg(windows)]
    job: Option<win::Job>,
}

impl StdGroup {
    pub fn new() -> Result<Self, String> {
        #[cfg(windows)]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let n = NEXT.fetch_add(1, Ordering::SeqCst);
            let name = format!("kelir-helper-{}-{n}", std::process::id());
            let job = win::Job::create_named(&name, true)
                .ok_or_else(|| "cannot create a job object".to_string())?;
            Ok(Self {
                pid: std::sync::atomic::AtomicU32::new(0),
                job: Some(job),
            })
        }
        #[cfg(not(windows))]
        Ok(Self {
            pid: std::sync::atomic::AtomicU32::new(0),
        })
    }

    pub fn configure(&self, command: &mut std::process::Command) {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            let mut flags = win::CREATE_NO_WINDOW | win::CREATE_NEW_PROCESS_GROUP;
            if !win::has_console() {
                flags |= win::CREATE_NO_WINDOW;
            }
            command.creation_flags(flags);
        }
        #[cfg(not(any(unix, windows)))]
        let _ = command;
    }

    pub fn spawned(&self, pid: u32) {
        self.pid.store(pid, std::sync::atomic::Ordering::SeqCst);
        #[cfg(windows)]
        if let Some(job) = &self.job
            && let Err(_error) = job.assign(pid)
        {
            win::terminate(pid);
        }
        #[cfg(not(windows))]
        let _ = pid;
    }

    pub fn kill(&self) {
        #[cfg(unix)]
        let pid = self.pid.load(std::sync::atomic::Ordering::SeqCst);
        #[cfg(unix)]
        {
            // Safe: the child leads the group this runner created. Never group 0.
            if pid != 0 {
                unsafe {
                    libc::killpg(pid as libc::pid_t, libc::SIGKILL);
                }
            }
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate_job();
        }
        #[cfg(not(any(unix, windows)))]
        let _ = pid;
    }
}

impl ProcessGroup {
    pub fn new() -> Self {
        Self {
            state: Arc::new(GroupState {
                killed: AtomicBool::new(false),
                detached: AtomicBool::new(false),
                pid: AtomicU32::new(0),
                #[cfg(windows)]
                job: Mutex::new(None),
            }),
        }
    }

    /// On Windows, bind the group to the named job the session created for
    /// this call. Unix needs nothing: the shim's own group is the tree.
    #[cfg(windows)]
    pub fn with_job(self, job: win::Job) -> Self {
        if let Ok(mut slot) = self.state.job.lock() {
            *slot = Some(job);
        }
        self
    }

    /// Apply the platform's grouping flags to a command before spawning.
    pub fn configure(&self, command: &mut Command) {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.as_std_mut().process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            let mut flags = win::CREATE_SUSPENDED | win::CREATE_NEW_PROCESS_GROUP;
            // A session without a console (a tool shell, a service) must not leave the call's shell to allocate
            // one: msys `bash.exe` then creates a *visible* console window — one per call — and can stall for
            if !win::has_console() {
                flags |= win::CREATE_NO_WINDOW;
            }
            command.as_std_mut().creation_flags(flags);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = command;
        }
    }

    /// Bind the group to the freshly spawned child. On Windows this is where
    /// the suspended child enters the Job Object and is allowed to run.
    pub fn spawned(&self, pid: u32) -> Result<(), String> {
        self.state.pid.store(pid, Ordering::SeqCst);
        #[cfg(windows)]
        {
            let assigned = match self.state.job.lock() {
                Ok(job) => match job.as_ref() {
                    Some(job) => job.assign(pid),
                    None => Err("the call has no job object".to_string()),
                },
                Err(_) => Err("the call's job object is poisoned".to_string()),
            };
            if let Err(error) = assigned {
                // A call outside its Job Object cannot be stopped, so it is
                // taken down here rather than left running unreachable. The child is still suspended.
                win::terminate(pid);
                self.kill();
                return Err(format!(
                    "cannot put the child process {pid} in its job object: {error}"
                ));
            }
            // A child that never resumes would hang the call forever.
            if let Err(error) = win::resume(pid) {
                self.kill();
                return Err(format!("cannot resume the child process {pid}: {error}"));
            }
        }
        #[cfg(not(any(unix, windows)))]
        let _ = pid;
        // A verb may have killed this call while the process was being created:
        // the kill could not reach a process that did not exist yet.
        if self.is_killed() {
            self.terminate();
        }
        Ok(())
    }

    pub fn kill(&self) -> bool {
        if self.state.detached.load(Ordering::SeqCst) {
            return false;
        }
        if self.state.killed.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.terminate();
        true
    }

    /// The session stops owning the tree: a promoted job keeps running after the session exits, so neither
    /// `Drop` nor a late `kill` may take it down.
    pub fn detach(&self) {
        self.state.detached.store(true, Ordering::SeqCst);
        #[cfg(windows)]
        if let Ok(mut job) = self.state.job.lock() {
            if let Some(job) = job.as_ref() {
                job.set_kill_on_close(false);
            }
            // Dropping the handle without terminating is the handover.
            let _ = job.take();
        }
    }

    /// Signal the tree now, whatever `killed` says.
    fn terminate(&self) {
        let pid = self.state.pid.load(Ordering::SeqCst);
        if pid == 0 {
            return;
        }
        #[cfg(unix)]
        {
            // Safe: `killpg` on a group this session created.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        {
            if let Ok(job) = self.state.job.lock()
                && let Some(job) = job.as_ref()
            {
                job.terminate_job();
                return;
            }
            // Without a job handle the shim itself is the only thing left to
            // signal; its own Job Object is closed with it.
            win::terminate(pid);
        }
    }

    pub fn is_killed(&self) -> bool {
        self.state.killed.load(Ordering::SeqCst)
    }

    pub fn is_detached(&self) -> bool {
        self.state.detached.load(Ordering::SeqCst)
    }
}

impl Default for ProcessGroup {
    fn default() -> Self {
        Self::new()
    }
}

/// Every process group the session started.
#[derive(Debug, Clone, Default)]
pub struct SessionGroups(Arc<GroupsInner>);

#[derive(Debug, Default)]
struct GroupsInner(Mutex<Vec<ProcessGroup>>);

impl Drop for GroupsInner {
    fn drop(&mut self) {
        let groups = self
            .0
            .lock()
            .map(|groups| groups.clone())
            .unwrap_or_default();
        for group in groups {
            group.kill();
        }
    }
}

impl SessionGroups {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn keep(&self, group: ProcessGroup) {
        if let Ok(mut groups) = self.0.0.lock() {
            groups.push(group);
        }
    }
}

/// The calls running right now, keyed by output directory, so a verb can kill exactly one of them (or
/// all of them) and the batch can tell a killed call (`stopped`) from one that never started
#[derive(Debug, Clone, Default)]
pub struct Running(Arc<Mutex<HashMap<String, ProcessGroup>>>);

impl Running {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, key: &str, group: ProcessGroup) {
        if let Ok(mut running) = self.0.lock() {
            running.insert(key.to_string(), group);
        }
    }

    pub fn remove(&self, key: &str) {
        if let Ok(mut running) = self.0.lock() {
            running.remove(key);
        }
    }

    pub fn keys(&self) -> Vec<String> {
        self.0
            .lock()
            .map(|running| running.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn kill(&self, key: &str) -> bool {
        let group = self
            .0
            .lock()
            .ok()
            .and_then(|running| running.get(key).cloned());
        match group {
            Some(group) => group.kill(),
            None => false,
        }
    }

    /// Kill every running call; returns how many were still running.
    pub fn kill_all(&self) -> usize {
        let groups: Vec<ProcessGroup> = self
            .0
            .lock()
            .map(|running| running.values().cloned().collect())
            .unwrap_or_default();
        let mut killed = 0;
        for group in groups {
            if group.kill() {
                killed += 1;
            }
        }
        killed
    }
}

/// When the process `pid` started, as the OS records it.
pub fn start_time(pid: u32) -> Option<std::time::SystemTime> {
    #[cfg(windows)]
    {
        win::start_time(pid)
    }
    #[cfg(target_os = "linux")]
    {
        linux_start_time(pid)
    }
    #[cfg(target_os = "macos")]
    {
        macos_start_time(pid)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// macOS: `ps -o lstart=` (local time), converted to epoch seconds by
/// `date -j -f`. `ps`'s seconds are whole, which is all the identity check
/// needs: a recycled pid gets a different value unless it is reused in the
/// same second.
#[cfg(target_os = "macos")]
fn macos_start_time(pid: u32) -> Option<std::time::SystemTime> {
    let output = crate::sys::runner::std_command_name("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return None;
    }
    let converted = crate::sys::runner::std_command_name("date")
        .args(["-j", "-f", "%a %b %e %T %Y", &text, "+%s"])
        .output()
        .ok()?;
    if !converted.status.success() {
        return None;
    }
    let seconds: u64 = String::from_utf8_lossy(&converted.stdout)
        .trim()
        .parse()
        .ok()?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
}

/// Linux: `/proc/<pid>/stat` field 22 is the start time in clock ticks since
/// boot; `/proc/stat`'s `btime` is when the boot was.
#[cfg(target_os = "linux")]
fn linux_start_time(pid: u32) -> Option<std::time::SystemTime> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is in parentheses and may itself contain `)` and
    // spaces, so the fields are counted from the last `)`.
    let after = stat.rsplit_once(')')?.1;
    // `after` starts with field 3 (`state`); `starttime` is field 22.
    let ticks: u64 = after.split_whitespace().nth(19)?.parse().ok()?;
    let boot = std::fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("btime ")?.trim().parse::<u64>().ok())?;
    // `sysconf(_SC_CLK_TCK)` is 100 on every Linux kelir runs on, but asking
    // costs nothing and a wrong constant would make every identity check fail.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let hz = if hz > 0 { hz as u64 } else { 100 };
    let offset = boot + ticks / hz;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(offset))
}

/// How many processes are in process group `pgid`, **not counting the caller**.
#[cfg(target_os = "linux")]
pub fn group_others(pgid: u32) -> Option<usize> {
    if pgid == 0 {
        return None;
    }
    let me = std::process::id();
    let mut others = 0;
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(after) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
            continue;
        };
        // Field 8 after the name is `pgrp`; `after` starts at field 3.
        if after
            .split_whitespace()
            .nth(2)
            .and_then(|field| field.parse::<u32>().ok())
            == Some(pgid)
        {
            others += 1;
        }
    }
    Some(others)
}

#[cfg(not(target_os = "linux"))]
pub fn group_others(_pgid: u32) -> Option<usize> {
    None
}

/// The pids still in process group `pgid` on Linux. `None` when the platform cannot say.
#[cfg(target_os = "linux")]
pub fn group_members(pgid: u32) -> Option<Vec<u32>> {
    if pgid == 0 {
        return None;
    }
    let mut members = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(after) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
            continue;
        };
        // `after` starts at field 3 (`state`): field 5 is `pgrp`.
        if after
            .split_whitespace()
            .nth(2)
            .and_then(|field| field.parse::<u32>().ok())
            == Some(pgid)
        {
            members.push(pid);
        }
    }
    Some(members)
}

#[cfg(target_os = "macos")]
pub fn group_members(pgid: u32) -> Option<Vec<u32>> {
    if pgid == 0 {
        return None;
    }
    let output = crate::sys::runner::ps_command()
        .args(["-ax", "-o", "pid=,pgid="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut members = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(group)) = (
            fields.next().and_then(|p| p.parse::<u32>().ok()),
            fields.next().and_then(|p| p.parse::<u32>().ok()),
        ) else {
            continue;
        };
        if group == pgid {
            members.push(pid);
        }
    }
    Some(members)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn group_members(_pgid: u32) -> Option<Vec<u32>> {
    None
}

/// Does the OS still report a process with this pid and recorded start time?
/// A recycled pid never inherits its predecessor's start time. When the
/// platform cannot tell, the caller must not claim identity it cannot prove.
pub fn is_same_process(pid: u32, started_ms: Option<u64>) -> bool {
    if pid == 0 {
        return false;
    }
    match started_ms {
        Some(was) => match crate::features::children::start_ms(pid) {
            Some(now) => was.abs_diff(now) <= 2_000,
            None => false,
        },
        // On macOS start time is unavailable: identity cannot be proved.
        None => false,
    }
}

/// Signal one process by pid, never by process group. `0` is never ours to
/// signal: on Unix it would mean the caller's whole group.
#[cfg(unix)]
pub fn signal(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: a positive pid, identity checked by the caller. Never group 0.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) == 0 }
}

/// Signal one process by pid (Windows: TerminateProcess). `0` is never ours.
#[cfg(windows)]
pub fn signal(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    win::terminate(pid);
    true
}

/// Wait until the root of a killed tree is gone, or the grace period ends.
/// Zombies count as gone: they hold no resources and cannot signal anything.
pub fn wait_gone(pid: u32, started_ms: Option<u64>, grace: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + grace;
    loop {
        #[cfg(target_os = "linux")]
        let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(')')
                    .and_then(|(_, rest)| rest.trim_start().chars().next())
            })
            == Some('Z');
        #[cfg(not(target_os = "linux"))]
        let zombie = false;
        if zombie || !crate::features::children::process_alive(pid, started_ms) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// Kill a registered tree by pid and start time, without assuming the root
/// leads a process group: a session spawned from a terminal inherits its
/// parent's group. On Unix the tree is found by parentage and every victim's
/// start time is checked against the snapshot before it is signalled; on
/// Windows the OS's own tree walk (or the call's Job Object) is used.
/// Returns whether the root was actually gone after the grace period.
pub fn kill_tree(pid: u32, started_ms: Option<u64>) -> bool {
    if !crate::features::children::process_alive(pid, started_ms) {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        // Never signal a pid whose identity the OS cannot confirm, even if
        // the caller's recorded start time matched a moment ago.
        if !is_same_process(pid, started_ms) {
            return false;
        }
        let victims = tree_descendants(pid);
        for (victim, start) in victims.iter().rev() {
            // A pid recycled between the walk and the signal is never ours:
            // re-read its own start time from /proc before signalling it.
            if !is_same_process(*victim, *start) {
                continue;
            }
            signal(*victim);
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // macOS has no /proc tree to walk and no reliable start time; a
        // registered child leads its own group (ProcessGroup::configure),
        // and only that verified leader may be group-signalled here.
        if group_leader(pid) {
            // SAFETY: getpgid proved the pid leads the group we created.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        } else if !signal(pid) {
            return false;
        }
    }
    #[cfg(windows)]
    {
        // The Windows path is unchanged: terminate_tree walks pid + creation
        // time, and the named Job Object holds each call's whole tree.
        terminate_tree(pid);
    }
    wait_gone(pid, started_ms, std::time::Duration::from_secs(3))
}

/// Is `pid` the leader of its own process group? Only a group we created may
/// ever be group-signalled; a root session inherits its parent's group.
#[cfg(unix)]
pub fn group_leader(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: querying a pid never changes it.
    unsafe { libc::getpgid(pid as libc::pid_t) == pid as libc::pid_t }
}

/// Linux: the live descendants of `pid` (each with its own start time,
/// converted to the same millisecond clock the owner file uses), `pid` first
/// and deepest last, by parent links. No group id is involved, so a group the
/// caller does not own is never touched.
#[cfg(target_os = "linux")]
fn tree_descendants(pid: u32) -> Vec<(u32, Option<u64>)> {
    let mut children: std::collections::HashMap<u32, Vec<(u32, Option<u64>)>> =
        std::collections::HashMap::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(child) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(after) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
            continue;
        };
        // `after` starts at field 3 (state); field 4 is the parent pid.
        let parent = after
            .split_whitespace()
            .nth(1)
            .and_then(|field| field.parse::<u32>().ok());
        if let Some(parent) = parent {
            children
                .entry(parent)
                .or_default()
                .push((child, crate::features::children::start_ms(child)));
        }
    }
    let me = std::process::id();
    let mut order = vec![(pid, crate::features::children::start_ms(pid))];
    let mut seen = std::collections::HashSet::from([pid]);
    let mut at = 0;
    while at < order.len() {
        let parent = order[at].0;
        at += 1;
        let Some(kids) = children.get(&parent) else {
            continue;
        };
        for (kid, start) in kids {
            if *kid == me || !seen.insert(*kid) {
                continue;
            }
            order.push((*kid, *start));
        }
    }
    order
}

/// Kill `pid` and every process the OS still reports as its descendant, deepest first.
/// Returns how many were signalled.
#[cfg(windows)]
pub fn terminate_tree(pid: u32) -> usize {
    if pid == 0 {
        return 0;
    }
    let me = std::process::id();
    let mut created: HashMap<u32, Option<std::time::SystemTime>> = HashMap::new();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for info in win::snapshot() {
        created.insert(info.pid, info.created);
        children.entry(info.ppid).or_default().push(info.pid);
    }
    let mut order = vec![pid];
    let mut seen: HashSet<u32> = HashSet::from([pid]);
    let mut at = 0;
    while at < order.len() {
        let parent = order[at];
        at += 1;
        let Some(kids) = children.get(&parent) else {
            continue;
        };
        for kid in kids {
            if *kid == me || !seen.insert(*kid) {
                continue;
            }
            if let (Some(Some(older)), Some(Some(younger))) =
                (created.get(&parent), created.get(kid))
                && younger < older
            {
                // Not this tree's: a child cannot predate its parent.
                continue;
            }
            order.push(*kid);
        }
    }
    let mut killed = 0;
    for victim in order.iter().rev() {
        win::terminate(*victim);
        killed += 1;
    }
    killed
}

#[cfg(not(windows))]
pub fn terminate_tree(_pid: u32) -> usize {
    0
}
