//! `edit`: exact string replacement in one file.
//! `old` must match exactly once unless `replace_all` is set.
//! The literal text is tried first, so a file with mixed line endings is edited exactly as
//! written.
//! Only when the literal does not match is the pattern retried with the file's line endings (CRLF

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::log::spill;
use crate::tools::{Ctx, ToolOutcome};

pub fn run(args: &Value, ctx: &Ctx, dir: &str) -> ToolOutcome {
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
    match persist(&outcome.result, ctx, dir) {
        Ok(path) => ToolOutcome {
            out_path: crate::util::path_string(&path),
            ..outcome
        },
        Err(message) => ToolOutcome::error(message),
    }
}

fn edit(args: &Value, ctx: &Ctx) -> Result<String, String> {
    let raw_path = args
        .get("path")
        .and_then(|v| v.as_str())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "edit: missing `path`".to_string())?;
    let hunks = parse_hunks(args)?;
    let path = resolve(&ctx.cwd, raw_path);
    let display = raw_path.to_string();

    if !path.exists() {
        return create(&path, &display, &hunks);
    }
    let bytes =
        std::fs::read(&path).map_err(|error| format!("edit: cannot read {display}: {error}"))?;
    let content = String::from_utf8(bytes)
        .map_err(|_| format!("edit: {display} is not valid UTF-8; use bash for binary files"))?;
    let mut text = content.clone();
    let mut reported: Vec<String> = Vec::new();
    for (index, hunk) in hunks.iter().enumerate() {
        let line = apply(&mut text, hunk, &display, index, hunks.len())?;
        reported.push(line);
    }
    if text == content {
        // Nothing changed the bytes (a hunk replaced text with itself). Writing
        // would touch the mtime for no reason; say what happened instead.
        return Ok(format!(
            "edited {display}: {}",
            if hunks.len() == 1 {
                "no change (the replacement equals the text it replaces)".to_string()
            } else {
                format!("{} changes, none of which altered the file", hunks.len())
            }
        ));
    }
    std::fs::write(&path, text.as_bytes())
        .map_err(|error| format!("edit: cannot write {display}: {error}"))?;
    Ok(join_report(&display, &reported))
}

struct Hunk {
    old: String,
    new: String,
    replace_all: bool,
}

/// Read the arguments into hunks: `old`/`new`/`replace_all`, or `edits`.
fn parse_hunks(args: &Value) -> Result<Vec<Hunk>, String> {
    let single = args.get("old").is_some() || args.get("new").is_some();
    let list = args.get("edits");
    match (single, list) {
        (true, Some(_)) => Err(
            "edit: send either `old`/`new` or `edits`, not both (one call, one form)".to_string(),
        ),
        (true, None) => Ok(vec![Hunk {
            old: args
                .get("old")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "edit: missing `old` (send \"\" to create a file)".to_string())?
                .to_string(),
            new: args
                .get("new")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "edit: missing `new`".to_string())?
                .to_string(),
            replace_all: args
                .get("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        }]),
        (false, Some(list)) => {
            let items = list
                .as_array()
                .ok_or_else(|| "edit: `edits` must be an array".to_string())?;
            if items.is_empty() {
                return Err("edit: `edits` is empty: send at least one {old, new}".to_string());
            }
            let mut hunks = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let at = index + 1;
                let old = item
                    .get("old")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("edit: edits[{at}] is missing `old`"))?;
                let new = item
                    .get("new")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("edit: edits[{at}] is missing `new`"))?;
                hunks.push(Hunk {
                    old: old.to_string(),
                    new: new.to_string(),
                    replace_all: item
                        .get("replace_all")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                });
            }
            Ok(hunks)
        }
        (false, None) => Err(
            "edit: missing `old` and `new` (send \"\" as `old` to create a file, or `edits`)"
                .to_string(),
        ),
    }
}
/// The file does not exist: only an empty `old` can create it, and the hunks
/// after that apply to what the first one wrote.
fn create(path: &Path, display: &str, hunks: &[Hunk]) -> Result<String, String> {
    let Some(first) = hunks.first() else {
        return Err("edit: no changes".to_string());
    };
    if !first.old.is_empty() {
        return Err(format!("edit: file not found: {display}"));
    }
    for (index, hunk) in hunks.iter().enumerate().skip(1) {
        if hunk.old.is_empty() {
            return Err(format!(
                "edit: edits[{}] has an empty `old`, but {display} does not exist yet (only the \
                 first hunk can create it: put the whole content there)",
                index + 1
            ));
        }
    }
    // The later hunks edit what the first one created, so one call can create
    // and refine a file atomically.
    let mut text = first.new.clone();
    let mut reported = vec![format!("created {display} ({} bytes)", text.len())];
    for (index, hunk) in hunks.iter().enumerate().skip(1) {
        let line = apply(&mut text, hunk, display, index, hunks.len())?;
        reported.push(line);
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("edit: cannot create {}: {error}", parent.display()))?;
    }
    std::fs::write(path, text.as_bytes())
        .map_err(|error| format!("edit: cannot write {display}: {error}"))?;
    Ok(join_report(display, &reported))
}

/// Apply one hunk to `text`, returning the one result line for it. Every check happens before
/// the caller writes anything, so a failure here leaves the file untouched on disk.
fn apply(
    text: &mut String,
    hunk: &Hunk,
    display: &str,
    index: usize,
    total: usize,
) -> Result<String, String> {
    let prefix = if total > 1 {
        format!("change {} of {total}: ", index + 1)
    } else {
        String::new()
    };
    if hunk.old.is_empty() {
        return Err(format!(
            "edit: {prefix}`old` is empty but {display} already exists; pass the exact text to \
             replace"
        ));
    }
    let (needle, replacement) = match_line_endings(text, &hunk.old, &hunk.new);
    let matches = find_matches(text, &needle);
    if matches.is_empty() {
        return Err(format!(
            "edit: {prefix}`old` not found in {display}. Nothing was written ({total} hunk{} in \
             this call, {index} applied)",
            if total == 1 { "" } else { "s" }
        ));
    }
    if matches.len() > 1 && !hunk.replace_all {
        return Err(format!(
            "edit: {prefix}`old` matches {} times in {display}; pass replace_all or include more \
             context. Nothing was written",
            matches.len()
        ));
    }
    let line_numbers = line_numbers(text, &matches);
    let count = if hunk.replace_all { matches.len() } else { 1 };
    if hunk.replace_all {
        *text = text.replace(&needle, &replacement);
    } else {
        let at = matches[0];
        text.replace_range(at..at + needle.len(), &replacement);
    }
    Ok(format!(
        "{prefix}replaced {count} {} at line{} {}",
        if count == 1 {
            "occurrence"
        } else {
            "occurrences"
        },
        if line_numbers.len() == 1 { "" } else { "s" },
        render_lines(&line_numbers)
    ))
}

/// The result text: one line per change under a headline, so a multi-edit reads
/// as a list while a single edit keeps its old, one-line shape.
fn join_report(display: &str, reported: &[String]) -> String {
    match reported {
        [] => format!("edited {display}: nothing to do"),
        [one] if one.starts_with("edited ") => one.clone(),
        [one] => format!("edited {display}: {one}"),
        many => {
            let mut out = format!("edited {display}: {} changes, all applied", many.len());
            for line in many {
                out.push_str("\n  ");
                out.push_str(line);
            }
            out
        }
    }
}

/// The pattern to use: the literal text if it occurs, else the same text with the file's line endings.
fn match_line_endings(content: &str, old: &str, new: &str) -> (String, String) {
    if !find_matches(content, old).is_empty() {
        return (old.to_string(), new.to_string());
    }
    if content.contains("\r\n") {
        let candidate = to_crlf(old);
        if !find_matches(content, &candidate).is_empty() {
            return (candidate, to_crlf(new));
        }
    }
    if old.contains("\r\n") {
        let candidate = old.replace("\r\n", "\n");
        if !find_matches(content, &candidate).is_empty() {
            return (candidate, new.replace("\r\n", "\n"));
        }
    }
    (old.to_string(), new.to_string())
}

fn persist(text: &str, ctx: &Ctx, dir: &str) -> Result<PathBuf, String> {
    let dir = ctx.call_dir(dir);
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
            ctx: Ctx::new(
                session,
                dir.path().to_path_buf(),
                vec!["bash".into(), "-c".into()],
                10_240,
                String::new(),
            ),
            _dir: dir,
        }
    }

    fn edit_args(path: &str, old: &str, new: &str) -> Value {
        serde_json::json!({"path": path, "old": old, "new": new})
    }

    #[test]
    fn single_edits_replace_create_and_refuse() {
        // (initial file and contents, args, expected outcome, expected contents, message fragment).
        type EditCase<'a> = (
            &'a str,
            Option<&'a str>,
            Value,
            &'a str,
            Option<&'a str>,
            &'a str,
        );
        let cases: [EditCase<'_>; 7] = [
            (
                "a.txt",
                Some("one\ntwo\nthree\n"),
                edit_args("a.txt", "two", "TWO"),
                "ok",
                Some("one\nTWO\nthree\n"),
                "line 2",
            ),
            (
                "a.txt",
                Some("one\n"),
                edit_args("a.txt", "nope", "x"),
                "error",
                Some("one\n"),
                "not found",
            ),
            (
                "nested/dir/new.txt",
                None,
                edit_args("nested/dir/new.txt", "", "hello\n"),
                "ok",
                Some("hello\n"),
                "created",
            ),
            (
                "a.txt",
                Some("keep\n"),
                edit_args("a.txt", "", "x"),
                "error",
                Some("keep\n"),
                "already exists",
            ),
            (
                "nope.txt",
                None,
                edit_args("nope.txt", "a", "b"),
                "error",
                None,
                "file not found",
            ),
            (
                "mixed.txt",
                Some("x\r\ny\n"),
                edit_args("mixed.txt", "x", "1\n2"),
                "ok",
                Some("1\n2\r\ny\n"),
                "",
            ),
            (
                "a.txt",
                Some("x\ny\nx\n"),
                serde_json::json!({"path":"a.txt","old":"x","new":"z","replace_all":true}),
                "ok",
                Some("z\ny\nz\n"),
                "replaced 2 occurrences",
            ),
        ];
        for (path, initial, args, status, expected, message) in cases {
            let f = fixture();
            if let Some(text) = initial {
                std::fs::write(f.ctx.cwd.join(path), text).unwrap();
            }
            let outcome = run(&args, &f.ctx, "c");
            assert_eq!(outcome.outcome, status, "{}", outcome.result);
            assert!(outcome.result.contains(message), "{}", outcome.result);
            if let Some(text) = expected {
                assert_eq!(
                    std::fs::read_to_string(f.ctx.cwd.join(path)).unwrap(),
                    text,
                    "{path}"
                );
            } else {
                assert!(!f.ctx.cwd.join(path).exists(), "{path}");
            }
        }
    }

    #[test]
    fn line_endings_and_path_forms_are_exact() {
        // (file name, initial bytes, args, expected bytes).
        let cases: [(&str, &[u8], Value, &[u8]); 6] = [
            (
                "win.txt",
                b"a\r\nb\r\nc\r\n",
                edit_args("win.txt", "b", "B"),
                b"a\r\nB\r\nc\r\n",
            ),
            (
                "win.txt",
                b"a\r\nb\r\nc\r\n",
                edit_args("win.txt", "b\nc", "b\nC"),
                b"a\r\nb\r\nC\r\n",
            ),
            (
                "unix.txt",
                b"a\nb\nc\n",
                edit_args("unix.txt", "b\r\nc", "B\r\nC"),
                b"a\nB\nC\n",
            ),
            (
                "mixed.txt",
                b"a\r\nb\nc\n",
                edit_args("mixed.txt", "b\nc", "B\nC"),
                b"a\r\nB\nC\n",
            ),
            (
                "mixed.txt",
                b"a\r\nb\nc\n",
                edit_args("mixed.txt", "a\nb", "A\nB"),
                b"A\r\nB\nc\n",
            ),
            // The literal text wins over the fallback: a single LF region stays LF.
            (
                "mixed.txt",
                b"x\r\ny\n",
                edit_args("mixed.txt", "x", "1\n2"),
                b"1\n2\r\ny\n",
            ),
        ];
        for (name, initial, args, expected) in cases {
            let f = fixture();
            let path = f.ctx.cwd.join(name);
            std::fs::write(&path, initial).unwrap();
            let outcome = run(&args, &f.ctx, "c");
            assert_eq!(outcome.outcome, "ok", "{}: {}", name, outcome.result);
            assert_eq!(std::fs::read(&path).unwrap(), expected, "{name}");
        }
        assert_eq!(find_matches("aaaa", "aa"), vec![0, 2]);
        assert_eq!(find_matches("abc", "z"), Vec::<usize>::new());
        let f = fixture();
        let target = f.ctx.cwd.join("abs.txt");
        std::fs::write(&target, "x\n").unwrap();
        let outcome = run(&edit_args(target.to_str().unwrap(), "x", "y"), &f.ctx, "c");
        assert_eq!(outcome.outcome, "ok", "{}", outcome.result);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "y\n");
    }

    #[test]
    fn a_non_utf8_file_is_refused() {
        let f = fixture();
        std::fs::write(f.ctx.cwd.join("bin"), [0xff, 0xfe, 0x00]).unwrap();
        let outcome = run(&edit_args("bin", "a", "b"), &f.ctx, "c");
        assert_eq!(outcome.outcome, "error");
        assert!(
            outcome.result.contains("not valid UTF-8"),
            "{}",
            outcome.result
        );
    }

    #[test]
    fn ambiguous_single_edit_is_refused_until_replace_all() {
        let f = fixture();
        std::fs::write(f.ctx.cwd.join("a.txt"), "x\ny\nx\n").unwrap();
        let outcome = run(&edit_args("a.txt", "x", "z"), &f.ctx, "c1");
        assert_eq!(outcome.outcome, "error");
        assert!(outcome.result.contains("matches 2 times"));
        assert_eq!(
            std::fs::read_to_string(f.ctx.cwd.join("a.txt")).unwrap(),
            "x\ny\nx\n"
        );
        // The successful single edit also reports its output file.
        std::fs::write(f.ctx.cwd.join("b.txt"), "one\ntwo\n").unwrap();
        let outcome = run(&edit_args("b.txt", "one", "ONE"), &f.ctx, "9-1-1");
        assert!(
            outcome.out_path.ends_with("9-1-1/out"),
            "{}",
            outcome.out_path
        );
        assert!(std::path::Path::new(&outcome.out_path).exists());
    }

    // ---------------------------------------------------------- multi-hunk

    fn edits(path: &str, pairs: &[(&str, &str)]) -> Value {
        let items: Vec<Value> = pairs
            .iter()
            .map(|(old, new)| serde_json::json!({"old": old, "new": new}))
            .collect();
        serde_json::json!({"path": path, "edits": items})
    }

    #[test]
    fn hunks_apply_in_order_or_are_all_refused() {
        // (path, initial contents, args, status, expected contents, message fragment).
        type EditCase<'a> = (
            &'a str,
            Option<&'a str>,
            Value,
            &'a str,
            Option<&'a str>,
            &'a str,
        );
        let cases: [EditCase<'_>; 7] = [
            (
                "a.rs",
                Some("fn a() {}\nfn b() {}\nfn c() {}\n"),
                edits(
                    "a.rs",
                    &[("fn a()", "fn alpha()"), ("fn c()", "fn gamma()")],
                ),
                "ok",
                Some("fn alpha() {}\nfn b() {}\nfn gamma() {}\n"),
                "2 changes, all applied",
            ),
            (
                "a.txt",
                Some("one\ntwo\n"),
                edits("a.txt", &[("one", "ONE"), ("nope", "x")]),
                "error",
                Some("one\ntwo\n"),
                "Nothing was written",
            ),
            (
                "a.txt",
                Some("x\ny\nx\n"),
                edits("a.txt", &[("y", "Y"), ("x", "z")]),
                "error",
                Some("x\ny\nx\n"),
                "matches 2 times",
            ),
            (
                "a.txt",
                Some("x\ny\nx\n"),
                serde_json::json!({"path":"a.txt","edits":[
                {"old":"y","new":"Y"},{"old":"x","new":"z","replace_all":true}]}),
                "ok",
                Some("z\nY\nz\n"),
                "",
            ),
            (
                "a.txt",
                Some("start\n"),
                edits("a.txt", &[("start", "mid"), ("mid", "end")]),
                "ok",
                Some("end\n"),
                "",
            ),
            (
                "new/deep/a.txt",
                None,
                edits("new/deep/a.txt", &[("", "alpha\nbeta\n"), ("beta", "BETA")]),
                "ok",
                Some("alpha\nBETA\n"),
                "created",
            ),
            (
                "new.txt",
                None,
                edits("new.txt", &[("", "content\n"), ("", "more\n")]),
                "error",
                None,
                "only the first hunk",
            ),
        ];
        for (path, initial, args, status, expected, message) in cases {
            let f = fixture();
            if let Some(text) = initial {
                std::fs::write(f.ctx.cwd.join(path), text).unwrap();
            }
            let outcome = run(&args, &f.ctx, "m");
            assert_eq!(outcome.outcome, status, "{}", outcome.result);
            assert!(outcome.result.contains(message), "{}", outcome.result);
            if let Some(text) = expected {
                assert_eq!(
                    std::fs::read_to_string(f.ctx.cwd.join(path)).unwrap(),
                    text,
                    "{path}"
                );
            } else {
                assert!(!f.ctx.cwd.join(path).exists(), "{path}");
            }
        }
    }

    /// An empty `edits` array is refused rather than silently doing nothing.
    #[test]
    fn an_empty_edits_array_is_refused() {
        let f = fixture();
        let outcome = run(
            &serde_json::json!({"path": "a.txt", "edits": []}),
            &f.ctx,
            "m",
        );
        assert_eq!(outcome.outcome, "error");
        assert!(
            outcome.result.contains("`edits` is empty"),
            "{}",
            outcome.result
        );
    }

    /// A single change keeps the one-line `edited ...` headline.
    #[test]
    fn a_single_edit_keeps_the_edited_headline() {
        let report = join_report("a.txt", &["one change".to_string()]);
        assert_eq!(report, "edited a.txt: one change");
    }

    /// `old`/`new` and `edits` in one call is refused, not silently merged.
    #[test]
    fn a_call_with_both_forms_is_refused() {
        let f = fixture();
        let outcome = run(
            &serde_json::json!({
                "path": "a.txt",
                "old": "x",
                "new": "y",
                "edits": [{"old": "x", "new": "y"}]
            }),
            &f.ctx,
            "m",
        );
        assert_eq!(outcome.outcome, "error");
        assert!(outcome.result.contains("not both"), "{}", outcome.result);
    }
}
