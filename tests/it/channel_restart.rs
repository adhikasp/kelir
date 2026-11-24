//! Fresh-reviewer proving tests for the M10 fix pass. These do NOT modify
//! product code; they drive the real `kelir channel` binary against the
//! scripted mock + fake bridge, with the same TreeGuard/watchdog harness the
//! shipped reviewer tests use.
//!
//! r15 proves the "latent" B4 fix: two NEW conversations in one channel
//! process each get their own named session (task-local plan id) and both
//! answer. It also captures driver stderr on failure to make the r14 flake
//! diagnosable.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use crate::support::*;

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
            "[mcp.servers.fake]\ncommand = '{}'\nargs = ['{}']\nchannel = true\n\
             [mcp.servers.fake.env]\nMCP_CHAN_INBOX = '{}'\nMCP_CHAN_REPLIES = '{}'\n\
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
        let mut full = std::fs::read_to_string(&sandbox.config).unwrap();
        full.push_str(&mcp);
        std::fs::write(&sandbox.config, full).unwrap();
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
    let before = shell_count();
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
    // No secret from config.toml env may appear in the bridge args or events.
    for path in [&test.args_path, &test.events] {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            !text.contains("fix-canary-9e2b"),
            "secret leaked into {path:?}"
        );
    }
    driver.kill().await.ok();
    let _ = driver.wait().await;
    assert!(shell_count().saturating_sub(before) <= 20, "leaked shells");
}

#[test]
fn r17_torn_multibyte_tail_keeps_every_good_byte_and_the_exact_torn_suffix() {
    use kelir::features::channel::{Message, Spool};
    let dir = std::env::temp_dir().join(format!(
        "kelir-m10fix-torn-{}",
        ulid::Ulid::generate().to_string().to_lowercase()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let msg = |id: &str| Message {
        id: id.into(),
        server: "fake".into(),
        conversation: "c".into(),
        sender: "Ada".into(),
        text: "hello".into(),
        reply_to: None,
        attachments: Vec::new(),
    };
    let mut spool = Spool::open(&dir, "fake").unwrap();
    spool.inbound(msg("m1")).unwrap();
    spool.inbound(msg("m2")).unwrap();
    drop(spool);
    let path = dir.join("spool").join("fake.jsonl");
    let original = std::fs::read(&path).unwrap();
    // Append a partial record that starts with a complete ASCII prefix and is
    // cut in the middle of a 4-byte UTF-8 character.
    let torn = b"{\"seq\":3,\"op\":\"inbound\",\"id\":\"m3\",\"message\":{\"text\":\"\xf0\x9f";
    let mut bytes = original.clone();
    bytes.extend_from_slice(torn);
    std::fs::write(&path, &bytes).unwrap();
    let reopened = Spool::open(&dir, "fake").unwrap();
    let ids: Vec<&str> = reopened.replay().iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["m1", "m2"], "both complete records must survive");
    let repaired = std::fs::read(&path).unwrap();
    assert_eq!(
        repaired, original,
        "repaired file must be exactly the good prefix"
    );
    let kept = std::fs::read(path.with_extension("jsonl.torn")).unwrap();
    assert_eq!(
        kept, torn,
        "the exact torn bytes must be preserved for inspection"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// B5 strong probe: a handled `/stop` is marked `done`, so restarting the
/// driver on the same state must NOT send a second "stopped" reply. Also
/// asserts the single reply's text is exactly "stopped".
#[tokio::test(flavor = "multi_thread")]
async fn r18_stop_is_done_and_not_replied_twice_across_restart() {
    let before = shell_count();
    let mock = Mock::start(vec![]).await;
    let test = ChannelTest::new();
    let mut driver = test
        .spawn_driver(&mock.base_url(), &["--max-messages", "1", "--status-once"])
        .await;
    test.inject(json!({
        "message_id": "m-stop-restart", "conversation_id": "c-stop-restart",
        "sender": "Ada", "content": "/stop please"
    }))
    .await;
    test.wait_for(&test.replies, |lines| !lines.is_empty())
        .await;
    let replies = test.lines(&test.replies);
    assert_eq!(replies.len(), 1, "exactly one stopped reply");
    assert_eq!(replies[0]["text"], "stopped");
    driver.kill().await.ok();
    let _ = driver.wait().await;
    let spool = std::fs::read_to_string(test.sandbox.state.join("spool").join("fake.jsonl"))
        .unwrap_or_default();
    assert!(
        spool.contains("\"op\":\"done\""),
        "/stop must be done: {spool}"
    );
    let mut driver2 = test
        .spawn_driver(&mock.base_url(), &["--max-messages", "1"])
        .await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let replies = test.lines(&test.replies);
    assert_eq!(
        replies.len(),
        1,
        "restart must not re-answer a done /stop: {replies:?}"
    );
    driver2.kill().await.ok();
    let _ = driver2.wait().await;
    assert!(shell_count().saturating_sub(before) <= 20, "leaked shells");
}

/// B3 strong probe: a config with both a forbidden capability and a secret in
/// its `env` block must be refused, and the refusal must never echo the secret
/// value from `env` (STEER-7). Applies to case/separator/substring variants.
#[test]
fn r20_config_rejects_forbidden_variants_without_echoing_env_secret() {
    let secret = "super-secret-env-value-4f9a";
    for cap in [
        "claude/channel/permission_relay",
        "Claude/Channel/APPROVAL/relay",
        "x-permission-x",
        "approval_relay",
    ] {
        let text = format!(
            "[mcp.servers.fake]\ncommand = 'python'\ndeclare_capabilities = ['{cap}']\n\
             [mcp.servers.fake.env]\nAPI_TOKEN = '{secret}'\n"
        );
        let err = config_err(&text)
            .err()
            .unwrap_or_else(|| panic!("capability {cap} must be refused"));
        assert!(!err.contains(secret), "error echoed the env secret: {err}");
    }
}
