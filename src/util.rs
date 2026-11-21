//! Small helpers shared across modules.
//! Deduped text and process helpers.

/// FNV-1a, 64 bit, rendered as 16 lowercase hex characters.
pub fn fnv1a64_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Cut a string to at most `max` bytes on a char boundary, appending `…`.
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let end = text.floor_char_boundary(max);
    let mut out = text[..end].to_string();
    out.push('…');
    out
}

/// Cut to at most `max` bytes on a char boundary, and say whether it was cut.
pub fn truncate_bytes_boundary(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let end = text.floor_char_boundary(max);
    (&text[..end], true)
}

/// First line of `text`, trimmed.
pub fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

/// Fold whitespace into single spaces and cut to `max` bytes.
pub fn one_line(text: &str, max: usize) -> String {
    truncate_chars(&text.split_whitespace().collect::<Vec<_>>().join(" "), max)
}

/// Fold newlines into a visible marker and cut to `max` bytes.
pub fn one_line_visible(text: &str, max: usize) -> String {
    truncate_chars(&text.replace('\n', " ↵ "), max)
}

/// Fold all control characters into spaces, then whitespace, for a safe label.
pub fn one_line_clean(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    one_line(&flat, max)
}

/// Replace every occurrence of a secret with `[redacted]`.
pub fn redact(text: &str, secret: Option<&str>) -> String {
    let Some(secret) = secret.filter(|secret| secret.len() >= 8) else {
        return text.to_string();
    };
    if !text.contains(secret) {
        return text.to_string();
    }
    text.replace(secret, "[redacted]")
}

/// A path as one string with forward slashes everywhere, so logs, notices and
/// `sed`/`rg` hints read the same on every platform.
pub fn path_string(path: &std::path::Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// `text` cut to at most `max` bytes, on a character boundary.
pub fn truncate_bytes(text: &str, max: usize) -> String {
    truncate_bytes_boundary(text, max).0.to_string()
}

fn usable_exe(path: &std::path::Path) -> bool {
    !path.to_string_lossy().ends_with(" (deleted)") && path.is_file()
}

/// Resolve a runnable path to this binary for self-spawns, falling back to a PATH lookup when
/// `current_exe()` is stale (binary replaced under a live session). See [`usable_exe`].
pub fn self_binary() -> Result<std::path::PathBuf, String> {
    let current = std::env::current_exe().ok();
    resolve_binary(
        current.as_deref(),
        &crate::envs::var_os("PATH").unwrap_or_default(),
    )
}

/// Pure core of [`self_binary`], so the stale-exe fallback can be tested.
fn resolve_binary(
    current: Option<&std::path::Path>,
    path: &std::ffi::OsStr,
) -> Result<std::path::PathBuf, String> {
    if let Some(path) = current.filter(|p| usable_exe(p)) {
        return Ok(path.to_path_buf());
    }
    let name = if cfg!(windows) { "kelir.exe" } else { "kelir" };
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| usable_exe(candidate))
        .ok_or_else(|| "cannot find the kelir binary (current_exe is stale and not on PATH)".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_or_deleted_exe_is_rejected() {
        // A real, existing path is usable.
        let here = std::env::current_exe().unwrap();
        assert!(usable_exe(&here));
        // A deleted path (the literal suffix Linux puts on /proc/self/exe) is not.
        let deleted = std::path::PathBuf::from(format!("{}/kelir (deleted)", "/usr/bin"));
        assert!(!usable_exe(&deleted));
        // A path that does not exist is not.
        assert!(!usable_exe(std::path::Path::new(
            "/no/such/kelir-binary-xyz"
        )));
    }

    #[test]
    #[cfg(unix)]
    fn resolver_falls_back_to_path_when_current_exe_is_stale() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("kelir");
        std::fs::write(&binary, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::ffi::OsString::from(dir.path().as_os_str());

        // A stale current_exe (deleted suffix) must resolve via PATH.
        let stale = std::path::PathBuf::from("/old/kelir (deleted)");
        assert_eq!(resolve_binary(Some(&stale), &path).unwrap(), binary);

        // A usable current_exe wins over PATH.
        let good = std::env::current_exe().unwrap();
        assert_eq!(resolve_binary(Some(&good), &path).unwrap(), good);

        // No usable exe anywhere is an error.
        let empty = std::ffi::OsString::new();
        assert!(resolve_binary(Some(&stale), &empty).is_err());
    }

    #[test]
    fn fnv_is_stable_and_hex() {
        assert_eq!(fnv1a64_hex(b""), "cbf29ce484222325");
        assert_eq!(fnv1a64_hex(b"a"), "af63dc4c8601ec8c");
        assert_eq!(fnv1a64_hex(b"a").len(), 16);
        assert_ne!(fnv1a64_hex(b"a"), fnv1a64_hex(b"b"));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate_chars("abc", 10), "abc");
        assert_eq!(truncate_chars("abcdef", 3), "abc…");
        // "é" is two bytes: cutting at 1 must not split it.
        assert_eq!(truncate_chars("ééé", 5), "éé…");
    }

    #[test]
    fn a_secret_is_redacted_and_short_strings_are_left_alone() {
        assert_eq!(
            redact(
                "Bearer fixture-key-abc into the void",
                Some("fixture-key-abc")
            ),
            "Bearer [redacted] into the void"
        );
        assert_eq!(
            redact("nothing to do", Some("fixture-key-abc")),
            "nothing to do"
        );
        assert_eq!(redact("nothing to do", None), "nothing to do");
        // Too short to be a credential: redacting it would mangle the message.
        assert_eq!(redact("a low bar", Some("a low")), "a low bar");
    }

    #[test]
    fn path_string_uses_forward_slashes() {
        let path = std::path::Path::new("state").join("sessions").join("ses_1");
        assert_eq!(path_string(&path), "state/sessions/ses_1");
    }
}
