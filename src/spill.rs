//! Big outputs: over the limit, the model sees a notice instead of the bytes.
//! The full output always goes to `commands/<n>/out`; memory holds only what the model could see
//! anyway: the first `limit` bytes (inline result), the first 1 KB (head) and a 1 KB tail ring,
//! plus byte and line counts.
//! Output of any size therefore costs the same ~11 KB of memory.

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

/// Markers a shell prints when it ran a command that was cut short: Git bash's heredoc warning is the
/// one seen in the wild ( seq 344, seq 613: a 9 KB heredoc wrote an 8 KB file and **exited 0**).
const TRUNCATION_MARKERS: [&str; 3] = [
    "delimited by end-of-file",
    "unexpected EOF",
    "unexpected end of file",
];

/// The notice the model gets when its output looks truncated.
pub fn truncation_notice(text: &str, exit_code: Option<i32>) -> Option<&'static str> {
    if looks_cut(text, exit_code) {
        return Some(
            "[kelir: your command may have been cut short: write big files with `edit` \
             (empty `old`) instead of a heredoc, and check the exit code of each command.]",
        );
    }
    None
}

/// Does this output look like a command that was **cut**, rather than one that merely printed a marker
/// (: `git grep "unexpected EOF" src` made the model re-read a command that was never cut)?
fn looks_cut(text: &str, exit_code: Option<i32>) -> bool {
    let failed = exit_code.is_some_and(|code| code != 0);
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(NOTICE_LINES);
    lines.iter().enumerate().any(|(index, line)| {
        if !TRUNCATION_MARKERS
            .iter()
            .any(|marker| line.contains(marker))
        {
            return false;
        }
        shell_diagnostic(line) || failed || index >= start
    })
}

/// Is this line a shell's own message about the command (`bash: …`, `sh: -c: line 3: …`) rather than
/// program output?
fn shell_diagnostic(line: &str) -> bool {
    let Some((prefix, rest)) = line.split_once(": ") else {
        return false;
    };
    let prefix = prefix.trim();
    let shell = prefix
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(prefix)
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    let known = matches!(
        shell.as_str(),
        "bash" | "sh" | "zsh" | "dash" | "ksh" | "cmd" | "powershell" | "pwsh" | "fish"
    );
    known || rest.to_ascii_lowercase().starts_with("warning:")
}

/// How many trailing lines count as "the end" of a command's output when the command succeeded.
pub const NOTICE_LINES: usize = 3;

/// Line count from a streaming read: newlines, plus one for an unterminated
/// final line.
fn line_count(bytes: u64, newlines: u64, last: Option<u8>) -> u64 {
    if bytes == 0 {
        0
    } else if last == Some(b'\n') {
        newlines
    } else {
        newlines + 1
    }
}

/// What a stopped call had already written when its process tree was killed.
#[derive(Debug, Clone, PartialEq)]
pub struct Partial {
    pub bytes: u64,
    pub lines: u64,
    /// Up to the last [`TAIL_BYTES`] bytes, lossily decoded.
    pub tail: String,
}

/// Read a tool's output file to describe its partial output: counts, and a tail of at most
/// [`TAIL_BYTES`]. Streamed in 8 KB chunks, so memory stays bounded whatever the file size.
pub fn partial(path: &Path) -> std::io::Result<Partial> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut buffer = [0u8; 8192];
    let mut bytes = 0u64;
    let mut newlines = 0u64;
    let mut last = None;
    let mut tail: VecDeque<u8> = VecDeque::with_capacity(TAIL_BYTES);
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        bytes += read as u64;
        newlines += chunk.iter().filter(|byte| **byte == b'\n').count() as u64;
        last = chunk.last().copied();
        if chunk.len() >= TAIL_BYTES {
            tail.clear();
            tail.extend(&chunk[chunk.len() - TAIL_BYTES..]);
        } else {
            tail.extend(chunk.iter().copied());
            while tail.len() > TAIL_BYTES {
                tail.pop_front();
            }
        }
    }
    let tail: Vec<u8> = tail.iter().copied().collect();
    Ok(Partial {
        bytes,
        lines: line_count(bytes, newlines, last),
        tail: String::from_utf8_lossy(&tail).into_owned(),
    })
}

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
    /// `=== label` lines seen anywhere in the stream: (byte offset, label).
    sections: Vec<(u64, String)>,
    line_start: u64,
    line_head: Vec<u8>,
}

/// At most this many section labels are remembered, each cut to [`LABEL_BYTES`].
const MAX_SECTIONS: usize = 40;
const LABEL_BYTES: usize = 40;

impl Capture {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            inline: Vec::new(),
            tail: VecDeque::with_capacity(TAIL_BYTES),
            bytes: 0,
            newlines: 0,
            last: None,
            sections: Vec::new(),
            line_start: 0,
            line_head: Vec::new(),
        }
    }

    /// Follow line starts so `=== label` headers are known even when the bytes
    /// around them are not kept.
    fn track_sections(&mut self, chunk: &[u8]) {
        let base = self.bytes;
        for (i, byte) in chunk.iter().enumerate() {
            if *byte == b'\n' {
                self.close_line();
                self.line_start = base + i as u64 + 1;
            } else if self.line_head.len() < LABEL_BYTES + 4 {
                self.line_head.push(*byte);
            }
        }
    }

    fn close_line(&mut self) {
        if let Some(rest) = self.line_head.strip_prefix(b"=== ")
            && self.sections.len() < MAX_SECTIONS
        {
            let label = String::from_utf8_lossy(rest);
            let label = label.trim_end_matches('\r').trim().to_string();
            self.sections.push((self.line_start, label));
        }
        self.line_head.clear();
    }

    #[optimize(speed)]
    pub fn push(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        self.track_sections(chunk);
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
        line_count(self.bytes, self.newlines, self.last)
    }

    pub fn spilled(&self) -> bool {
        self.bytes > self.limit as u64
    }

    /// Bytes held in memory: bounded by construction, whatever the output size.
    pub fn stored_bytes(&self) -> usize {
        self.inline.len() + self.tail.len()
    }

    /// The last [`TAIL_BYTES`] of output, lossily decoded: what a promoted call
    /// shows the model so it can see where the job had got to.
    pub fn tail_text(&self) -> String {
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        String::from_utf8_lossy(&tail).into_owned()
    }

    /// For a labelled batch (`echo === name` between reads): every section and
    /// the byte offset it starts at, so a header that fell in the cut is not lost.
    fn section_map(&self) -> Option<String> {
        let mut sections = self.sections.clone();
        if let Some(rest) = self.line_head.strip_prefix(b"=== ") {
            // An unterminated last line is still a header.
            if sections.len() < MAX_SECTIONS {
                let label = String::from_utf8_lossy(rest).trim().to_string();
                sections.push((self.line_start, label));
            }
        }
        if sections.is_empty() {
            return None;
        }
        let list: Vec<String> = sections
            .iter()
            .map(|(offset, label)| format!("{label}@{offset}"))
            .collect();
        let more = if sections.len() >= MAX_SECTIONS {
            ", ..."
        } else {
            ""
        };
        Some(format!(
            "--- sections (=== label@byte offset); only the first {HEAD_BYTES} and last {TAIL_BYTES} bytes are shown, the others are in the file: {}{more}\n",
            list.join(", ")
        ))
    }

    /// What the model sees, and whether the output spilled.
    pub fn render(&self, exit_code: Option<i32>, out_path: &Path) -> (String, bool) {
        if !self.spilled() {
            let mut out = String::new();
            if let Some(code) = exit_code
                && code != 0
            {
                out.push_str(&format!("[exit code {code}]\n"));
            }
            if self.inline.is_empty() {
                out.push_str("[no output]");
            } else {
                out.push_str(&String::from_utf8_lossy(&self.inline));
            }
            if let Some(notice) = truncation_notice(&out, exit_code) {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(notice);
                out.push('\n');
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
        if let Some(code) = exit_code
            && code != 0
        {
            out.push_str(&format!("[exit code {code}]\n"));
        }
        out.push_str(&format!("--- first {HEAD_BYTES} bytes ---\n{head}\n"));
        out.push_str(&format!("--- last {TAIL_BYTES} bytes ---\n{tail}\n"));
        if let Some(line) = self.section_map() {
            out.push_str(&line);
        }
        if let Some(notice) =
            truncation_notice(&head, exit_code).or_else(|| truncation_notice(&tail, exit_code))
        {
            out.push_str(notice);
            out.push('\n');
        }
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
    fn a_big_labelled_batch_lists_every_section_with_its_offset() {
        let mut capture = Capture::new(10 * 1024);
        let mut total = 0u64;
        let mut expect = Vec::new();
        for name in ["spec", "deviations", "grep", "tail"] {
            expect.push(format!("{name}@{total}"));
            let header = format!("=== {name}\n");
            let body = format!("{}\n", "x".repeat(10_000));
            capture.push(header.as_bytes());
            // Split a chunk mid-line to cover a header spanning pushes.
            capture.push(body.as_bytes());
            total += (header.len() + body.len()) as u64;
        }
        assert!(total > 40_000);
        let (result, spilled) = capture.render(Some(0), &path());
        assert!(spilled);
        let map = result
            .lines()
            .find(|line| line.starts_with("--- sections"))
            .expect("a section map");
        for item in &expect {
            assert!(map.contains(item), "{item} missing from {map}");
        }
        assert!(result.len() < 3_500, "notice stays small: {}", result.len());
    }

    #[test]
    fn a_header_split_across_pushes_is_still_found() {
        let mut capture = Capture::new(8);
        capture.push(b"abc\n==");
        capture.push(b"= one\nzzzzzzzzzz\n=== two");
        let (result, _) = capture.render(Some(0), &path());
        assert!(result.contains("one@4"), "{result}");
        assert!(result.contains("two@"), "{result}");
    }

    #[test]
    fn plain_spills_have_no_section_line() {
        let mut capture = Capture::new(8);
        capture.push(&[b'x'; 40]);
        let (result, _) = capture.render(Some(0), &path());
        assert!(!result.contains("--- sections"));
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

    fn temp_out(name: &str, bytes: &[u8]) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("kelir-partial-{name}-{}", ulid::Ulid::generate()));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn partial_output_is_counted_and_tailed_within_a_kilobyte() {
        let mut bytes = b"first line\n".to_vec();
        bytes.extend(vec![b'-'; 20_000]);
        bytes.extend(b"LAST LINE\n");
        let path = temp_out("big", &bytes);
        let partial = partial(&path).unwrap();
        assert_eq!(partial.bytes, bytes.len() as u64);
        assert_eq!(partial.lines, count_lines(&bytes));
        assert!(partial.tail.ends_with("LAST LINE\n"));
        assert_eq!(partial.tail.len(), TAIL_BYTES);
        assert!(!partial.tail.contains("first line"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn partial_output_of_a_small_file_is_the_whole_file() {
        let path = temp_out("small", b"one\ntwo");
        let partial = partial(&path).unwrap();
        assert_eq!(partial.bytes, 7);
        assert_eq!(partial.lines, 2);
        assert_eq!(partial.tail, "one\ntwo");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn partial_output_of_a_missing_file_is_an_error() {
        let path = std::env::temp_dir().join(format!("kelir-none-{}", ulid::Ulid::generate()));
        assert!(partial(&path).is_err());
    }

    /// the notice is for output that *was* cut.
    #[test]
    fn a_marker_in_the_middle_of_a_successful_output_is_not_truncation() {
        // A marker inside a long successful output is not evidence of truncation.
        let mut lines = vec![
            "src/spill.rs:33: const TRUNCATION_MARKERS: [&str; 3] = [".to_string(),
            "src/spill.rs:34: \"delimited by end-of-file\",".to_string(),
            "src/spill.rs:35: \"unexpected EOF\",".to_string(),
        ];
        for index in 0..20 {
            lines.push(format!("docs/book.md:{index}: prose about EOF handling"));
        }
        let text = lines.join("\n");
        assert_eq!(truncation_notice(&text, Some(0)), None, "{text}");
        assert_eq!(truncation_notice(&text, None), None);
        // From a command that failed, a marker anywhere is the notice's case: a
        // failure is at least worth a look at the bytes that ran.
        assert!(truncation_notice(&text, Some(1)).is_some());
        // A grep hit is not a shell diagnostic, but a marker in a successful command's *last* lines still
        // counts: the notice is a cheap hint at the tail, and that is enough (the reviewer's wording: "exit
        let last = ["a", "b", "c", "d", "src/x.rs:1: \"unexpected EOF\""].join("\n");
        assert!(truncation_notice(&last, Some(0)).is_some());
    }

    /// A cut heredoc is the case the notice is for: the shell says so, it usually still exits 0, and bash
    /// prints the warning *before* the body it did read, so the marker is nowhere near the end.
    #[test]
    fn a_shell_diagnostic_is_noticed_wherever_the_shell_printed_it() {
        let early = [
            "bash: warning: here-document at line 1 delimited by end-of-file (wanted `X')",
            "line one",
            "line two",
            "WRONG",
        ]
        .join("\n");
        assert!(truncation_notice(&early, Some(0)).is_some());
        // The `sh` variant of the same message, without `warning:`.
        let dash = ["sh: -c: line 1: unexpected EOF while looking for matching `\"'"].join("\n");
        assert!(truncation_notice(&dash, Some(2)).is_some());
        // A failed command's marker, wherever it is.
        let failed = ["a", "b", "boom: unexpected EOF", "more", "tail"].join("\n");
        assert!(truncation_notice(&failed, Some(1)).is_some());
        // A successful command with the marker only in its middle, and not a
        // shell's own line: quiet.
        let mid = ["a", "b", "prose: unexpected EOF", "c", "d", "e", "the end"].join("\n");
        assert_eq!(truncation_notice(&mid, Some(0)), None);
        // A successful command whose last lines carry the marker: a notice.
        let tail = ["doing work", "line", "still: unexpected EOF"].join("\n");
        assert!(truncation_notice(&tail, Some(0)).is_some());
    }

    #[test]
    fn ordinary_output_gets_no_notice() {
        assert_eq!(
            truncation_notice(
                "all good
",
                Some(0)
            ),
            None
        );
        assert_eq!(truncation_notice("", Some(0)), None);
        assert_eq!(
            truncation_notice(
                "error: EOF expected
",
                Some(1)
            ),
            None
        );
    }
}
