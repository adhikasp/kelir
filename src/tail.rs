//! Bounded tail reader for `kelir attach` (M11).
//!
//! The viewer must follow a growing `events.jsonl` with a bounded working set:
//! read the file a chunk at a time, hand out whole lines, tolerate a torn last
//! line (the writer may be mid-write) and skip a line that is absurdly large
//! rather than allocate it. No file is ever read whole.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// The largest line accepted from the event log. A log line is an event; normal
/// ones are a few KB (payloads over 10 KB spill to a blob). Bigger lines are
/// either corruption or somebody appending a blob by hand: skip them.
pub const MAX_LINE: usize = 1024 * 1024;
/// Bytes read per step of the tail.
const CHUNK: usize = 32 * 1024;

/// A cursor over one file. `offset` is the byte position of the next unread
/// byte; `pending` holds the bytes after the last complete line.
#[derive(Debug, Default)]
pub struct Tail {
    pub offset: u64,
    pending: Vec<u8>,
    /// True once a line was too long: the reader skipped to the next newline.
    pub skipped: usize,
}

impl Tail {
    /// Start at the beginning of the file.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start just before the last `n` *complete* lines. Returns how many lines
    /// were actually available (fewer than `n` in a short log).
    ///
    /// The file is scanned backwards in chunks, never held in memory beyond
    /// one chunk. `n = 0` means the whole log.
    pub fn from_last_lines(path: &Path, n: usize) -> io::Result<(Self, usize)> {
        let mut file = std::fs::File::open(path)?;
        let len = file.seek(SeekFrom::End(0))?;
        if n == 0 || len == 0 {
            return Ok((Self::new(), 0));
        }
        let mut start = len;
        let mut found = 0usize;
        let mut stopped_at_line = false;
        // Count newlines from the end. The final line need not end in a
        // newline; it still counts as one when it has bytes.
        'outer: while start > 0 {
            let want = CHUNK.min(start as usize) as u64;
            start -= want;
            file.seek(SeekFrom::Start(start))?;
            let mut chunk = vec![0u8; want as usize];
            file.read_exact(&mut chunk)?;
            for (index, byte) in chunk.iter().enumerate().rev() {
                if *byte == b'\n' {
                    // The newline ends a line. A newline as the very last byte
                    // does not add an empty line after it.
                    if start + index as u64 + 1 < len {
                        found += 1;
                        if found >= n {
                            start += index as u64 + 1;
                            stopped_at_line = true;
                            break 'outer;
                        }
                    }
                }
            }
        }
        // A newline at byte 0.. starts a line too: scanning backwards only
        // counts the newlines that *close* a later line, so the first line of
        // the file has to be added once when we scanned the whole file.
        if !stopped_at_line {
            found += 1;
        }
        Ok((Self::at(start), found.min(n)))
    }

    /// A cursor at a known byte offset.
    pub fn at(offset: u64) -> Self {
        Self {
            offset,
            pending: Vec::new(),
            skipped: 0,
        }
    }

    /// Read from `path` everything appended since the last call, returning
    /// whole lines as owned `String`s. A final line without a newline is held
    /// back (torn-line tolerance): the next call completes it when the writer
    /// appends the rest. Invalid UTF-8 is skipped as a bad line, not fatal.
    pub fn poll(&mut self, path: &Path) -> io::Result<Vec<String>> {
        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let len = file.seek(SeekFrom::End(0))?;
        if len < self.offset {
            // The file was replaced (rotation, a restored snapshot): start
            // over rather than read stale offsets from a different log.
            self.offset = 0;
            self.pending.clear();
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        let mut buf = [0u8; CHUNK];
        loop {
            let read = file.read(&mut buf)?;
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buf[..read]);
        }
        self.offset += bytes.len() as u64;
        self.pending.extend_from_slice(&bytes);
        let mut out = Vec::new();
        while let Some(index) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=index).collect();
            let line = &line[..line.len() - 1]; // the newline
            let line = line.strip_suffix(b"\r").unwrap_or(line); // CRLF logs
            if line.len() > MAX_LINE {
                self.skipped += 1;
                continue;
            }
            if let Ok(text) = std::str::from_utf8(line)
                && !text.trim().is_empty()
            {
                out.push(text.to_string());
            }
        }
        if self.pending.len() > MAX_LINE {
            // A line that grows without a newline (a runaway writer): drop up
            // to the last newline, or all of it, and keep the cursor bounded.
            self.skipped += 1;
            self.pending.clear();
        }
        Ok(out)
    }
}

/// How many bytes of a log lie before the last `n` complete lines, by a simple
/// forward count. Used to check [`Tail::from_last_lines`] in tests.
#[cfg(test)]
fn offset_of_last_n(bytes: &[u8], n: usize) -> usize {
    if n == 0 || bytes.is_empty() {
        return 0;
    }
    let mut lines: Vec<usize> = bytes
        .iter()
        .enumerate()
        .filter_map(|(i, b)| (*b == b'\n').then_some(i + 1))
        .collect();
    if bytes.last() != Some(&b'\n') {
        lines.push(bytes.len());
    }
    let index = lines.len().saturating_sub(n);
    if index == 0 { 0 } else { lines[index - 1] }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn log(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        file.flush().unwrap();
        file
    }

    #[test]
    fn complete_lines_are_returned_in_order() {
        let file = log(&["one", "two", "three"]);
        let mut tail = Tail::new();
        assert_eq!(tail.poll(file.path()).unwrap(), ["one", "two", "three"]);
        assert!(tail.poll(file.path()).unwrap().is_empty());
    }

    #[test]
    fn a_torn_last_line_is_held_until_the_rest_arrives() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(file, "one\n{{\"seq\":2,\"bo").unwrap();
        file.flush().unwrap();
        let mut tail = Tail::new();
        assert_eq!(tail.poll(file.path()).unwrap(), ["one"]);
        write!(file, "dy\":{{}}}}\nthree\n").unwrap();
        file.flush().unwrap();
        assert_eq!(
            tail.poll(file.path()).unwrap(),
            ["{\"seq\":2,\"body\":{}}", "three"]
        );
    }

    #[test]
    fn a_line_without_a_newline_is_never_emitted_as_partial() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(file, "{{\"seq\":1}}").unwrap();
        file.flush().unwrap();
        let mut tail = Tail::new();
        assert!(tail.poll(file.path()).unwrap().is_empty());
    }

    #[test]
    fn since_skips_to_the_last_n_lines() {
        let lines: Vec<String> = (0..40).map(|i| format!("line-{i}")).collect();
        let file = log(&lines.iter().map(String::as_str).collect::<Vec<_>>());
        let bytes = std::fs::read(file.path()).unwrap();
        for n in [0, 1, 5, 39, 40, 100] {
            let (mut tail, found) = Tail::from_last_lines(file.path(), n).unwrap();
            assert_eq!(found, n.min(40));
            assert_eq!(tail.offset as usize, offset_of_last_n(&bytes, n));
            let expected = if n == 0 { 40 } else { n.min(40) };
            let got = tail.poll(file.path()).unwrap();
            assert_eq!(got.len(), expected, "n={n}");
            assert_eq!(got.last().map(String::as_str), Some("line-39"));
        }
    }

    #[test]
    fn since_works_on_a_log_without_a_trailing_newline() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(file, "a\nb\nc").unwrap();
        file.flush().unwrap();
        let (mut tail, found) = Tail::from_last_lines(file.path(), 2).unwrap();
        assert_eq!(found, 2);
        assert_eq!(tail.poll(file.path()).unwrap(), ["b"]); // "c" is torn
    }

    #[test]
    fn a_huge_line_is_skipped_without_allocating_it() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "ok").unwrap();
        write!(file, "{}", "x".repeat(MAX_LINE + 10)).unwrap();
        writeln!(file, "\nafter").unwrap();
        file.flush().unwrap();
        let mut tail = Tail::new();
        assert_eq!(tail.poll(file.path()).unwrap(), ["ok", "after"]);
        assert_eq!(tail.skipped, 1);
    }

    #[test]
    fn a_runaway_unterminated_line_is_bounded() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(file, "{}", "x".repeat(MAX_LINE * 2)).unwrap();
        file.flush().unwrap();
        let mut tail = Tail::new();
        assert!(tail.poll(file.path()).unwrap().is_empty());
        assert!(tail.pending.len() <= MAX_LINE);
        assert!(tail.skipped >= 1);
    }

    #[test]
    fn a_replaced_file_starts_over() {
        let file = log(&["old-line", "another-stale-line"]);
        let mut tail = Tail::new();
        assert_eq!(
            tail.poll(file.path()).unwrap(),
            ["old-line", "another-stale-line"]
        );
        // A restored snapshot or rotated log is shorter than the cursor we
        // held into the old one. Start over instead of reading unrelated
        // bytes at that offset.
        std::fs::write(file.path(), b"new-content-here\n").unwrap();
        assert_eq!(tail.poll(file.path()).unwrap(), ["new-content-here"]);
    }

    #[test]
    fn reads_span_many_chunks() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for i in 0..2000 {
            writeln!(file, "{{\"seq\":{i},\"pad\":\"{}\"}}", "x".repeat(40)).unwrap();
        }
        file.flush().unwrap();
        let mut tail = Tail::new();
        assert_eq!(tail.poll(file.path()).unwrap().len(), 2000);
    }
}
