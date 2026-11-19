//! Small helpers shared across modules.

/// FNV-1a, 64 bit, rendered as 16 lowercase hex characters.
///
/// Used for request and system-prompt hashes: stable across runs and
/// processes, unlike `DefaultHasher`.
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
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_string();
    out.push('…');
    out
}

/// Replace every occurrence of a secret with `[redacted]`.
///
/// A provider (or the gateway in front of it) can echo the request, and a 4xx
/// that quotes the `Authorization` header would otherwise write a credential
/// into `events.jsonl` and into NDJSON stdout. Only the live key value is
/// known, so that is what is replaced; a key shorter than eight characters is
/// left alone because a two-letter "secret" would redact the whole message.
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

#[cfg(test)]
mod tests {
    use super::*;

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
