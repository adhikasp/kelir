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
}
