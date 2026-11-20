//! M10 end-to-end: `kelir channel` against a fake channel MCP server and a
//! scripted mock provider. Real processes, in the test's TreeGuard: a failing
//! test cannot leave a server or a channel driver behind.

mod support;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::*;
use tokio::io::AsyncWriteExt;

const SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/support/channel_server.py"
);

fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

struct ChannelTest {
    sandbox: Sandbox,
    inbox: PathBuf,
    replies: PathBuf,
    events: PathBuf,
}

impl ChannelTest {
    fn new() -> ChannelTest {
        let sandbox = Sandbox::new("");
        let inbox = sandbox.cwd.join("inbox.jsonl");
        let replies = sandbox.cwd.join("replies.jsonl");
        let events = sandbox.cwd.join("channel-events.jsonl");
        for path in [&inbox, &replies, &events] {
            let _ = std::fs::write(path, "");
        }
        let mcp = format!(
            "[servers.fake]\ncommand = '{}'\nargs = ['{}']\nchannel = true\n\
             description = \"fake channel\"\n\
             [servers.fake.env]\nMCP_CHAN_INBOX = '{}'\nMCP_CHAN_REPLIES = '{}'\n\
             MCP_CHAN_EVENTS = '{}'\nMCP_CHAN_SECRETS = 'channel-canary-7f3a'\n",
            python(),
            SCRIPT.replace('\\', "/"),
            inbox.to_string_lossy().replace('\\', "/"),
            replies.to_string_lossy().replace('\\', "/"),
            events.to_string_lossy().replace('\\', "/"),
        );
        std::fs::write(sandbox.cwd.join("mcp.toml"), mcp).unwrap();
        ChannelTest {
            sandbox,
            inbox,
            replies,
            events,
        }
    }

    async fn inject(&self, message: Value) {
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&self.inbox)
            .await
            .unwrap();
        file.write_all(format!("{}\n", message).as_bytes())
            .await
            .unwrap();
        file.flush().await.unwrap();
    }

    fn lines(&self, path: &PathBuf) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    async fn wait_for<F: Fn(&[Value]) -> bool>(&self, path: &PathBuf, ready: F) -> Vec<Value> {
        let started = Instant::now();
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

    /// Spawn `kelir channel`; the guard owns the whole process tree.
    async fn spawn_driver(&self, base_url: &str) -> tokio::process::Child {
        let mut command = self.sandbox.kelir_command(base_url);
        command
            .env("KELIR_MCP_CONFIG", self.sandbox.cwd.join("mcp.toml"))
            .args([
                "channel",
                "--server",
                "fake",
                "--max-messages",
                "1",
                "--status-once",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut command: tokio::process::Command = command.into();
        self.sandbox.guard.spawn(&mut command).await.unwrap()
    }
}

async fn one_answer_and_done_spool() {
    let test = ChannelTest::new();
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("hello from kelir", 0)))]).await;
    let mut child = test.spawn_driver(&mock.base_url()).await;
    test.inject(json!({
        "message_id": "m-1",
        "conversation_id": "chat-42",
        "sender": "Ada",
        "content": "please summarize the lighthouse report"
    }))
    .await;
    let replies = test
        .wait_for(&test.replies, |lines| !lines.is_empty())
        .await;
    assert_eq!(replies.len(), 1, "exactly one reply");
    assert_eq!(replies[0]["id"], "m-1");
    assert_eq!(replies[0]["conversation"], "chat-42");
    assert!(
        replies[0]["text"]
            .as_str()
            .unwrap()
            .contains("hello from kelir")
    );
    assert_eq!(replies[0]["duplicate"], false);
    // Capability negotiation declares the channel but never an approval relay.
    let events = test
        .wait_for(&test.events, |lines| {
            lines.iter().any(|line| line["kind"] == "initialize")
        })
        .await;
    let caps = &events
        .iter()
        .find(|line| line["kind"] == "initialize")
        .unwrap()["capabilities"];
    assert!(caps.get("claude/channel").is_some());
    assert!(caps.get("claude/channel/permission").is_none());
    assert!(caps.get("claude/channel/permissions").is_none());
    // The bridge is told the turn is over.
    let _ = test
        .wait_for(&test.events, |lines| {
            lines.iter().any(|line| line["kind"] == "turn_done")
        })
        .await;
    // The user event is marked as coming from a remote chat user.
    let state = &test.sandbox.state;
    let sessions = kelir::paths::sessions_newest_first_in(&state.join("sessions"));
    assert!(!sessions.is_empty(), "a named session was created");
    let log = std::fs::read_to_string(
        state
            .join("sessions")
            .join(&sessions[0])
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(log.contains("\"from\":\"channel\""), "{log}");
    assert!(log.contains("remote chat user"), "{log}");
    assert!(
        !log.contains("channel-canary-7f3a"),
        "no secret canary in the log"
    );
    let spool_path = state.join("spool").join("fake.jsonl");
    let spool = std::fs::read_to_string(&spool_path).unwrap_or_else(|e| {
        panic!(
            "no spool at {}: {e}; state has {:?}",
            spool_path.display(),
            std::fs::read_dir(state)
                .map(|r| r
                    .filter_map(|x| x.ok().map(|y| y.file_name()))
                    .collect::<Vec<_>>())
                .unwrap_or_default()
        );
    });
    assert!(spool.contains("\"op\":\"done\""), "{spool}");
    assert!(
        !spool.contains("channel-canary-7f3a"),
        "no canary in the spool"
    );
    let _ = child.kill().await;
    let _ = child.wait().await;
}

async fn one_fallback_on_failure() {
    let test = ChannelTest::new();
    let mock = Mock::start(vec![Reply::Http(500, "provider down".into())]).await;
    let mut child = test.spawn_driver(&mock.base_url()).await;
    test.inject(json!({
        "message_id": "m-2",
        "conversation_id": "chat-43",
        "sender": "Grace",
        "content": "hello"
    }))
    .await;
    let replies = test
        .wait_for(&test.replies, |lines| !lines.is_empty())
        .await;
    assert_eq!(replies.len(), 1, "exactly one fallback reply");
    assert!(
        replies[0]["text"].as_str().unwrap().contains("Sorry"),
        "{:?}",
        replies[0]
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(test.lines(&test.replies).len(), 1, "never a second reply");
    let _ = child.kill().await;
    let _ = child.wait().await;
}

async fn dedupe_replayed_id() {
    let test = ChannelTest::new();
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("once", 0)))]).await;
    let mut child = test.spawn_driver(&mock.base_url()).await;
    let message = json!({
        "message_id": "m-3",
        "conversation_id": "chat-44",
        "sender": "Lin",
        "content": "hi"
    });
    test.inject(message.clone()).await;
    test.inject(message).await;
    let replies = test
        .wait_for(&test.replies, |lines| !lines.is_empty())
        .await;
    assert_eq!(replies.len(), 1, "two inbound copies, one reply");
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// The process-count watchdog from AGENTS.md: count shells before and after
/// the drive, abort if a test leaked more than 20. Counting an image name is
/// allowed; killing one is not — the TreeGuard owns our processes by pid.
fn shell_count() -> usize {
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
                .filter(|line| line.to_ascii_lowercase().contains("sh.exe") || line.contains(" sh "))
                .count()
        })
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread")]
async fn channel_e2e_pyramid() {
    let shells_before = shell_count();
    // One test drives all three cases sequentially: the real-process harness
    // holds a machine-wide spawn budget, and three concurrent drivers make the
    // cases contend for it (then all time out on a busy desktop).
    one_answer_and_done_spool().await;
    one_fallback_on_failure().await;
    dedupe_replayed_id().await;
    let leaked = shell_count().saturating_sub(shells_before);
    assert!(leaked <= 20, "the channel tests leaked {leaked} shells");
}
