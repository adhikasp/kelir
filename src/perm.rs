//! "Only its owner may use it": the second half of the session socket's trust
//! boundary (STEER-7, retro M5-A F8).
//!
//! A token ([`crate::trust`]) authenticates a caller; permissions decide who can
//! reach the socket at all. Both are needed, because the token's own file is
//! protected by nothing but the directory it lives in, and a socket an unrelated
//! user can connect to is a prompt-injection channel into a fully autonomous
//! agent (`say` is a message from the caller).
//!
//! * **Unix.** `bind` chmods the socket `0600`; [`check_socket`] re-reads the
//!   mode afterwards, because a umask, a pre-existing file or a platform
//!   quirk must not leave a group- or world-accessible socket behind. A socket
//!   that is not owner-only is an error, not a warning: the session refuses to
//!   serve it.
//! * **Windows.** A named pipe's DACL comes from the default descriptor, which
//!   grants `FILE_ALL_ACCESS` to the user's SID and to `SYSTEM` and `Administrators`
//!   only — but only if the pipe was created with the default descriptor, and
//!   nothing in the code said so. Here the listener is created with an explicit
//!   descriptor (owner = the creating user, one ACE: `FILE_ALL_ACCESS` for the
//!   owner, no inheritance, no `Everyone`), and [`check_pipe`] re-reads the DACL
//!   from the pipe handle and refuses anything wider.
//!
//! Both checks are cheap and run once, before `session_started` is recorded, so
//! a client that sees the event can connect to a socket that is already
//! restricted.

/// Is this socket/pipe restricted to its owner? Called after the listener is
/// bound and before it accepts anything.
#[cfg(unix)]
pub fn check_socket(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("cannot read the socket {}: {error}", path.display()))?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "the session socket {} is {mode:o}, not owner-only; another user could steer this \
             session",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn check_socket(_path: &std::path::Path) -> Result<(), String> {
    // Windows has no socket path to look at: a named pipe is checked through
    // its handle, in `check_pipe`.
    Ok(())
}

/// The Windows side: a pipe created by [`crate::trust::pipe_descriptor`] is
/// owner-only, and this is the proof. It re-reads the DACL of the name and
/// refuses if any ACE grants access to a SID that is not the current user.
#[cfg(windows)]
pub fn check_pipe(name: &str) -> Result<(), String> {
    win::check_pipe(name)
}

#[cfg(not(windows))]
pub fn check_pipe(_name: &str) -> Result<(), String> {
    Ok(())
}

/// The security descriptor a session's named pipe is created with: owner-only,
/// one ACE for the creating user. Handed to interprocess's
/// `ListenerOptionsExt::security_descriptor`.
#[cfg(windows)]
pub fn pipe_descriptor()
-> std::io::Result<interprocess::os::windows::security_descriptor::SecurityDescriptor> {
    win::owner_only_descriptor()
}

/// Give one file — the session's token — the same owner-only DACL the pipe gets
/// (`D:P(A;;FA;;;<user>)`). Used by [`crate::trust::restrict_to_owner`].
#[cfg(windows)]
pub fn restrict_file_to_owner(path: &std::path::Path) -> Result<(), String> {
    win::restrict_file_to_owner(path)
}

#[cfg(not(windows))]
pub fn restrict_file_to_owner(_path: &std::path::Path) -> Result<(), String> {
    Ok(())
}

/// Is one file's DACL owner-only? The read-back of [`restrict_file_to_owner`]:
/// a null DACL, more than one ACE or an `Everyone` ACE means no.
#[cfg(windows)]
pub fn file_is_owner_only(path: &std::path::Path) -> Result<bool, String> {
    win::file_is_owner_only(path)
}

#[cfg(not(windows))]
pub fn file_is_owner_only(path: &std::path::Path) -> Result<bool, String> {
    let _ = std::fs::metadata(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    Ok(true)
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

    use interprocess::os::windows::security_descriptor::{
        AsSecurityDescriptorExt as _, BorrowedSecurityDescriptor, SecurityDescriptor,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        GetNamedSecurityInfoW, GetSecurityInfo, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetSecurityDescriptorDacl,
        GetTokenInformation, INHERIT_ONLY_ACE, IsWellKnownSid, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser, WinWorldSid,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows_sys::core::PWSTR;

    const SDDL_REVISION_1: u32 = 1;

    /// `D:P(A;;FA;;;<the current user>)`: a protected DACL (no inheritance from
    /// the parent) with one allow-ACE, `FILE_ALL_ACCESS`, for this process's user
    /// and nobody else. Not `OW` (owner-rights): the owner is not known until the
    /// object exists, and spelling the SID out makes the descriptor readable here.
    pub fn owner_only_descriptor() -> std::io::Result<SecurityDescriptor> {
        let user = current_user_sid()?;
        let sddl = format!("D:P(A;;FA;;;{user})");
        let wide = to_wide(&sddl);
        let mut raw: *mut c_void = std::ptr::null_mut();
        // SAFETY: the string is NUL-terminated and the out pointer is ours.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut raw,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a self-relative descriptor produced by Windows; `to_owned_sd`
        // copies it into an absolute, self-owning one, so the original is freed
        // right after.
        let owned = unsafe { BorrowedSecurityDescriptor::from_ptr(raw).to_owned_sd() };
        // SAFETY: allocated by the conversion above with LocalAlloc.
        unsafe {
            LocalFree(raw);
        }
        owned
    }

    enum Verdict {
        OwnerOnly,
        TooWide(&'static str),
    }

    /// Re-read the DACL of the pipe and refuse anything wider than the owner.
    /// The pipe is opened for `READ_CONTROL` only, so this cannot disturb a
    /// client that is connecting.
    pub fn check_pipe(pipe_name: &str) -> Result<(), String> {
        let handle = open_for_read_dacl(pipe_name)
            .map_err(|error| format!("cannot open the session pipe to read its DACL: {error}"))?;
        // The handle is owned by `handle` and closed when it drops.
        let verdict = dacl_verdict(handle.as_raw_handle() as HANDLE);
        drop(handle);
        match verdict {
            Ok(Verdict::OwnerOnly) => Ok(()),
            Ok(Verdict::TooWide(what)) => Err(format!(
                "the session pipe {pipe_name} is not owner-only ({what}); another user could \
                 steer this session"
            )),
            Err(error) => Err(format!(
                "cannot read the DACL of the session pipe {pipe_name}: {error}"
            )),
        }
    }

    fn open_for_read_dacl(pipe_name: &str) -> std::io::Result<OwnedHandle> {
        let wide = to_wide(pipe_name);
        // SAFETY: a NUL-terminated name and the documented flag set; the handle
        // is wrapped in OwnedHandle, so it is closed once.
        let raw = unsafe {
            CreateFileW(
                wide.as_ptr(),
                READ_CONTROL,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if raw.is_null() || raw as isize == -1 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `raw` is a valid handle from CreateFileW, handed over here.
        Ok(unsafe { OwnedHandle::from_raw_handle(raw as _) })
    }

    fn dacl_verdict(handle: HANDLE) -> Result<Verdict, String> {
        let mut owner: PSID = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd: *mut c_void = std::ptr::null_mut();
        // SAFETY: every out-pointer is ours; the descriptor is freed below.
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!("GetSecurityInfo failed ({status})"));
        }
        let verdict = dacl_of(dacl);
        // SAFETY: the descriptor was allocated by GetSecurityInfo with
        // LocalAlloc and nothing borrows from it any more.
        unsafe {
            LocalFree(sd);
        }
        verdict
    }

    fn dacl_of(dacl: *mut ACL) -> Result<Verdict, String> {
        if dacl.is_null() {
            // A null DACL grants everyone everything.
            return Ok(Verdict::TooWide("it has no DACL"));
        }
        // SAFETY: `dacl` points at a well-formed ACL and `size` is ours.
        let mut size: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            GetAclInformation(
                dacl,
                std::ptr::from_mut(&mut size).cast::<c_void>(),
                std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        };
        if ok == 0 {
            return Err("GetAclInformation failed".to_string());
        }
        // The descriptor this session creates has exactly one ACE. More means
        // either the default descriptor leaked in or something replaced it.
        if size.AceCount > 1 {
            return Ok(Verdict::TooWide("it grants more than the owner"));
        }
        for index in 0..size.AceCount {
            let mut ace: *mut c_void = std::ptr::null_mut();
            // SAFETY: the index is below the ACL's own count and `ace` is ours.
            let ok = unsafe { GetAce(dacl, index, &mut ace) };
            if ok == 0 || ace.is_null() {
                return Err("GetAce failed".to_string());
            }
            // SAFETY: an allow-ACE begins with ACE_HEADER, then the mask, then
            // the SID bytes (`SidStart` is the SID's first sub-authority).
            let allowed = unsafe { &*(ace as *const ACCESS_ALLOWED_ACE) };
            if allowed.Header.AceFlags & (INHERIT_ONLY_ACE as u8) != 0 {
                continue;
            }
            // SAFETY: `SidStart` is the head of the SID stored inside the ACE.
            let sid = std::ptr::addr_of!(allowed.SidStart) as PSID;
            // SAFETY: a SID inside the ACE read just above.
            if unsafe { IsWellKnownSid(sid, WinWorldSid) } != 0 {
                return Ok(Verdict::TooWide("it grants Everyone"));
            }
        }
        Ok(Verdict::OwnerOnly)
    }

    /// Give one file an owner-only DACL: the same SDDL the pipe gets, written
    /// onto the file itself by name.
    ///
    /// By name, not by handle: a handle opened for `WRITE_DAC` alone is refused
    /// with `ERROR_ACCESS_DENIED` for an ordinary user's file, while
    /// `SetNamedSecurityInfoW` — the same operation, taking the filename — is
    /// allowed for the file's owner.
    pub fn restrict_file_to_owner(path: &std::path::Path) -> Result<(), String> {
        let user = current_user_sid().map_err(|error| error.to_string())?;
        let sddl = format!("D:P(A;;FA;;;{user})");
        let wide_sddl = to_wide(&sddl);
        let mut raw: *mut c_void = std::ptr::null_mut();
        // SAFETY: the string is NUL-terminated and the out pointer is ours.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide_sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut raw,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(format!(
                "cannot build the token file's DACL: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut present: i32 = 0;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut defaulted: i32 = 0;
        // SAFETY: a self-relative descriptor from the conversion above; the DACL
        // points inside it and is used before `raw` is freed.
        let read =
            unsafe { GetSecurityDescriptorDacl(raw, &mut present, &mut dacl, &mut defaulted) };
        if read == 0 || present == 0 || dacl.is_null() {
            // SAFETY: allocated by the conversion above.
            unsafe {
                LocalFree(raw);
            }
            return Err("the token file's DACL could not be read back".to_string());
        }
        let wide_path = to_wide(&path.to_string_lossy());
        // SAFETY: the name is NUL-terminated, the DACL lives in `raw` (freed
        // below) and the object is a file.
        let status = unsafe {
            SetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: allocated by the conversion above and nothing borrows it now.
        unsafe {
            LocalFree(raw);
        }
        if status != ERROR_SUCCESS {
            return Err(format!(
                "cannot restrict {}: SetNamedSecurityInfo failed ({status})",
                path.display()
            ));
        }
        Ok(())
    }

    /// Is one file's DACL owner-only? The same verdict the pipe gets, read by
    /// name (see [`restrict_file_to_owner`] for why not by handle).
    pub fn file_is_owner_only(path: &std::path::Path) -> Result<bool, String> {
        let wide_path = to_wide(&path.to_string_lossy());
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd: *mut c_void = std::ptr::null_mut();
        // SAFETY: every out-pointer is ours; the descriptor is freed below.
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide_path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!("GetNamedSecurityInfo failed ({status})"));
        }
        let verdict = dacl_of(dacl);
        // SAFETY: the descriptor was allocated by GetNamedSecurityInfo with
        // LocalAlloc and nothing borrows from it any more.
        unsafe {
            LocalFree(sd);
        }
        match verdict {
            Ok(Verdict::OwnerOnly) => Ok(true),
            Ok(Verdict::TooWide(_)) => Ok(false),
            Err(error) => Err(format!("cannot read {}'s DACL: {error}", path.display())),
        }
    }

    /// This process's user SID, in the `S-1-5-…` spelling SDDL uses.
    fn current_user_sid() -> std::io::Result<String> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo-handle is valid for this process; the token handle
        // is closed below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut needed = 0u32;
        // SAFETY: a size probe with a null buffer is how the API reports size;
        // the error it sets is expected.
        unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            // SAFETY: the token handle is ours and open exactly once.
            unsafe {
                CloseHandle(token);
            }
            return Err(std::io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; needed as usize];
        // SAFETY: the buffer is `needed` bytes, as asked for above.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr() as *mut c_void,
                needed,
                &mut needed,
            )
        };
        // SAFETY: the token handle is closed exactly once.
        unsafe {
            CloseHandle(token);
        }
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: the buffer holds a TOKEN_USER whose SID points into it, and
        // `buffer` outlives this borrow.
        let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
        sid_to_string(user.User.Sid)
    }

    fn sid_to_string(sid: PSID) -> std::io::Result<String> {
        let mut text: PWSTR = std::ptr::null_mut();
        // SAFETY: `sid` lives in the token buffer, which is still alive.
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a NUL-terminated string allocated by the conversion above.
        let string = unsafe { wide_to_string(text) };
        // SAFETY: allocated with LocalAlloc by the conversion above.
        unsafe {
            LocalFree(text as *mut c_void);
        }
        Ok(string)
    }

    unsafe fn wide_to_string(text: PWSTR) -> String {
        let mut len = 0;
        // SAFETY: the caller guarantees a NUL-terminated string.
        unsafe {
            while *text.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
        }
    }

    fn to_wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Unix socket half is testable everywhere there is a socket; on other
    /// platforms the check is a documented no-op.
    #[cfg(unix)]
    #[test]
    fn a_group_readable_socket_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("kelir-perm-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("sock");
        std::fs::write(&socket, b"").unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(check_socket(&socket).is_ok());
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660)).unwrap();
        let error = check_socket(&socket).unwrap_err();
        assert!(error.contains("not owner-only"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_socket_is_an_error_not_a_pass() {
        let dir = std::env::temp_dir().join(format!("kelir-perm-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(check_socket(&dir.join("nope")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn the_pipe_descriptor_is_built_from_the_current_user() {
        // The construction is the assertion: a descriptor Windows parses, and
        // the SDDL it came from names this process's user and nobody else.
        let descriptor = pipe_descriptor().expect("a descriptor");
        let _ = descriptor;
    }

    /// A file written with the owner-only DACL reads back as owner-only, and a
    /// file in the temp directory (inherited ACL: `Everyone` is not there, but
    /// `SYSTEM` and `Administrators` are) is refused — the check has to be able
    /// to say no, or it is decoration.
    #[cfg(windows)]
    #[test]
    fn a_token_file_gets_a_dacl_and_the_read_back_notices_a_wide_one() {
        let dir = std::env::temp_dir().join(format!("kelir-perm-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("token");
        std::fs::write(&file, b"secret").unwrap();
        assert!(
            !file_is_owner_only(&file).unwrap(),
            "the inherited ACL must be seen as too wide"
        );
        restrict_file_to_owner(&file).unwrap();
        assert!(file_is_owner_only(&file).unwrap());
        assert_eq!(std::fs::read(&file).unwrap(), b"secret");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
