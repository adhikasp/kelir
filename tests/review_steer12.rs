//! STEER-12 review: finding tests. The ignored ones prove a defect (run them with
//! `cargo test --test review_steer12 -- --ignored`); the others pin behaviour the
//! review relied on.

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn python() -> Option<&'static str> {
    for name in ["python3", "python"] {
        let ok = Command::new(name)
            .args([
                "-c",
                "import sys; sys.exit(0 if sys.version_info >= (3, 9) else 1)",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if ok {
            return Some(name);
        }
    }
    None
}

/// One session log: one step per command, each a single bash call.
fn log_with(commands: &[&str]) -> String {
    let mut out = String::new();
    let mut seq = 1;
    let mut push = |line: serde_json::Value| {
        out.push_str(&line.to_string());
        out.push('\n');
    };
    push(
        serde_json::json!({"seq": seq, "ts": "2026-11-20T00:00:00.0Z", "type": "session_started",
        "id": "ses_x", "cwd": "C:/nowhere", "provider": "openrouter", "model": "m"}),
    );
    for (index, command) in commands.iter().enumerate() {
        let step = index + 1;
        let second = index * 2;
        let call = format!("call_{step}");
        seq += 1;
        push(
            serde_json::json!({"seq": seq, "ts": format!("2026-11-20T00:00:{second:02}.0Z"),
            "type": "step_started", "step": step}),
        );
        seq += 1;
        push(
            serde_json::json!({"seq": seq, "ts": format!("2026-11-20T00:00:{second:02}.5Z"),
            "type": "assistant", "step": step, "text": "",
            "tool_calls": [{"id": call, "name": "bash", "args": {"command": command}}],
            "finish_reason": "tool_calls"}),
        );
        seq += 1;
        push(
            serde_json::json!({"seq": seq, "ts": format!("2026-11-20T00:00:{second:02}.5Z"),
            "type": "usage", "step": step, "prompt_tokens": 100, "completion_tokens": 10,
            "cached_tokens": 0, "cache_write_tokens": 0, "cost": 0.0}),
        );
        seq += 1;
        push(
            serde_json::json!({"seq": seq, "ts": format!("2026-11-20T00:00:{second:02}.6Z"),
            "type": "tool_started", "call_id": call, "name": "bash", "input": {"command": command}}),
        );
        seq += 1;
        push(
            serde_json::json!({"seq": seq, "ts": format!("2026-11-20T00:00:{second:02}.9Z"),
            "type": "tool_settled", "call_id": call, "outcome": "ok", "exit_code": 0,
            "bytes": 1, "lines": 1, "spilled": false, "result": "x"}),
        );
    }
    out
}

fn read_calls(commands: &[&str]) -> Option<u64> {
    let python = python()?;
    let dir = std::env::temp_dir().join(format!("kelir-review-steer12-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("events.jsonl");
    std::fs::write(&path, log_with(commands)).unwrap();
    let output = Command::new(python)
        .arg(repo().join("scripts/metrics/trajectory.py"))
        .arg(&path)
        .args(["--commits", "0", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    json["total"]["read_calls"].as_u64()
}

/// Sanity: the commands the new prompt asks for ARE read calls.
#[test]
fn a_labelled_batch_is_one_read_call() {
    let Some(reads) = read_calls(&[
        "echo === a; sed -n 1,5p x; echo === b; git grep -n y src; echo === c; grep -A3 z f",
        "cd /c/x && cat a.rs",
    ]) else {
        return;
    };
    assert_eq!(reads, 2);
}

/// FINDING S2 (metrics): the classifier ignores redirects and in-place flags, so a
/// *write* through `cat`/`sed`/`echo` counts as a read call. The new prompt tells the
/// model to write notes, and a model that writes them with `cat > notes.md <<EOF`
/// (the old logs hold three such calls) inflates the very number the AFTER table is
/// compared on.
#[test]
#[ignore = "finding S2: redirects and sed -i make a write a read call in trajectory.py"]
fn a_write_is_not_a_read_call() {
    let Some(reads) = read_calls(&[
        "cat > notes.md <<'EOF'\nplan\nEOF",
        "sed -i 's/a/b/' src/x.rs",
        "echo plan >> notes.md",
        "find . -name '*.tmp' -delete",
    ]) else {
        return;
    };
    assert_eq!(reads, 0, "four writes counted as reads");
}

/// FINDING N3 (metrics): `powershell -Command "Get-Content f"` is a read the brief
/// names ("powershell reads"); the double-quoted payload is one word and is not looked into.
#[test]
#[ignore = "finding N3: a double-quoted powershell -Command payload is not classified"]
fn a_quoted_powershell_read_is_a_read_call() {
    let Some(reads) = read_calls(&["powershell -NoProfile -Command \"Get-Content a.txt\""]) else {
        return;
    };
    assert_eq!(reads, 1);
}
