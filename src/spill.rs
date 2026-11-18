//! Big outputs: over the limit, the model sees a notice instead of the bytes.
//!
//! The full output is always kept in `commands/<call-id>/out`; the notice
//! carries byte and line counts, the path, a 1 KB head and tail, and a hint.

use std::path::Path;

pub const HEAD_BYTES: usize = 1024;
pub const TAIL_BYTES: usize = 1024;

/// Number of displayed lines: newline count, plus one for an unterminated
/// final line.
pub fn count_lines(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|byte| **byte == b'\n').count() as u64;
    if bytes.ends_with(b"\n") {
        newlines
    } else {
        newlines + 1
    }
}

/// A shell-friendly rendering of a path (`rg` and `sed` are POSIX tools, so
/// forward slashes even on Windows).
pub fn shell_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// What the model sees for a tool result, and whether it spilled.
pub fn format_result(
    bytes: &[u8],
    exit_code: Option<i32>,
    limit: usize,
    out_path: &Path,
) -> (String, bool) {
    if bytes.len() <= limit {
        let mut out = String::new();
        if let Some(code) = exit_code {
            if code != 0 {
                out.push_str(&format!("[exit code {code}]\n"));
            }
        }
        if bytes.is_empty() {
            out.push_str("[no output]");
        } else {
            out.push_str(&String::from_utf8_lossy(bytes));
        }
        return (out, false);
    }

    let path = shell_path(out_path);
    let mut out = String::new();
    out.push_str(&format!(
        "[output spilled: {} bytes, {} lines; full output at {}]\n",
        bytes.len(),
        count_lines(bytes),
        path
    ));
    if let Some(code) = exit_code {
        if code != 0 {
            out.push_str(&format!("[exit code {code}]\n"));
        }
    }
    out.push_str(&format!(
        "--- first {HEAD_BYTES} bytes ---\n{}\n",
        head(bytes, HEAD_BYTES)
    ));
    out.push_str(&format!(
        "--- last {TAIL_BYTES} bytes ---\n{}\n",
        tail(bytes, TAIL_BYTES)
    ));
    out.push_str(&format!(
        "---\nhint: rg -n '<pattern>' \"{path}\"   or   sed -n '1,80p' \"{path}\"\n"
    ));
    (out, true)
}

/// The first `n` bytes, lossily decoded so non-UTF-8 output never panics.
pub fn head(bytes: &[u8], n: usize) -> String {
    let end = bytes.len().min(n);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// The last `n` bytes, lossily decoded.
pub fn tail(bytes: &[u8], n: usize) -> String {
    let start = bytes.len().saturating_sub(n);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("state")
            .join("sessions")
            .join("ses_1")
            .join("out")
    }

    #[test]
    fn exactly_at_limit_does_not_spill() {
        let bytes = vec![b'a'; 10_240];
        let (result, spilled) = format_result(&bytes, Some(0), 10_240, &path());
        assert!(!spilled);
        assert_eq!(result.len(), 10_240);
        assert!(result.starts_with("aaa"));
    }

    #[test]
    fn one_byte_over_limit_spills() {
        let bytes = vec![b'a'; 10_241];
        let (result, spilled) = format_result(&bytes, Some(0), 10_240, &path());
        assert!(spilled);
        assert!(result.contains("10241 bytes"));
        assert!(result.contains("output spilled"));
        assert!(result.contains("first 1024 bytes"));
        assert!(result.contains("last 1024 bytes"));
        assert!(result.contains("hint: rg -n"));
    }

    #[test]
    fn spill_notice_carries_head_and_tail_content() {
        let mut bytes = b"FIRST".to_vec();
        bytes.extend(vec![b'-'; 20_000]);
        bytes.extend(b"LAST");
        let (result, spilled) = format_result(&bytes, None, 10_240, &path());
        assert!(spilled);
        assert!(result.contains("FIRST"));
        assert!(result.contains("LAST"));
        // The middle is not in the notice.
        assert!(!result.contains(&"-".repeat(5_000)));
    }

    #[test]
    fn non_utf8_output_does_not_panic_and_counts_bytes() {
        let mut bytes = vec![0xff, 0xfe, b'o', b'k'];
        bytes.extend(vec![0x80; 11_000]);
        let (result, spilled) = format_result(&bytes, Some(2), 10_240, &path());
        assert!(spilled);
        assert!(result.contains("11004 bytes"));
        assert!(result.contains("[exit code 2]"));
    }

    #[test]
    fn small_non_utf8_result_is_lossy() {
        let bytes = vec![b'a', 0xff, b'b'];
        let (result, spilled) = format_result(&bytes, Some(0), 10_240, &path());
        assert!(!spilled);
        assert!(result.starts_with('a'));
        assert!(result.ends_with('b'));
    }

    #[test]
    fn failing_command_is_labelled() {
        let (result, spilled) = format_result(b"boom\n", Some(3), 10_240, &path());
        assert!(!spilled);
        assert_eq!(result, "[exit code 3]\nboom\n");
    }

    #[test]
    fn empty_output_says_so() {
        let (result, _) = format_result(b"", Some(0), 10_240, &path());
        assert_eq!(result, "[no output]");
    }

    #[test]
    fn line_counting_handles_trailing_newline() {
        assert_eq!(count_lines(b""), 0);
        assert_eq!(count_lines(b"a"), 1);
        assert_eq!(count_lines(b"a\n"), 1);
        assert_eq!(count_lines(b"a\nb"), 2);
        assert_eq!(count_lines(b"a\nb\n"), 2);
        assert_eq!(count_lines(b"\n\n"), 2);
    }

    #[test]
    fn notice_path_uses_forward_slashes() {
        let (result, _) = format_result(&vec![b'x'; 11_000], None, 10_240, &path());
        assert!(!result.contains('\\'));
        assert!(result.contains("state/sessions/ses_1/out"));
    }
}
