//! Tool-child process grouping, so a stop can kill a whole tree.
//!
//! Unix: the child leads its own process group (`process_group(0)`), which
//! `killpg` signals. Windows: the child is created **suspended**, assigned to a
//! Job Object with kill-on-close, and only then resumed, so it cannot run user
//! code before it is inside the job (M0 review N2).
//!
//! A [`ProcessGroup`] is a cheap, cloneable handle: the call holds one to kill
//! its own tree, and the session keeps another so a leftover background child
//! survives the call and is cleaned up when the session ends.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct ProcessGroup {
    state: Arc<GroupState>,
}

#[derive(Debug)]
struct GroupState {
    killed: AtomicBool,
    #[cfg(unix)]
    pgid: std::sync::atomic::AtomicU32,
    #[cfg(windows)]
    job: Option<win::Job>,
    #[cfg(not(any(unix, windows)))]
    _none: (),
}

impl ProcessGroup {
    pub fn new() -> Self {
        Self {
            state: Arc::new(GroupState {
                killed: AtomicBool::new(false),
                #[cfg(unix)]
                pgid: std::sync::atomic::AtomicU32::new(0),
                #[cfg(windows)]
                job: win::Job::create(),
                #[cfg(not(any(unix, windows)))]
                _none: (),
            }),
        }
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
            command
                .as_std_mut()
                .creation_flags(win::CREATE_SUSPENDED | win::CREATE_NEW_PROCESS_GROUP);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = command;
        }
    }

    /// Bind the group to the freshly spawned child. On Windows this is where
    /// the suspended child enters the Job Object and is allowed to run.
    pub fn spawned(&self, pid: u32) -> Result<(), String> {
        #[cfg(unix)]
        self.state.pgid.store(pid, Ordering::SeqCst);
        #[cfg(windows)]
        {
            let Some(job) = &self.state.job else {
                // Without a Job Object the call could not be stopped and its
                // tree could outlive the session: refuse it instead of running
                // something uncontrollable (M1 review N7).
                win::terminate(pid);
                return Err(format!(
                    "cannot create a job object for the child process {pid}"
                ));
            };
            if let Err(error) = job.assign(pid) {
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

    /// Kill the whole tree. Idempotent; returns true the first time.
    pub fn kill(&self) -> bool {
        if self.state.killed.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.terminate();
        true
    }

    /// Signal the tree now, whatever `killed` says.
    fn terminate(&self) {
        #[cfg(unix)]
        {
            let pgid = self.state.pgid.load(Ordering::SeqCst);
            if pgid > 0 {
                // Safe: `killpg` on a group this session created.
                unsafe {
                    libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
                }
            }
        }
        #[cfg(windows)]
        if let Some(job) = &self.state.job {
            job.terminate_job();
        }
    }

    pub fn is_killed(&self) -> bool {
        self.state.killed.load(Ordering::SeqCst)
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

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::io;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenThread, PROCESS_SET_QUOTA, PROCESS_TERMINATE, ResumeThread,
        THREAD_SUSPEND_RESUME, TerminateProcess,
    };

    pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    pub const CREATE_SUSPENDED: u32 = 0x0000_0004;

    /// A Job Object handle. Closing it (Drop) kills every process still inside.
    #[derive(Debug)]
    pub struct Job(HANDLE);

    // The handle is just a kernel object reference; it may cross threads.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn create() -> Option<Job> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    CloseHandle(handle);
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
    }

    /// Kill one process by id, for the case where it never made it into its
    /// Job Object.
    pub fn terminate(pid: u32) {
        unsafe {
            let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !process.is_null() {
                TerminateProcess(process, 1);
                CloseHandle(process);
            }
        }
    }

    impl Drop for Job {
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
}
