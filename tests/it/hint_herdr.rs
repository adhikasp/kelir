//! hint herdr tests: a_failing_or_malformed_hc_omit, a_hanging_hc_costs_about_a_sec, a_missing_hc_omits_the_line, in_a_pane_the_frozen_system_ca, outside_herdr_there_is_no_line, the_same_pane_renders_the_same.
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::support::*;
use serde_json::{Value, json};

use super::hint_core::*;

#[tokio::test]
async fn in_a_pane_the_frozen_system_carries_one_herdr_line() {
    let hc = fake_hc(PANE_JSON, HcMode::Print);
    let run = run_with_pane(Some(&hc), Some("t9")).await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let system = system_text(&run);
    assert!(
        system.contains("herdr: pane t9, space spcA, tab tbB, where \"home / build\"\n"),
        "{system}"
    );
    let started = run.one_event("session_started");
    let herdr: Vec<&Value> = started["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|source| source["name"] == "herdr" && source["kind"] == "herdr")
        .collect();
    assert_eq!(herdr.len(), 1, "{started}");
    assert_eq!(herdr[0]["kind"], "herdr");
    assert_eq!(herdr[0]["path"], "");
    // No live count is ever frozen in.
    assert!(!system.contains("agents"), "{system}");
}

#[tokio::test]
async fn the_same_pane_renders_the_same_bytes() {
    let hc = fake_hc(PANE_JSON, HcMode::Print);
    let a = run_with_pane(Some(&hc), Some("t9")).await;
    let b = run_with_pane(Some(&hc), Some("t9")).await;
    // Everything else in the system is the same too, except the cwd each
    // sandbox has; the line itself is byte-identical.
    let line = |run: &Run| {
        system_text(run)
            .lines()
            .find(|line| line.starts_with("herdr:"))
            .map(str::to_string)
    };
    assert!(line(&a).is_some());
    assert_eq!(line(&a), line(&b));
}

#[tokio::test]
async fn outside_herdr_there_is_no_line_and_no_note() {
    // `hc` is on PATH, but there is no pane: it must not even be asked.
    let hc = fake_hc(PANE_JSON, HcMode::Print);
    let run = run_with_pane(Some(&hc), None).await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(!system_text(&run).contains("herdr:"));
    assert!(
        run.events_of("error")
            .iter()
            .all(|event| !event["message"].as_str().unwrap_or("").contains("herdr")),
        "{:?}",
        run.events_of("error")
    );
}

#[tokio::test]
async fn a_failing_or_malformed_hc_omits_the_line_with_a_note() {
    for (mode, json) in [
        (HcMode::Fail, ""),
        (HcMode::Print, "this is not json"),
        (HcMode::Print, "[1,2,3]"),
    ] {
        let hc = fake_hc(json, mode);
        let run = run_with_pane(Some(&hc), Some("t9")).await;
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(!system_text(&run).contains("herdr:"));
        let notes: Vec<String> = run
            .events_of("error")
            .iter()
            .filter(|event| event["where"] == "source")
            .filter_map(|event| event["message"].as_str().map(str::to_string))
            .collect();
        assert!(notes.iter().any(|note| note.contains("herdr")), "{notes:?}");
    }
}

#[tokio::test]
async fn a_hanging_hc_costs_about_a_second_and_never_fails_the_session() {
    let hc = fake_hc(PANE_JSON, HcMode::Hang);
    let started = std::time::Instant::now();
    let run = run_with_pane(Some(&hc), Some("t9")).await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "took {:?}",
        started.elapsed()
    );
    assert!(!system_text(&run).contains("herdr:"));
}

#[tokio::test]
async fn a_missing_hc_omits_the_line() {
    // PATH with nothing that is `hc` in it (the empty directory).
    let dir = tempfile::tempdir().unwrap();
    let only = dir.path().to_string_lossy().into_owned();
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let system_root = std::env::var("SystemRoot").unwrap_or_default();
    let path = if cfg!(windows) {
        format!("{only};{system_root}\\System32")
    } else {
        format!("{only}:/usr/bin:/bin")
    };
    let run = run_task(
        &mock,
        &["run", "--json", "hello"],
        &[("PATH", path.as_str()), ("HC_TERMINAL_ID", "t9")],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(!system_text(&run).contains("herdr:"));
}
