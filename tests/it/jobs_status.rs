//! jobs status tests.
#![allow(clippy::pedantic, clippy::nursery)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::support::*;
use serde_json::{Value, json};

use super::jobs::{clean, completions, finish, kelir, of_type, start, status, text, until};

/// A live session still refuses a second `--name` run, naming the holder pid,
/// without starting a turn. Ported from e2e_m9.rs.
#[tokio::test]
async fn a_name_held_by_a_live_session_is_refused_with_its_pid() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![Reply::Gated(gate.clone(), sse(&text_turn("slow", 0)))]).await;
    let sb = Sandbox::new("");
    let holder = start(&sb, &mock, &["run", "--name", "busy", "hold"], &[]).await;
    let pid = holder.id().unwrap();
    until("the first request", || mock.request_count() >= 1).await;

    let refused = kelir(&sb, &mock, &["run", "--name", "busy", "second"], &[]).await;
    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert!(
        refused.stderr.contains(&format!("pid {pid}")),
        "{}",
        refused.stderr
    );
    assert!(refused.stderr.contains("busy"), "{}", refused.stderr);
    assert_eq!(mock.request_count(), 1, "no second turn started");

    gate.notify_one();
    assert_eq!(finish(holder).await.code, 0);
    clean(&sb).await;
}

/// A corrupt name file is tolerated and replaced; STATUS files in the same
/// directory must not be treated as corrupt names. Ported from e2e_m9.rs.
#[tokio::test]
async fn a_corrupt_name_file_is_tolerated_and_replaced() {
    let mock = Mock::start(vec![text("ok")]).await;
    let sb = Sandbox::new("");
    std::fs::create_dir_all(sb.state.join("names")).unwrap();
    std::fs::write(sb.state.join("names").join("broken"), "{ nope").unwrap();
    let ls = kelir(&sb, &mock, &["ls", "sessions"], &[]).await;
    assert_eq!(ls.code, 0, "{}", ls.stderr);
    assert!(!ls.stderr.contains("panicked"), "{}", ls.stderr);
    let run = kelir(&sb, &mock, &["run", "--name", "broken", "go"], &[]).await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stderr.contains("unreadable"), "{}", run.stderr);
    let entry: Value = serde_json::from_str(
        &std::fs::read_to_string(sb.state.join("names").join("broken")).unwrap(),
    )
    .unwrap();
    assert_eq!(entry["session"], sb.only_session().as_str());
    clean(&sb).await;
}

/// STATUS is always a whole JSON record: a reader polling while the run is
/// live never sees a torn file, and sees `running`. Ported from review_m9.rs.
#[tokio::test]
async fn status_files_are_always_parseable_while_the_run_is_live() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![Reply::Gated(gate.clone(), sse(&text_turn("late", 0)))]).await;
    let sb = Sandbox::new("");
    let holder = start(
        &sb,
        &mock,
        &["run", "--name", "atomic", "--single-flight", "hold"],
        &[],
    )
    .await;
    let path = sb.state.join("names").join("atomic.status.json");
    until("STATUS", || path.exists()).await;
    let mut saw_running = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let text = std::fs::read_to_string(&path).unwrap();
        let value: Value = serde_json::from_str(&text).expect("a torn STATUS");
        saw_running |= value["outcome"] == "running";
        if mock.request_count() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(saw_running, "the running state was published");
    gate.notify_one();
    assert_eq!(finish(holder).await.code, 0);
    clean(&sb).await;
}

/// Two processes naming a free name at once still end with one session and
/// one `session_started`. Ported from review_m9.rs.
#[tokio::test]
async fn two_first_runs_of_one_new_name_do_not_share_a_session() {
    let mock = Mock::start(vec![
        Reply::SseAfter(Duration::from_secs(2), sse(&text_turn("one", 0))),
        text("two"),
    ])
    .await;
    let sb = Sandbox::new("");
    let a = start(&sb, &mock, &["run", "--name", "race2", "first"], &[]).await;
    let b = start(&sb, &mock, &["run", "--name", "race2", "second"], &[]).await;
    let out_a = finish(a).await;
    let out_b = finish(b).await;
    let ids = sb.session_ids();
    assert_eq!(ids.len(), 1, "one name must bind one session: {ids:?}");
    let log = sb.log(&ids[0]);
    assert_eq!(
        of_type(&log, "session_started").len(),
        1,
        "one session must start once (codes {} {})",
        out_a.code,
        out_b.code
    );
    clean(&sb).await;
}

/// A young unreadable lock is respected and the skip explains why. Ported
/// from review_m9fix.rs (without the removed history line).
#[tokio::test]
async fn an_unreadable_young_lock_is_respected_and_the_skip_explains_why() {
    let sb = Sandbox::new("");
    std::fs::create_dir_all(sb.state.join("names")).unwrap();
    let lock = sb.state.join("names").join("unreadable.lock");
    std::fs::write(&lock, b"not json at all").unwrap();
    let mock = Mock::start(vec![]).await;
    let out = kelir(
        &sb,
        &mock,
        &["run", "--name", "unreadable", "--single-flight", "go"],
        &[],
    )
    .await;
    assert_eq!(out.code, 75, "{}", out.stderr);
    assert!(out.stderr.contains("unreadable"), "{}", out.stderr);
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        "not json at all",
        "a fresh unreadable lock is never taken over"
    );
    let skipped = status(&sb, "unreadable");
    assert_eq!(skipped["outcome"], "skipped");
    assert_eq!(skipped["exit_code"], 75);
    assert!(
        skipped["error"]
            .as_str()
            .unwrap_or("")
            .contains("unreadable")
    );
    assert!(mock.requests().is_empty(), "no model turn on a skip");
    clean(&sb).await;
}

/// A dead OLD lock whose recorded pid (1) is alive is taken over because the
/// start time does not match. Ported from review_m9fix.rs.
#[tokio::test]
async fn an_old_lock_with_a_recycled_pid_is_taken_over() {
    let sb = Sandbox::new("");
    std::fs::create_dir_all(sb.state.join("names")).unwrap();
    let lock = sb.state.join("names").join("recycled.lock");
    let record = json!({
        "token": "dead-token",
        "pid": 1,
        "start": 1_u64,
        "cmd": "old kelir",
        "started": "2020-01-01T00:00:00Z"
    });
    std::fs::write(&lock, record.to_string()).unwrap();
    let old = std::fs::File::options().write(true).open(&lock).unwrap();
    old.set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000))
        .unwrap();
    drop(old);

    let mock = Mock::start(vec![text("fresh run")]).await;
    let out = kelir(
        &sb,
        &mock,
        &["run", "--name", "recycled", "--single-flight", "go"],
        &[],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(status(&sb, "recycled")["outcome"], "done");
    assert_eq!(completions(&mock).len(), 1);
    assert!(!lock.exists(), "the taken-over lock was released");
    clean(&sb).await;
}
