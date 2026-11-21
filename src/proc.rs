//! Tool-child process grouping, so a stop can kill a whole tree.
//! Every tool child is `kelir bg-shim`, which owns the command it runs: the shim puts the command
//! in its own process group on Unix and in a Job Object on Windows, so the shim can take the tree
//! down by itself, and its owner can signal intent by killing the shim's group / job.

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
        // macOS has no /proc.
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
        // `ps lstart` is local time.
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
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
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
    let output = std::process::Command::new("ps")
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

    /// Close this process to the rest of its user: no `PROCESS_VM_READ`, `VM_WRITE`, `CREATE_THREAD`,
    /// `DUP_HANDLE` or `QUERY_INFORMATION`, and no `WRITE_DAC` to take them back.
    pub fn deny_same_user_memory_access() -> Result<(), String> {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, SetKernelObjectSecurity};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let sddl: Vec<u16> = "D:P(A;;GA;;;SY)(A;;0x101501;;;OW)"
            .encode_utf16()
            .chain(std::iter::once(0))
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
    /// A console of its own, with no window: a console-subsystem child gets a console it can use,
    /// without the overhead (and the visible window) of a detached process allocating one per job.
    pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    pub const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    pub fn has_console() -> bool {
        use windows_sys::Win32::System::Console::GetConsoleOutputCP;
        unsafe { GetConsoleOutputCP() != 0 }
    }

    /// Assign, query, terminate and change the job: everything the shim and the
    /// canceller need by name (`JOB_OBJECT_ALL_ACCESS`).
    const JOB_ACCESS: u32 = 0x001F_001F;

    #[derive(Debug)]
    pub struct Job(HANDLE);

    // The handle is just a kernel object reference; it may cross threads.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        /// Create (or open) the named job for one call.
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

        /// Turn kill-on-close on or off. On is the default for a call: closing the last handle
        /// terminates the job's processes.
        pub fn set_kill_on_close(&self, on: bool) -> bool {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            // Nothing else is set on purpose: no breakaway (a descendant that could leave the job could
            // not be cancelled —), no memory or process limits.
            info.BasicLimitInformation.LimitFlags = if on {
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            } else {
                0
            };
            unsafe {
                SetInformationJobObject(
                    self.0,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&info).cast::<c_void>(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) != 0
            }
        }

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

        /// How many processes the job holds right now (`None` when the query fails).
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
                    std::ptr::from_mut(&mut info).cast::<c_void>(),
                    std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                )
            };
            (queried != 0).then_some(info.ActiveProcesses)
        }

        /// Hand a copy of this job handle to another process — the job's own command — so the job
        /// object, and with it the **name** that `cancel` opens, outlives the shim that made it.
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

    /// When the process was created: the kernel's own answer, which is the same clock a status
    /// file's `started` comes from. `None` when the process is gone or not ours to look at.
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

    /// Watch another process without polling: the handle is signalled when it exits.
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
