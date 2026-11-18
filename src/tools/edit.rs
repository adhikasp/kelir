//! `edit`: exact string replacement in one file.
//!
//! `old` must match exactly once unless `replace_all` is set. The file's line
//! endings are preserved: a CRLF file is matched and rewritten with CRLF.
//! `old: ""` creates a missing file. The changed line numbers come back so the
//! model (and a gate) can look at the right place.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::spill;
use crate::tools::{Ctx, ToolOutcome};

pub fn run(args: &Value, ctx: &Ctx, call_id: &str) -> ToolOutcome {
    let outcome = match edit(args, ctx) {
        Ok(text) => ToolOutcome {
            outcome: "ok".into(),
            exit_code: None,
            bytes: text.len() as u64,
            lines: spill::count_lines(text.as_bytes()),
            out_path: String::new(),
            spilled: false,
            result: text,
        },
        Err(message) => ToolOutcome::error(message),
    };
    match persist(&outcome.result, ctx, call_id) {
        Ok(path) => ToolOutcome {
            out_path: path.display().to_string(),
            ..outcome
        },
        Err(message) => ToolOutcome::error(message),
    }
}

/// The edit itself; the caller adds the command file bookkeeping.
fn edit(args: &Value, ctx: &Ctx) -> Result<String, String> {
    let raw_path = args
        .get("path")
        .and_then(|v| v.as_str())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "edit: missing `path`".to_string())?;
    let old = args
        .get("old")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "edit: missing `old` (send \"\" to create a file)".to_string())?;
    let new = args
        .get("new")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "edit: missing `new`".to_string())?;
    let replace_all = args
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let path = resolve(&ctx.cwd, raw_path);
    let display = raw_path.to_string();

    if !path.exists() {
        if !old.is_empty() {
            return Err(format!("edit: file not found: {display}"));
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    format!("edit: cannot create {}: {error}", parent.display())
                })?;
            }
        }
        std::fs::write(&path, new.as_bytes())
            .map_err(|error| format!("edit: cannot write {display}: {error}"))?;
        return Ok(format!("created {display} ({} bytes)", new.len()));
    }

    let bytes =
        std::fs::read(&path).map_err(|error| format!("edit: cannot read {display}: {error}"))?;
    let content = String::from_utf8(bytes)
        .map_err(|_| format!("edit: {display} is not valid UTF-8; use bash for binary files"))?;

    if old.is_empty() {
        return Err(format!(
            "edit: `old` is empty but {display} already exists; pass the exact text to replace"
        ));
    }

    // A CRLF file is matched with CRLF: same-looking text works whether the
    // model wrote \n or \r\n, and the untouched lines keep their endings.
    let crlf = content.contains("\r\n");
    let (needle, replacement) = if crlf {
        (to_crlf(old), to_crlf(new))
    } else {
        (old.to_string(), new.to_string())
    };

    let matches = find_matches(&content, &needle);
    if matches.is_empty() {
        return Err(format!("edit: `old` not found in {display}"));
    }
    if matches.len() > 1 && !replace_all {
        return Err(format!(
            "edit: `old` matches {} times in {display}; pass replace_all or include more context",
            matches.len()
        ));
    }

    let line_numbers = line_numbers(&content, &matches);
    let updated = if replace_all {
        content.replace(&needle, &replacement)
    } else {
        let at = matches[0];
        let mut out = String::with_capacity(content.len() - needle.len() + replacement.len());
        out.push_str(&content[..at]);
        out.push_str(&replacement);
        out.push_str(&content[at + needle.len()..]);
        out
    };
    std::fs::write(&path, updated.as_bytes())
        .map_err(|error| format!("edit: cannot write {display}: {error}"))?;

    let count = if replace_all { matches.len() } else { 1 };
    Ok(format!(
        "edited {display}: replaced {count} {} at line{} {}",
        if count == 1 {
            "occurrence"
        } else {
            "occurrences"
        },
        if line_numbers.len() == 1 { "" } else { "s" },
        render_lines(&line_numbers)
    ))
}

fn persist(text: &str, ctx: &Ctx, call_id: &str) -> Result<PathBuf, String> {
    let dir = ctx.call_dir(call_id);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("edit: cannot create {}: {error}", dir.display()))?;
    let path = dir.join("out");
    std::fs::write(&path, text.as_bytes())
        .map_err(|error| format!("edit: cannot write {}: {error}", path.display()))?;
    Ok(path)
}

fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

/// Non-overlapping occurrences, left to right.
fn find_matches(haystack: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut start = 0;
    while let Some(offset) = haystack[start..].find(needle) {
        let at = start + offset;
        found.push(at);
        start = at + needle.len();
    }
    found
}

/// 1-based line numbers for byte offsets into `content`.
fn line_numbers(content: &str, offsets: &[usize]) -> Vec<usize> {
    offsets
        .iter()
        .map(|at| 1 + content[..*at].bytes().filter(|byte| *byte == b'\n').count())
        .collect()
}

fn render_lines(lines: &[usize]) -> String {
    const SHOWN: usize = 20;
    let mut rendered: Vec<String> = lines
        .iter()
        .take(SHOWN)
        .map(|line| line.to_string())
        .collect();
    if lines.len() > SHOWN {
        rendered.push(format!("… and {} more", lines.len() - SHOWN));
    }
    rendered.join(", ")
}

fn to_crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        ctx: Ctx,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("state").join("ses_test");
        std::fs::create_dir_all(&session).unwrap();
        Fixture {
            ctx: Ctx {
                session_dir: session,
                cwd: dir.path().to_path_buf(),
                shell: vec!["bash".into(), "-c".into()],
                spill_limit: 10_240,
            },
            _dir: dir,
        }
    }

    fn edit_args(path: &str, old: &str, new: &str) -> Value {
        serde_json::json!({"path": path, "old": old, "new": new})
    }

    #[test]
    fn replaces_one_occurrence_and_reports_the_line() {
        let f = fixture();
        std::fs::write(f.ctx.cwd.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let outcome = run(&edit_args("a.txt", "two", "TWO"), &f.ctx, "call_1");
        assert_eq!(outcome.outcome, "ok", "{}", outcome.result);
        assert_eq!(
            std::fs::read_to_string(f.ctx.cwd.join("a.txt")).unwrap(),
            "one\nTWO\nthree\n"
        );
        assert!(outcome.result.contains("line 2"), "{}", outcome.result);
        assert!(
            outcome.out_path.ends_with("call_1/out") || outcome.out_path.ends_with("call_1\\out")
        );
        assert!(std::path::Path::new(&outcome.out_path).exists());
    }

    #[test]
    fn missing_old_is_an_error() {
        let f = fixture();
        std::fs::write(f.ctx.cwd.join("a.txt"), "one\n").unwrap();
        let outcome = run(&edit_args("a.txt", "nope", "x"), &f.ctx, "c");
        assert_eq!(outcome.outcome, "error");
        assert!(outcome.result.contains("not found"));
        assert_eq!(
            std::fs::read_to_string(f.ctx.cwd.join("a.txt")).unwrap(),
            "one\n"
        );
    }

    #[test]
    fn ambiguous_old_is_refused_unless_replace_all() {
        let f = fixture();
        std::fs::write(f.ctx.cwd.join("a.txt"), "x\ny\nx\n").unwrap();
        let outcome = run(&edit_args("a.txt", "x", "z"), &f.ctx, "c1");
        assert_eq!(outcome.outcome, "error");
        assert!(outcome.result.contains("matches 2 times"));
        assert_eq!(
            std::fs::read_to_string(f.ctx.cwd.join("a.txt")).unwrap(),
            "x\ny\nx\n"
        );

        let args =
            serde_json::json!({"path": "a.txt", "old": "x", "new": "z", "replace_all": true});
        let outcome = run(&args, &f.ctx, "c2");
        assert_eq!(outcome.outcome, "ok", "{}", outcome.result);
        assert_eq!(
            std::fs::read_to_string(f.ctx.cwd.join("a.txt")).unwrap(),
            "z\ny\nz\n"
        );
        assert!(outcome.result.contains("replaced 2 occurrences"));
        assert!(outcome.result.contains("lines 1, 3"));
    }

    #[test]
    fn crlf_files_stay_crlf() {
        let f = fixture();
        let path = f.ctx.cwd.join("win.txt");
        std::fs::write(&path, "a\r\nb\r\nc\r\n").unwrap();
        let outcome = run(&edit_args("win.txt", "b", "B"), &f.ctx, "c");
        assert_eq!(outcome.outcome, "ok", "{}", outcome.result);
        assert_eq!(std::fs::read(&path).unwrap(), b"a\r\nB\r\nc\r\n");
    }

    #[test]
    fn lf_old_matches_inside_a_crlf_file() {
        let f = fixture();
        let path = f.ctx.cwd.join("win.txt");
        std::fs::write(&path, "a\r\nb\r\nc\r\n").unwrap();
        let outcome = run(&edit_args("win.txt", "b\nc", "b\nC"), &f.ctx, "c");
        assert_eq!(outcome.outcome, "ok", "{}", outcome.result);
        assert_eq!(std::fs::read(&path).unwrap(), b"a\r\nb\r\nC\r\n");
    }

    #[test]
    fn empty_old_creates_a_missing_file() {
        let f = fixture();
        let outcome = run(&edit_args("nested/dir/new.txt", "", "hello\n"), &f.ctx, "c");
        assert_eq!(outcome.outcome, "ok", "{}", outcome.result);
        assert_eq!(
            std::fs::read_to_string(f.ctx.cwd.join("nested/dir/new.txt")).unwrap(),
            "hello\n"
        );
        assert!(outcome.result.contains("created"));
    }

    #[test]
    fn empty_old_on_an_existing_file_is_refused() {
        let f = fixture();
        std::fs::write(f.ctx.cwd.join("a.txt"), "keep\n").unwrap();
        let outcome = run(&edit_args("a.txt", "", "x"), &f.ctx, "c");
        assert_eq!(outcome.outcome, "error");
        assert!(outcome.result.contains("already exists"));
    }

    #[test]
    fn missing_file_with_nonempty_old_is_an_error() {
        let f = fixture();
        let outcome = run(&edit_args("nope.txt", "a", "b"), &f.ctx, "c");
        assert_eq!(outcome.outcome, "error");
        assert!(outcome.result.contains("file not found"));
    }

    #[test]
    fn non_utf8_file_is_refused() {
        let f = fixture();
        std::fs::write(f.ctx.cwd.join("bin"), [0xff, 0xfe, 0x00]).unwrap();
        let outcome = run(&edit_args("bin", "a", "b"), &f.ctx, "c");
        assert_eq!(outcome.outcome, "error");
        assert!(outcome.result.contains("not valid UTF-8"));
    }

    #[test]
    fn missing_arguments_are_reported() {
        let f = fixture();
        for args in [
            serde_json::json!({}),
            serde_json::json!({"path": "a"}),
            serde_json::json!({"path": "a", "old": "x"}),
        ] {
            let outcome = run(&args, &f.ctx, "c");
            assert_eq!(outcome.outcome, "error");
        }
    }

    #[test]
    fn absolute_paths_are_used_as_given() {
        let f = fixture();
        let target = f.ctx.cwd.join("abs.txt");
        std::fs::write(&target, "x\n").unwrap();
        let outcome = run(&edit_args(target.to_str().unwrap(), "x", "y"), &f.ctx, "c");
        assert_eq!(outcome.outcome, "ok", "{}", outcome.result);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "y\n");
    }

    #[test]
    fn find_matches_is_non_overlapping() {
        assert_eq!(find_matches("aaaa", "aa"), vec![0, 2]);
        assert_eq!(find_matches("abc", "z"), Vec::<usize>::new());
    }
}
