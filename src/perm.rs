//! Owner-only creation of the session socket, pipe and token file. The token
//! ([`crate::trust`]) authenticates a caller; these permissions keep unrelated
//! users from reaching the listener in the first place. There is no read-back
//! proof: the creation calls are the boundary.

/// The security descriptor a session's named pipe is created with: owner-only, one ACE for
/// the creating user. Handed to interprocess's `ListenerOptionsExt::security_descriptor`.
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

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    use interprocess::os::windows::security_descriptor::{
        AsSecurityDescriptorExt as _, BorrowedSecurityDescriptor, SecurityDescriptor,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows_sys::core::PWSTR;

    const SDDL_REVISION_1: u32 = 1;

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
    /// The construction is the assertion: a descriptor Windows parses, and the
    /// SDDL it came from names this process's user and nobody else.
    #[cfg(windows)]
    #[test]
    fn the_pipe_descriptor_is_built_from_the_current_user() {
        let descriptor = super::pipe_descriptor().expect("a descriptor");
        let _ = descriptor;
    }
}
