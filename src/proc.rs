//! Tool-child process grouping, so a stop can kill a whole tree.
//!
//! Every tool child is `kelir bg-shim`, which owns the command it runs (M4):
//! the shim puts the command in its own process group on Unix and in a Job
//! Object on Windows, so the shim can take the tree down by itself, and its
//! owner can signal intent by killing the shim's group / job.
//!
//! *Unix:* the shim leads its own process group (`process_group(0)`), which
//! `killpg` signals; the command and everything it spawns inherit that group.
//!
//! *Windows:* the session creates a **named** Job Object per call, assigns the
//! suspended shim to it and only then resumes it, so the shim cannot run user
//! code before it is inside the job (M0 review N2). The shim opens the same job
//! by name, assigns the command to it and keeps its own handle: a promoted job
//! therefore survives the session closing its handle, and `kelir bg cancel` can
//! open the job by name from another process. The job is **not** breakaway-OK —
//! an msys `spawn` asks for `CREATE_BREAKAWAY_FROM_JOB` whenever the job allows
//! it, which is exactly how exec'd grandchildren (`sleep.exe`, a nested
//! `bash.exe`) used to escape a cancel (M4 review F1). It **is**
//! `KILL_ON_JOB_CLOSE` until promotion, so a session that is killed outright
//! still takes a leftover `cmd &` child with it (M4 review F6); promotion
//! clears the flag before the session closes its handle. A shim also watches
//! its owner, which is the same guarantee on both platforms.
//!
//! A [`ProcessGroup`] is a cheap, cloneable handle: the call holds one to kill
//! its own tree, and the session keeps another so a leftover background child
//! survives the call and is cleaned up when the session ends.

use std::collections::HashMap;
#[cfg(windows)]
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use tokio::process::Command;

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
            // A session without a console (a tool shell, a service) must not
            // leave the call's shell to allocate one: msys `bash.exe` then
            // creates a *visible* console window — one per call — and can stall
            // for seconds at start-up (M4 review round 2, G3). A session that
            // has a console passes it on, which costs nothing and shows no
            // window.
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
                // taken down here rather than left running unreachable
                // (M1 review N7). The child is still suspended.
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

    /// Kill the whole tree. Idempotent; returns true the first time. A
    /// detached (promoted) group is left alone.
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

    /// The session stops owning the tree: a promoted job keeps running after
    /// the session exits, so neither `Drop` nor a late `kill` may take it down.
    /// On Windows the job stops being kill-on-close (the session's handle is
    /// about to close, and the tree must outlive it) and the session's handle
    /// is closed here; the shim holds its own, so the job survives.
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

/// Every process group the session started. Holding them keeps a leftover
/// background child alive after its call returns; dropping the last handle
/// (the session exiting) kills what is still in them, as the spec requires.
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

/// The calls running right now, keyed by output directory, so a verb can kill
/// exactly one of them (or all of them) and the batch can tell a killed call
/// (`stopped`) from one that never started (`skipped`).
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

    /// The keys of the calls that are running this instant.
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
///
/// A status file outlives its shim (`taskkill /F`, an OOM kill, a reboot) and
/// the pid can belong to something else by the time a reader asks, so a reader
/// that is about to signal a pid compares this with the status's `started`
/// first (M4 review F5). `None` when the process is gone, or when the platform
/// cannot say (there is no `/proc` on macOS), in which case the caller falls
/// back to plain liveness.
pub fn start_time(pid: u32) -> Option<std::time::SystemTime> {
    #[cfg(windows)]
    {
        win::start_time(pid)
    }
    #[cfg(target_os = "linux")]
    {
        linux_start_time(pid)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = pid;
        None
    }
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
/// `None` when the platform cannot say, so a caller never mistakes "I cannot
/// tell" for "the group is empty".
///
/// This is what lets a finished call's shim stay behind as its tree's owner: it
/// exits as soon as nothing is left, and while something is left a `SIGKILL` of
/// the session still takes the tree down (M4 review F6).
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

/// The pids still in process group `pgid` on Linux. `None` when the platform
/// cannot say.
///
/// `cancel` uses it to reach a *lost* job's tree: the group is led by the dead
/// shim's pid, and a pid number cannot be reused while a group with that number
/// still has a member, so a non-empty group is that job's tree and nothing
/// else's (M4 review round 2, G2).
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

#[cfg(not(target_os = "linux"))]
pub fn group_members(_pgid: u32) -> Option<Vec<u32>> {
    None
}

/// Kill `pid` and every process the OS still reports as its descendant,
/// deepest first. Returns how many were signalled.
///
/// This is how `cancel` reaches a *lost* job's tree on Windows (M4 review round
/// 2, G2): the job object keeps no name once its last handle is gone, but the
/// processes are still there and their parent links say where. The caller must
/// have proved that `pid` is the command's own process first (its recorded
/// start time); a child is only walked into when it is not older than its
/// parent, so a recycled pid whose new holder has children of its own is left
/// alone.
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

/// Windows: the shim's Job Object, and the two raw calls the session needs to
/// spawn a suspended child into it.
#[cfg(windows)]
pub mod win {
    use std::ffi::c_void;
    use std::io;

    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, OpenJobObjectW,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenThread, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, ResumeThread, THREAD_SUSPEND_RESUME,
        TerminateProcess, WaitForSingleObject,
    };

    /// Close this process to the rest of its user: no `PROCESS_VM_READ`,
    /// `VM_WRITE`, `CREATE_THREAD`, `DUP_HANDLE` or `QUERY_INFORMATION`, and no
    /// `WRITE_DAC` to take them back. The DACL names `SYSTEM` (everything) and
    /// *Owner Rights* with only terminate / set-quota / limited-query /
    /// synchronize, which is what `cancel` and the liveness checks use; an
    /// explicit Owner Rights entry is what removes the owner's implicit
    /// `READ_CONTROL | WRITE_DAC`.
    ///
    /// This closes the plain `ReadProcessMemory` read of a shim's signing key by
    /// a sibling tool child. It does not close an elevated administrator that
    /// enables `SeDebugPrivilege`, a kernel debugger, or the user's own
    /// ability to log in as `SYSTEM`: those are outside the boundary (SPEC,
    /// STEER-7).
    pub fn deny_same_user_memory_access() -> Result<(), String> {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, SetKernelObjectSecurity};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let sddl: Vec<u16> = "D:P(A;;GA;;;SY)(A;;0x101101;;;OW) "
            .encode_utf16()
            .collect();
        let mut descriptor: *mut c_void = std::ptr::null_mut();
        // SAFETY: NUL-terminated string, out pointer ours; freed below.
        let built = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if built == 0 {
            return Err(format!(
                "cannot build the process DACL: {}",
                io::Error::last_os_error()
            ));
        }
        // SAFETY: the pseudo handle for this process always has WRITE_DAC; the
        // descriptor is the self-relative one built above.
        let set = unsafe {
            SetKernelObjectSecurity(GetCurrentProcess(), DACL_SECURITY_INFORMATION, descriptor)
        };
        let error = io::Error::last_os_error();
        // SAFETY: allocated by the conversion above.
        unsafe {
            LocalFree(descriptor);
        }
        if set == 0 {
            return Err(format!("cannot restrict this process: {error}"));
        }
        Ok(())
    }

    pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    pub const CREATE_SUSPENDED: u32 = 0x0000_0004;
    pub const DETACHED_PROCESS: u32 = 0x0000_0008;
    /// A console of its own, with no window: a console-subsystem child gets a
    /// console it can use, without the cost (and the visible window) of a
    /// detached process allocating one per job (M4 review round 2, G3).
    pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    pub const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    /// Does this process have a console at all? A hidden console answers yes
    /// (`GetConsoleOutputCP` works); a process without one gets 0, however it
    /// was started (`DETACHED_PROCESS`, a service, a tool shell). This is what
    /// tells a call's shim whether its shell would have to allocate a console
    /// — and a console-subsystem child of a console-less parent allocates a
    /// **visible** one (M4 review round 2, G3).
    pub fn has_console() -> bool {
        use windows_sys::Win32::System::Console::GetConsoleOutputCP;
        unsafe { GetConsoleOutputCP() != 0 }
    }

    /// Assign, query, terminate and change the job: everything the shim and the
    /// canceller need by name (`JOB_OBJECT_ALL_ACCESS`).
    const JOB_ACCESS: u32 = 0x001F_001F;

    /// A Job Object handle. The session holds one per call; the call's shim
    /// opens its own by name, which is what lets a promoted job outlive the
    /// session's handle.
    #[derive(Debug)]
    pub struct Job(HANDLE);

    // The handle is just a kernel object reference; it may cross threads.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        /// Create (or open) the named job for one call.
        ///
        /// `kill_on_close` is on for a call's job (a session that is killed
        /// outright still has its job handles closed by the kernel, which takes
        /// a leftover `cmd &` child with it) and off for a job that has to
        /// outlive anything (`kelir bg run`, a promoted call).
        pub fn create_named(name: &str, kill_on_close: bool) -> Option<Job> {
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let job = unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), wide.as_ptr());
                if handle.is_null() {
                    return None;
                }
                Job(handle)
            };
            if !job.set_kill_on_close(kill_on_close) {
                return None;
            }
            Some(job)
        }

        /// Turn kill-on-close on or off. On is the default for a call: closing
        /// the last handle terminates the job's processes. Off is what a
        /// promotion does, because the tree is handed to the shim from then on.
        pub fn set_kill_on_close(&self, on: bool) -> bool {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            // Nothing else is set on purpose: no breakaway (a descendant that
            // could leave the job could not be cancelled — M4 review F1), no
            // memory or process limits.
            info.BasicLimitInformation.LimitFlags = if on {
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            } else {
                0
            };
            unsafe {
                SetInformationJobObject(
                    self.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) != 0
            }
        }

        /// Open a job another process created, by name.
        pub fn open(name: &str) -> Option<Job> {
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                let handle = OpenJobObjectW(JOB_ACCESS, 0, wide.as_ptr());
                if handle.is_null() {
                    return None;
                }
                Some(Job(handle))
            }
        }

        pub fn assign(&self, pid: u32) -> Result<(), String> {
            unsafe {
                let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                if process.is_null() {
                    return Err(format!(
                        "cannot open the process: {}",
                        io::Error::last_os_error()
                    ));
                }
                let assigned = AssignProcessToJobObject(self.0, process);
                CloseHandle(process);
                if assigned == 0 {
                    return Err(format!(
                        "cannot assign it to the job: {}",
                        io::Error::last_os_error()
                    ));
                }
            }
            Ok(())
        }

        pub fn terminate_job(&self) {
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }

        /// How many processes the job holds right now (`None` when the query
        /// fails). A test harness asserts this is 0 after it terminates the
        /// job, so a leaked tree fails the test instead of outliving it.
        pub fn active_processes(&self) -> Option<u32> {
            use windows_sys::Win32::System::JobObjects::{
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
                QueryInformationJobObject,
            };

            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            let queried = unsafe {
                QueryInformationJobObject(
                    self.0,
                    JobObjectBasicAccountingInformation,
                    &mut info as *mut _ as *mut c_void,
                    std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                )
            };
            (queried != 0).then_some(info.ActiveProcesses)
        }

        /// Hand a copy of this job handle to another process — the job's own
        /// command — so the job object, and with it the **name** that `cancel`
        /// opens, outlives the shim that made it.
        ///
        /// A job's name is removed when its last handle closes, and a killed
        /// shim closes the last one; the job body lives on, unreachable. The
        /// command is in the job and outlives the shim in the ordinary case
        /// (`bash -c 'sleep … & wait'`), so a handle of its own keeps the name
        /// (M4 review round 2, G2). Only a job that must survive its shim gets
        /// one: a call's job before promotion must stay kill-on-close.
        pub fn lend_to(&self, pid: u32) -> Result<(), String> {
            use windows_sys::Win32::Foundation::{DuplicateHandle, HANDLE};
            use windows_sys::Win32::System::Threading::{GetCurrentProcess, PROCESS_DUP_HANDLE};

            unsafe {
                let target = OpenProcess(PROCESS_DUP_HANDLE, 0, pid);
                if target.is_null() {
                    return Err(format!(
                        "cannot open the command's process: {}",
                        io::Error::last_os_error()
                    ));
                }
                let mut borrowed: HANDLE = std::ptr::null_mut();
                let ok = DuplicateHandle(
                    GetCurrentProcess(),
                    self.0,
                    target,
                    &mut borrowed,
                    0,
                    0,
                    windows_sys::Win32::Foundation::DUPLICATE_SAME_ACCESS,
                );
                CloseHandle(target);
                if ok == 0 {
                    return Err(format!(
                        "cannot lend the job handle: {}",
                        io::Error::last_os_error()
                    ));
                }
            }
            Ok(())
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// Kill one process by id.
    pub fn terminate(pid: u32) {
        unsafe {
            let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !process.is_null() {
                TerminateProcess(process, 1);
                CloseHandle(process);
            }
        }
    }

    /// One process as the OS reports it: enough to walk a tree by parentage.
    #[derive(Debug, Clone, Copy)]
    pub struct ProcInfo {
        pub pid: u32,
        pub ppid: u32,
        pub created: Option<std::time::SystemTime>,
    }

    /// Every process on the machine: pid, parent pid and creation time.
    ///
    /// `cancel` uses it to reach a *lost* job's tree (M4 review round 2, G2):
    /// the job object's name dies with its last handle, but the tree is still
    /// there, and the parent links say which processes are its.
    pub fn snapshot() -> Vec<ProcInfo> {
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            PROCESSENTRY32, Process32First, Process32Next, TH32CS_SNAPPROCESS,
        };

        let mut out = Vec::new();
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return out;
            }
            let mut entry: PROCESSENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;
            let mut more = Process32First(snapshot, &mut entry) != 0;
            while more {
                let pid = entry.th32ProcessID;
                if pid != 0 {
                    out.push(ProcInfo {
                        pid,
                        ppid: entry.th32ParentProcessID,
                        created: start_time(pid),
                    });
                }
                more = Process32Next(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
        }
        out
    }

    /// When the process was created: the kernel's own answer, which is the same
    /// clock a status file's `started` comes from. `None` when the process is
    /// gone or not ours to look at.
    pub fn start_time(pid: u32) -> Option<std::time::SystemTime> {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::GetProcessTimes;

        // 1601-01-01, the FILETIME epoch, in seconds since the Unix epoch.
        const FILETIME_UNIX_EPOCH: u64 = 11_644_473_600;
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return None;
            }
            let (mut creation, mut exit, mut kernel, mut user): (
                FILETIME,
                FILETIME,
                FILETIME,
                FILETIME,
            ) = std::mem::zeroed();
            let ok = GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user);
            CloseHandle(process);
            if ok == 0 {
                return None;
            }
            let ticks = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
            let seconds = ticks / 10_000_000;
            if seconds < FILETIME_UNIX_EPOCH {
                return None;
            }
            Some(
                std::time::UNIX_EPOCH
                    + std::time::Duration::from_secs(seconds - FILETIME_UNIX_EPOCH)
                    + std::time::Duration::from_nanos(ticks % 10_000_000 * 100),
            )
        }
    }

    /// Watch another process without polling: the handle is signalled when it
    /// exits. Used by the shim to tell a session that died (a `kill -9`, a
    /// closed terminal) from one that promoted it and walked away.
    #[derive(Debug)]
    pub struct OwnerWatch(HANDLE);

    unsafe impl Send for OwnerWatch {}
    unsafe impl Sync for OwnerWatch {}

    impl OwnerWatch {
        pub fn open(pid: u32) -> Option<Self> {
            unsafe {
                let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
                if process.is_null() {
                    return None;
                }
                Some(OwnerWatch(process))
            }
        }

        pub fn gone(&self) -> bool {
            unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
        }
    }

    impl Drop for OwnerWatch {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// Resume the main thread of a child created with `CREATE_SUSPENDED`.
    ///
    /// `std::process::Child` does not expose the thread handle, so the thread
    /// is found by walking a snapshot of the system's threads; a suspended
    /// process has exactly one, and it cannot have started anything else.
    pub fn resume(pid: u32) -> io::Result<()> {
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut resumed = 0;
            let mut more = Thread32First(snapshot, &mut entry) != 0;
            while more {
                if entry.th32OwnerProcessID == pid {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if !thread.is_null() {
                        ResumeThread(thread);
                        CloseHandle(thread);
                        resumed += 1;
                    }
                }
                more = Thread32Next(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
            if resumed == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no thread of the child to resume",
                ));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn killing_a_group_twice_reports_once() {
        let group = ProcessGroup::new();
        assert!(!group.is_killed());
        assert!(group.kill());
        assert!(group.is_killed());
        assert!(!group.kill());
    }

    #[test]
    fn a_detached_group_is_never_killed() {
        let group = ProcessGroup::new();
        group.detach();
        assert!(group.is_detached());
        assert!(!group.kill(), "a promoted job is not the session's to kill");
        assert!(!group.is_killed());
    }

    #[test]
    fn the_running_registry_tracks_and_kills_keys() {
        let running = Running::new();
        assert!(running.keys().is_empty());
        running.insert("7-1-1", ProcessGroup::new());
        assert_eq!(running.keys(), vec!["7-1-1".to_string()]);
        assert!(running.kill("7-1-1"));
        assert!(!running.kill("nope"));
        assert_eq!(running.kill_all(), 0, "the group was already killed");
        running.remove("7-1-1");
        assert!(running.keys().is_empty());
    }

    #[test]
    fn kept_groups_are_killed_when_the_session_handle_goes_away() {
        let group = ProcessGroup::new();
        let observed = group.clone();
        {
            let groups = SessionGroups::new();
            groups.keep(group);
            assert!(!observed.is_killed());
        }
        assert!(observed.is_killed());
    }

    #[test]
    fn a_kept_group_that_was_promoted_survives_the_session() {
        let group = ProcessGroup::new();
        let observed = group.clone();
        {
            let groups = SessionGroups::new();
            groups.keep(group);
            // Promotion detaches it: the session no longer owns the job.
            observed.detach();
        }
        assert!(
            !observed.is_killed(),
            "a promoted job died with the session"
        );
    }
}
