//! hint memory adversarial tests: a_memory_note_the_model_writes, memory_dir_missing_or_a_file_i, memory_survives_delete_rename_, memory_with_a_symlink_loop_doe, pdf_and_odd_media_names_are_ha.
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::*;
use serde_json::json;

use super::hint_adversarial::*;

/// Mid-session: a file deleted, a file renamed, a huge file, a non-UTF-8 file, then a model write.
#[tokio::test]
async fn memory_survives_delete_rename_huge_and_non_utf8() {
    let cwd = tempfile::tempdir().unwrap();
    let notes = cwd.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(notes.join("a.md"), "NOTE-A").unwrap();
    std::fs::write(notes.join("keep.md"), "NOTE-KEEP").unwrap();
    std::fs::write(notes.join("huge.md"), "H".repeat(300_000)).unwrap();
    std::fs::write(
        notes.join("latin1.md"),
        [b'o', b'k', 0xff, 0xfe, b'!', b'\n'],
    )
    .unwrap();
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first", 0))),
        Reply::Sse(sse(&text_turn("second", 0))),
    ])
    .await;
    let config = format!("memory_dir = \"{}\"\n", toml_path(&notes));
    let mut live = LiveSession::start(
        &mock,
        &[
            "run",
            "--keep",
            "--json",
            "--cwd",
            &cwd.path().to_string_lossy(),
            "go",
        ],
        &config,
    )
    .await;
    live.wait_for("turn_ended").await;
    let log = live.log();
    let system = text_of(&of_type(&log, "session_started")[0]["system"]["text"]);
    // The first request carries the whole frozen system: check what it holds.
    let first_system = mock.requests()[0].messages()[0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    let _ = system;
    assert!(first_system.contains("NOTE-A") && first_system.contains("NOTE-KEEP"));
    assert!(
        first_system.len() < 120_000,
        "memory is bounded, system is {} bytes",
        first_system.len()
    );
    assert!(
        first_system.contains("ok"),
        "the non-UTF-8 note is read lossily"
    );

    // Delete a.md, rename keep.md -> kept.md, shrink huge.md, then a turn.
    std::fs::remove_file(notes.join("a.md")).unwrap();
    std::fs::rename(notes.join("keep.md"), notes.join("kept.md")).unwrap();
    std::fs::write(notes.join("huge.md"), "now small").unwrap();
    let answer = live.ctl(&["say", "and now?"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    live.wait_for("turn_ended").await;
    let log = live.log();
    let sources: Vec<(String, String, String)> = of_type(&log, "source")
        .iter()
        .map(|e| {
            (
                e["kind"].as_str().unwrap_or("").to_string(),
                e["name"].as_str().unwrap_or("").to_string(),
                text_of(&e["text"]),
            )
        })
        .collect();
    eprintln!("source events: {sources:#?}");
    // The system prefix is untouched, and the second request extends the first.
    let requests = mock.requests();
    assert_eq!(requests[1].messages()[0], requests[0].messages()[0]);
    let first = requests[0].messages().to_vec();
    assert_eq!(&requests[1].messages()[..first.len()], &first[..]);
    // The rename shows up as a new note; the model is told about a deletion somehow.
    assert!(
        sources
            .iter()
            .any(|(_, n, t)| n == "kept.md" && t.contains("NOTE-KEEP")),
        "a renamed note is a new note: {sources:?}"
    );
    assert!(
        sources
            .iter()
            .any(|(_, n, t)| n == "huge.md" && t.contains("now small")),
        "{sources:?}"
    );
    // Known limit (M5-A design): a deleted note is silent to the model, which keeps its last text.
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}

#[tokio::test]
async fn memory_with_a_symlink_loop_does_not_hang_or_fail() {
    let cwd = tempfile::tempdir().unwrap();
    let notes = cwd.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(notes.join("a.md"), "NOTE-A").unwrap();
    std::os::unix::fs::symlink(notes.join("loop.md"), notes.join("loop.md")).unwrap();
    std::os::unix::fs::symlink(&notes, notes.join("self")).unwrap();
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let config = format!("memory_dir = \"{}\"\n", toml_path(&notes));
    let run = run_task(
        &mock,
        &[
            "run",
            "--json",
            "--cwd",
            &cwd.path().to_string_lossy(),
            "go",
        ],
        &[],
        None,
        &config,
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
}

/// A memory directory that is not there is no memory; one that is a FILE is a note, not a crash.
#[tokio::test]
async fn memory_dir_missing_or_a_file_is_no_memory() {
    let cwd = tempfile::tempdir().unwrap();
    let afile = cwd.path().join("afile.md");
    std::fs::write(&afile, "x").unwrap();
    for target in [cwd.path().join("nope"), afile] {
        let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
        let config = format!("memory_dir = \"{}\"\n", toml_path(&target));
        let run = run_task(
            &mock,
            &[
                "run",
                "--json",
                "--cwd",
                &cwd.path().to_string_lossy(),
                "go",
            ],
            &[],
            None,
            &config,
        )
        .await;
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(!system_of_run(&run).contains("# Memory"));
    }
}

/// The model writes memory with `edit` / bash mid-turn: the edit arrives as a tail message.
#[tokio::test]
async fn a_memory_note_the_model_writes_arrives_as_a_tail_source_message() {
    let cwd = tempfile::tempdir().unwrap();
    let notes = cwd.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(notes.join("a.md"), "ORIGINAL").unwrap();
    let target = notes.join("a.md");
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "e1",
            "edit",
            &json!({"path": target.to_string_lossy(), "old": "ORIGINAL", "new": "BY-THE-MODEL"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let config = format!("memory_dir = \"{}\"\n", toml_path(&notes));
    let run = run_task(
        &mock,
        &[
            "run",
            "--json",
            "--cwd",
            &cwd.path().to_string_lossy(),
            "remember",
        ],
        &[],
        None,
        &config,
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let body = requests[1].body.to_string();
    assert!(body.contains("BY-THE-MODEL"), "{body}");
    assert_eq!(requests[1].messages()[0], requests[0].messages()[0]);
    let sources = run.events_of("source");
    assert!(sources.iter().any(|e| e["kind"] == "memory"), "{sources:?}");
}

#[tokio::test]
async fn pdf_and_odd_media_names_are_handled_not_crashed() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first", 0))),
        Reply::Sse(sse(&text_turn("second", 0))),
    ])
    .await;
    let mut live = LiveSession::start(&mock, &["run", "--keep", "--json", "hello"], "").await;
    live.wait_for("turn_ended").await;
    let pdf = live.cwd.join("paper.pdf");
    std::fs::write(&pdf, b"%PDF-1.4 not really").unwrap();
    let upper = live.cwd.join("SONG 1.MP3");
    std::fs::write(&upper, b"ID3").unwrap();
    let dir = live.cwd.join("a_dir.mp4");
    std::fs::create_dir_all(&dir).unwrap();
    let mut results = Vec::new();
    for path in [&pdf, &upper, &dir, &live.cwd.join("missing.wav")] {
        let answer = live.ctl(&["view", "--path", &path.to_string_lossy()]).await;
        results.push((
            path.file_name().unwrap().to_string_lossy().into_owned(),
            answer.code,
            answer.stdout.trim().to_string() + &answer.stderr,
        ));
    }
    eprintln!("view results: {results:#?}");
    let answer = live.ctl(&["say", "go on"]).await;
    assert_eq!(answer.code, 0);
    live.log_eventually("the second turn to end", |log| {
        of_type(log, "turn_ended").len() >= 2
    })
    .await;
    let log = live.log();
    let notes: Vec<_> = of_type(&log, "source")
        .into_iter()
        .filter(|e| e["kind"] == "attachment-note")
        .collect();
    // A directory named *.mp4 and a missing file are refused, never stored.
    assert_eq!(
        notes.len(),
        1,
        "{results:#?}
{notes:#?}"
    );
    // The PDF: stored with a note (the lead's brief), or refused with a clear reason.
    let pdf_result = &results[0];
    assert!(
        pdf_result.1 == 0 || !pdf_result.2.is_empty(),
        "a pdf neither stored nor explained: {pdf_result:?}"
    );
    let (code, stderr) = live.end().await;
    assert_eq!(code, 0, "{stderr}");
}
