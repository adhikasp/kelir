//! Tool-child process grouping, so a stop can kill a whole tree.
//!
//! Unix: the child leads its own process group (`process_group(0)`), which a
//! later `killpg` can signal. Windows: the child gets
//! `CREATE_NEW_PROCESS_GROUP` and every call owns a Job Object with
//! kill-on-close, so closing the handle after the call takes any stray
//! descendants with it.

use tokio::process::Command;

#[derive(Debug, Default)]
pub struct ProcessGroup {
    #[cfg(windows)]
    job: Option<win::Job>,
}

impl ProcessGroup {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            job: win::Job::create(),
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
                .creation_flags(win::CREATE_NEW_PROCESS_GROUP);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = command;
        }
    }

    /// Attach an already spawned child (Windows Job Object).
    #[allow(unused_variables)]
    pub fn assign(&self, pid: u32) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.assign(pid);
        }
    }

    /// Kill the group. Unix needs the group id, which the caller has as the
    /// child pid; that path lands with the `stop` verb in M1.
    #[allow(unused_variables)]
    pub fn kill(&self, pgid: Option<u32>) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

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

        pub fn assign(&self, pid: u32) {
            unsafe {
                let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                if process.is_null() {
                    return;
                }
                AssignProcessToJobObject(self.0, process);
                CloseHandle(process);
            }
        }

        pub fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.0, 1);
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
}
