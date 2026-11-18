//! Big outputs: over the limit, the model sees a notice instead of the bytes.
//!
//! The full output always goes to `commands/<n>/out`; memory holds only what
//! the model could see anyway: the first `limit` bytes (inline result), the
//! first 1 KB (head) and a 1 KB tail ring, plus byte and line counts. Output
//! of any size therefore costs the same ~11 KB of memory.

use std::collections::VecDeque;
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

/// A bounded view of one command's output.
#[derive(Debug)]
pub struct Capture {
    limit: usize,
    /// The first `limit` bytes: enough for the inline result, and the head.
    inline: Vec<u8>,
    /// The last [`TAIL_BYTES`] bytes.
    tail: VecDeque<u8>,
    bytes: u64,
    newlines: u64,
    last: Option<u8>,
}

impl Capture {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            inline: Vec::new(),
            tail: VecDeque::with_capacity(TAIL_BYTES),
            bytes: 0,
            newlines: 0,
            last: None,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        self.bytes += chunk.len() as u64;
        self.newlines += chunk.iter().filter(|byte| **byte == b'\n').count() as u64;
        self.last = chunk.last().copied();
        if self.inline.len() < self.limit {
            let take = (self.limit - self.inline.len()).min(chunk.len());
            self.inline.extend_from_slice(&chunk[..take]);
        }
        if chunk.len() >= TAIL_BYTES {
            self.tail.clear();
            self.tail.extend(&chunk[chunk.len() - TAIL_BYTES..]);
        } else {
            self.tail.extend(chunk.iter().copied());
            while self.tail.len() > TAIL_BYTES {
                self.tail.pop_front();
            }
        }
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn lines(&self) -> u64 {
        if self.bytes == 0 {
            0
        } else if self.last == Some(b'\n') {
            self.newlines
        } else {
            self.newlines + 1
        }
    }

    pub fn spilled(&self) -> bool {
        self.bytes > self.limit as u64
    }

    /// Bytes held in memory: bounded by construction, whatever the output size.
    pub fn stored_bytes(&self) -> usize {
        self.inline.len() + self.tail.len()
    }

    /// What the model sees, and whether the output spilled.
    pub fn render(&self, exit_code: Option<i32>, out_path: &Path) -> (String, bool) {
        if !self.spilled() {
            let mut out = String::new();
            if let Some(code) = exit_code {
                if code != 0 {
                    out.push_str(&format!("[exit code {code}]\n"));
                }
            }
            if self.inline.is_empty() {
                out.push_str("[no output]");
            } else {
                out.push_str(&String::from_utf8_lossy(&self.inline));
            }
            return (out, false);
        }

        let path = crate::util::path_string(out_path);
        let head = lossy_head(&self.inline, HEAD_BYTES);
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        let tail = String::from_utf8_lossy(&tail);

        let mut out = String::new();
        out.push_str(&format!(
            "[output spilled: {} bytes, {} lines; full output at {}]\n",
            self.bytes,
            self.lines(),
            path
        ));
        if let Some(code) = exit_code {
            if code != 0 {
                out.push_str(&format!("[exit code {code}]\n"));
            }
        }
        out.push_str(&format!("--- first {HEAD_BYTES} bytes ---\n{head}\n"));
        out.push_str(&format!("--- last {TAIL_BYTES} bytes ---\n{tail}\n"));
        // `sed -n` first: rg is not installed everywhere, sed always is.
        out.push_str(&format!(
            "---\nhint: sed -n '1,80p' \"{path}\"   or   rg -n '<pattern>' \"{path}\"\n"
        ));
        (out, true)
    }
}

fn lossy_head(bytes: &[u8], n: usize) -> String {
    let end = bytes.len().min(n);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
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

    fn capture(limit: usize, bytes: &[u8]) -> Capture {
        let mut capture = Capture::new(limit);
        capture.push(bytes);
        capture
    }

    #[test]
    fn exactly_at_limit_does_not_spill() {
        let bytes = vec![b'a'; 10_240];
        let capture = capture(10_240, &bytes);
        assert!(!capture.spilled());
        let (result, spilled) = capture.render(Some(0), &path());
        assert!(!spilled);
        assert_eq!(result.len(), 10_240);
        assert!(result.starts_with("aaa"));
    }

    #[test]
    fn one_byte_over_limit_spills() {
        let bytes = vec![b'a'; 10_241];
        let capture = capture(10_240, &bytes);
        assert!(capture.spilled());
        let (result, spilled) = capture.render(Some(0), &path());
        assert!(spilled);
        assert!(result.contains("10241 bytes"));
        assert!(result.contains("output spilled"));
        assert!(result.contains("first 1024 bytes"));
        assert!(result.contains("last 1024 bytes"));
        assert!(result.contains("hint: sed -n"));
    }

    #[test]
    fn spill_notice_carries_head_and_tail_content() {
        let mut bytes = b"FIRST".to_vec();
        bytes.extend(vec![b'-'; 20_000]);
        bytes.extend(b"LAST");
        let capture = capture(10_240, &bytes);
        let (result, spilled) = capture.render(None, &path());
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
        let capture = capture(10_240, &bytes);
        let (result, spilled) = capture.render(Some(2), &path());
        assert!(spilled);
        assert!(result.contains("11004 bytes"));
        assert!(result.contains("[exit code 2]"));
    }

    #[test]
    fn small_non_utf8_result_is_lossy() {
        let bytes = vec![b'a', 0xff, b'b'];
        let capture = capture(10_240, &bytes);
        let (result, spilled) = capture.render(Some(0), &path());
        assert!(!spilled);
        assert!(result.starts_with('a'));
        assert!(result.ends_with('b'));
    }

    #[test]
    fn failing_command_is_labelled() {
        let capture = capture(10_240, b"boom\n");
        let (result, spilled) = capture.render(Some(3), &path());
        assert!(!spilled);
        assert_eq!(result, "[exit code 3]\nboom\n");
    }

    #[test]
    fn empty_output_says_so() {
        let capture = capture(10_240, b"");
        let (result, _) = capture.render(Some(0), &path());
        assert_eq!(result, "[no output]");
        assert_eq!(capture.lines(), 0);
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
    fn capture_line_count_matches_count_lines_across_chunks() {
        for text in ["", "a", "a\n", "a\nb", "a\nb\n", "\n\n"] {
            let mut capture = Capture::new(10_240);
            for chunk in text.as_bytes().chunks(1) {
                capture.push(chunk);
            }
            assert_eq!(capture.lines(), count_lines(text.as_bytes()), "{text:?}");
        }
    }

    #[test]
    fn tail_ring_keeps_the_last_kilobyte_across_many_chunks() {
        let mut capture = Capture::new(10_240);
        for _ in 0..100 {
            capture.push(&vec![b'a'; 500]);
        }
        capture.push(b"ENDMARK");
        let (result, spilled) = capture.render(None, &path());
        assert!(spilled);
        assert!(result.contains("ENDMARK"));
        assert_eq!(capture.lines(), 1);
        assert_eq!(capture.tail.len(), TAIL_BYTES);
    }

    #[test]
    fn memory_is_bounded_by_construction() {
        // ~50 MB through the capture in 8 KB chunks: what we keep is the inline
        // window (10 KB) plus the tail ring (1 KB), never the output itself.
        let mut capture = Capture::new(10_240);
        let chunk = vec![b'x'; 8192];
        for _ in 0..6_104 {
            capture.push(&chunk);
        }
        assert!(capture.bytes() >= 50_000_000, "{}", capture.bytes());
        assert!(capture.spilled());
        assert!(
            capture.stored_bytes() <= 10_240 + TAIL_BYTES,
            "stored {} bytes",
            capture.stored_bytes()
        );
        let (result, spilled) = capture.render(Some(0), &path());
        assert!(spilled);
        assert!(result.len() < 4_000);
        assert_eq!(capture.lines(), 1);
    }

    #[test]
    fn notice_path_uses_forward_slashes() {
        let capture = capture(10_240, &vec![b'x'; 11_000]);
        let (result, _) = capture.render(None, &path());
        assert!(!result.contains('\\'));
        assert!(result.contains("state/sessions/ses_1/out"));
        // sed is offered first, rg second.
        let sed = result.find("sed -n").unwrap();
        let rg = result.find("rg -n").unwrap();
        assert!(sed < rg);
    }
}
