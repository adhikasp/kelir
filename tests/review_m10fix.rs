//! Fresh-reviewer proving tests for the M10 fix pass. These do NOT modify
//! product code; they drive the real `kelir channel` binary against the
//! scripted mock + fake bridge, with the same TreeGuard/watchdog harness the
//! shipped reviewer tests use.
//!
//! r15 proves the "latent" B4 fix: two NEW conversations in one channel
//! process each get their own named session (task-local plan id) and both
//! answer. It also captures driver stderr on failure to make the r14 flake
//! diagnosable.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

mod support;
use support::*;

fn shells() -> usize {
    #[cfg(windows)]
    let output = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq sh.exe", "/NH", "/FO", "CSV"])
        .output();
    #[cfg(not(windows))]
    let output = std::process::Command::new("ps").arg("-e").output();
    output
        .map(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            text.lines()
                .filter(|line| {
                    line.to_ascii_lowercase().contains("sh.exe") || line.contains(" sh ")
                })
                .count()
        })
        .unwrap_or(0)
}

struct ChannelTest {
    sandbox: Sandbox,
    inbox: PathBuf,
    replies: PathBuf,
    events: PathBuf,
    args_path: PathBuf,
}

impl ChannelTest {
    fn new() -> ChannelTest {
        let sandbox = Sandbox::new("");
        let inbox = sandbox.cwd.join("fix-inbox.jsonl");
        let replies = sandbox.cwd.join("fix-replies.jsonl");
        let events = sandbox.cwd.join("fix-events.jsonl");
        let args_path = sandbox.cwd.join("fix-reply-args.jsonl");
        for path in [&inbox, &replies, &events, &args_path] {
            let _ = std::fs::write(path, "");
        }
        let mcp = format!(
            "[servers.fake]\ncommand = '{}'\nargs = ['{}']\nchannel = true\n\
             [servers.fake.env]\nMCP_CHAN_INBOX = '{}'\nMCP_CHAN_REPLIES = '{}'\n\
             MCP_CHAN_EVENTS = '{}'\nMCP_CHAN_ARGS = '{}'\nMCP_CHAN_SECRETS = 'fix-canary-9e2b'\n",
            if cfg!(windows) { "python" } else { "python3" },
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/live/review_channel_server.py"
            )
            .replace('\\', "/"),
            inbox.to_string_lossy().replace('\\', "/"),
            replies.to_string_lossy().replace('\\', "/"),
            events.to_string_lossy().replace('\\', "/"),
            args_path.to_string_lossy().replace('\\', "/"),
        );
        std::fs::write(sandbox.cwd.join("mcp.toml"), mcp).unwrap();
        ChannelTest {
            sandbox,
            inbox,
            replies,
            events,
            args_path,
        }
    }

    async fn inject(&self, message: Value) {
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&self.inbox)
            .await
            .unwrap();
        use tokio::io::AsyncWriteExt;
        file.write_all(format!("{}\n", message).as_bytes())
            .await
            .unwrap();
    }

    fn lines(&self, path: &PathBuf) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    async fn wait_for<F: Fn(&[Value]) -> bool>(&self, path: &PathBuf, ready: F) -> Vec<Value> {
        let started = std::time::Instant::now();
        loop {
            let lines = self.lines(path);
            if ready(&lines) {
                return lines;
            }
            assert!(
                started.elapsed() < Duration::from_secs(90),
                "timed out waiting for {}: {lines:?}",
                path.display()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn spawn_driver(&self, base_url: &str, extra: &[&str]) -> tokio::process::Child {
        let mut std_command = self.sandbox.kelir_command(base_url);
        std_command
            .env("KELIR_MCP_CONFIG", self.sandbox.cwd.join("mcp.toml"))
            .args(std::iter::once("channel").chain(std::iter::once("--server")))
            .arg("fake")
            .args(extra)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut command: tokio::process::Command = std_command.into();
        self.sandbox.guard.spawn(&mut command).await.unwrap()
    }
}

/// The latent B4 bug: each `run --name` installed the process-wide plan, so a
/// second NEW conversation in one process failed. Two new conversations must
/// each create their own named session and both answer. The two sessions must
/// have DIFFERENT ids (names/ entries), proving the task-local plan id works.
#[tokio::test(flavor = "multi_thread")]
async fn r15_two_new_conversations_get_distinct_sessions_and_both_answer() {
    let before = shells();
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("answer A", 0))),
        Reply::Sse(sse(&text_turn("answer B", 0))),
    ])
    .await;
    let test = ChannelTest::new();
    let mut driver = test
        .spawn_driver(&mock.base_url(), &["--max-messages", "2"])
        .await;
    test.inject(json!({
        "message_id": "m-fix-a", "conversation_id": "c-fix-a",
        "sender": "A", "content": "hello A"
    }))
    .await;
    test.inject(json!({
        "message_id": "m-fix-b", "conversation_id": "c-fix-b",
        "sender": "B", "content": "hello B"
    }))
    .await;
    let replies = test.wait_for(&test.replies, |lines| lines.len() == 2).await;
    assert_eq!(replies.len(), 2, "both NEW conversations must answer");
    assert!(replies.iter().all(|r| r["duplicate"] == false));

    // The two conversations must have bound two distinct named sessions.
    let names_dir = test.sandbox.state.join("names");
    // The session name is slugged + hash-suffixed, so find by conversation substring.
    let find_name = |needle: &str| -> Option<PathBuf> {
        std::fs::read_dir(&names_dir).ok()?.flatten().find_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.contains(needle).then_some(e.path())
        })
    };
    let read_id = |p: &PathBuf| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.get("session").and_then(Value::as_str).map(str::to_string))
    };
    let id_a = find_name("c-fix-a").and_then(|p| read_id(&p));
    let id_b = find_name("c-fix-b").and_then(|p| read_id(&p));
    assert!(
        id_a.is_some(),
        "session name for conversation A must be bound"
    );
    assert!(
        id_b.is_some(),
        "session name for conversation B must be bound"
    );
    assert_ne!(
        id_a, id_b,
        "two NEW conversations must get distinct session ids"
    );
    // No secret from mcp.toml env may appear in the bridge args or events.
    for path in [&test.args_path, &test.events] {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            !text.contains("fix-canary-9e2b"),
            "secret leaked into {path:?}"
        );
    }
    driver.kill().await.ok();
    let _ = driver.wait().await;
    assert!(shells().saturating_sub(before) <= 20, "leaked shells");
}

/// r16: replicate r14's two-conversation concurrency with a held mock, but
/// capture driver stderr on failure. Distinguishes a product serialization
/// bug from a fake-bridge polling/harness timing flake.
#[tokio::test(flavor = "multi_thread")]
async fn r16_two_conversations_parallel_with_held_mock_diagnostic() {
    let before = shells();
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Gated(gate.clone(), sse(&text_turn("first", 0))),
        Reply::Gated(gate.clone(), sse(&text_turn("second", 0))),
    ])
    .await;
    let test = ChannelTest::new();
    let mut driver = test
        .spawn_driver(&mock.base_url(), &["--max-messages", "2"])
        .await;
    test.inject(json!({
        "message_id": "m-fix-c", "conversation_id": "c-fix-c",
        "sender": "C", "content": "hello"
    }))
    .await;
    test.inject(json!({
        "message_id": "m-fix-d", "conversation_id": "c-fix-d",
        "sender": "D", "content": "hello"
    }))
    .await;
    let start = std::time::Instant::now();
    while mock.request_count() < 2 && start.elapsed() < Duration::from_secs(20) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let count = mock.request_count();
    gate.notify_waiters();
    if count < 2 {
        // Drain whatever the bridge/driver wrote before we kill it.
        driver.kill().await.ok();
        let _ = driver.wait().await;
        let events = test.lines(&test.events);
        let replies = test.lines(&test.replies);
        panic!(
            "only {count} provider requests arrived; bridge events={events:?}, replies={replies:?}"
        );
    }
    test.wait_for(&test.replies, |lines| lines.len() == 2).await;
    assert!(mock.max_concurrent() >= 2);
    driver.kill().await.ok();
    let _ = driver.wait().await;
    assert!(shells().saturating_sub(before) <= 20, "leaked shells");
}
