//! Windows Job Object shim and handle helpers.
use std::ffi::c_void;
use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
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
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, ResumeThread, THREAD_SUSPEND_RESUME, TerminateProcess,
    WaitForSingleObject,
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
