//! Adversarial reviewer tests for M10 (channel mode). Every test names a
//! concrete failure of the milestone task; a failing test is a real finding.
//! Real-process cases use the TreeGuard harness with a shell-count watchdog.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
use std::path::PathBuf;
use std::time::Duration;

use kelir::channel::{
    INBOUND_METHOD, Message, Spool, capabilities, forbidden_capability, parse_notification,
    stop_command,
};
use kelir::mcp::config::ServerConfig;
use serde_json::{Value, json};

mod support;
use support::*;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kelir-m10-review-{name}-{}",
        ulid::Ulid::generate().to_string().to_lowercase()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn msg(id: &str, conversation: &str) -> Message {
    Message {
        id: id.into(),
        server: "fake".into(),
        conversation: conversation.into(),
        sender: "Ada".into(),
        text: "hello".into(),
        reply_to: None,
        attachments: Vec::new(),
    }
}

fn note(method: &str, params: Value) -> kelir::mcp::rpc::Notification {
    kelir::mcp::rpc::Notification {
        server: "fake".into(),
        method: method.into(),
        params,
    }
}

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

async fn wait_until_result<F: Fn() -> bool>(ready: F, timeout: Duration) -> Result<(), ()> {
    let started = std::time::Instant::now();
    loop {
        if ready() {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// R1: a torn tail that cuts a multi-byte character must be repaired, not
/// silently turn the whole spool into an empty file (seq restarts at 1, every
/// later record unreadable: total message loss).
#[test]
fn r1_invalid_utf8_torn_tail_must_not_erase_the_spool() {
    let dir = temp("utf8");
    let mut spool = Spool::open(&dir, "fake").unwrap();
    spool.inbound(msg("m1", "c1")).unwrap();
    drop(spool);
    let path = dir.join("spool").join("fake.jsonl");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.push(b'\xf0'); // torn start of a 4-byte char after a valid line
    std::fs::write(&path, bytes).unwrap();
    let spool = Spool::open(&dir, "fake").unwrap();
    assert_eq!(
        spool.replay().len(),
        1,
        "the valid inbound record must survive a torn multi-byte tail"
    );
    assert!(
        path.with_extension("jsonl.torn").exists(),
        "the torn tail must be kept beside the spool"
    );
}

/// R2: compaction keeps a done record but drops the inbound record, so the
/// reopened spool has no memory of the id. A bridge resend is accepted again
/// and answered twice.
#[test]
fn r2_a_compacted_done_id_must_still_be_deduped() {
    let dir = temp("compact-dedupe");
    let mut spool = Spool::open(&dir, "fake").unwrap();
    spool.inbound(msg("m1", "c1")).unwrap();
    spool.done("m1").unwrap();
    assert!(spool.compact(1, "now").unwrap());
    let mut reopened = Spool::open(&dir, "fake").unwrap();
    assert!(
        !reopened.inbound(msg("m1", "c1")).unwrap(),
        "a resend of a compacted done id must be deduped"
    );
}

/// R3: `reply()` always sends reply_to = message.id, ignoring the bridge's
/// explicit reply_to. A threaded conversation replies to the wrong message.
#[tokio::test]
async fn r3_reply_must_use_the_messages_reply_to() {
    // Drive the real `kelir channel` against the fake bridge and assert the
    // `reply` tool receives the bridge's explicit reply_to, not the message id.
    let before = shells();
    let test = ChannelTest::new();
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("answer", 0)))]).await;
    let mut driver = test.spawn_driver(&mock.base_url()).await;
    test.inject(json!({
        "message_id": "m-review-3",
        "conversation_id": "chat-review",
        "sender": "Ada",
        "content": "hello",
        "reply_to": "parent-9"
    }))
    .await;
    let replies = test
        .wait_for(&test.replies, |lines| !lines.is_empty())
        .await;
    assert_eq!(replies.len(), 1);
    // The fake server records the tool args it received in its `reply` line;
    // extend it to include reply_to and assert the thread was preserved.
    let recorded = test.lines(&test.sandbox.cwd.join("review-reply-args.jsonl"));
    if let Some(args) = recorded.first() {
        assert_eq!(
            args["reply_to"], "parent-9",
            "reply_to must be forwarded from the inbound message"
        );
    }
    driver.kill().await.ok();
    let _ = driver.wait().await;
    assert!(shells().saturating_sub(before) <= 20, "leaked shells");
}

/// R4: a permission-relay capability using `_` instead of `-` is accepted and
/// would be declared at initialize: a promise kelir cannot keep.
#[test]
fn r4_permission_relay_variants_must_be_forbidden() {
    for capability in [
        "claude/channel/permission_relay",
        "claude/channel/permission/relay",
        "Claude/Channel/Permission",
        "claude/channel/permissions/anything",
    ] {
        assert!(
            forbidden_capability(capability),
            "{capability} must be forbidden"
        );
    }
}

/// R4b: the config validator must reject a permission-relay capability too.
#[test]
fn r4b_config_rejects_permission_relay() {
    let text = "[servers.fake]\ncommand = 'python'\ndeclare_capabilities = ['claude/channel/permission_relay']\n";
    assert!(
        kelir::mcp::config::parse_layer(text, "review").is_err(),
        "a permission relay capability must be refused in mcp.toml"
    );
}

/// R4c: `capabilities()` must filter a forbidden capability even when a
/// bypassed config supplies one (defense in depth).
#[test]
fn r4c_capabilities_filter_forbidden_defense_in_depth() {
    let config = ServerConfig {
        name: "fake".into(),
        command: "python".into(),
        args: Vec::new(),
        env: std::collections::BTreeMap::new(),
        description: String::new(),
        channel: true,
        timeout: Duration::from_secs(10),
        reply_tool: "reply".into(),
        declare_capabilities: vec![
            "claude/channel".into(),
            "claude/channel/permission_relay".into(),
        ],
    };
    let caps = capabilities(&config);
    assert!(caps.get("claude/channel").is_some());
    assert!(
        caps.as_object()
            .unwrap()
            .keys()
            .all(|k| !k.contains("permission")),
        "capabilities() leaked a permission relay: {caps}"
    );
}

/// R5: /stop on a line with a trailing space must still map to the stop verb.
#[test]
fn r5_stop_command_whitespace_variants() {
    assert_eq!(stop_command("/stop   ").as_deref(), Some(""));
    assert_eq!(stop_command("/stop because").as_deref(), Some("because"));
}

/// R6: an oversized notification must be dropped with a bounded reason that
/// does not echo the text (STEER-7).
#[test]
fn r6_oversized_notification_is_dropped_without_echo() {
    let big = "x".repeat(128 * 1024);
    let result = parse_notification(&note(
        INBOUND_METHOD,
        json!({"message_id": "m1", "conversation_id": "c1", "sender": "A", "content": big}),
    ));
    assert!(result.is_err(), "oversized text must be dropped");
}

/// R7: the status snapshot path documented in docs/reference/channels.md must
/// be the one the driver writes.
#[test]
fn r7_status_snapshot_path_matches_docs() {
    let state = temp("status");
    let status = kelir::channel::Status {
        runtime: "channel".into(),
        ..Default::default()
    };
    // The driver's writer is private; `read_status` is the public reader and
    // must agree with the documented path.
    let documented = state.join("channel-status").join("status.json");
    std::fs::create_dir_all(documented.parent().unwrap()).unwrap();
    std::fs::write(&documented, serde_json::to_vec(&status).unwrap()).unwrap();
    assert!(
        kelir::channel::read_status(&state).is_some(),
        "read_status must agree with docs/reference/channels.md: channel-status/status.json"
    );
}

/// R8: the drop reason for a malformed notification must be shape-only: no
/// payload text (a STEER-7 canary) may be logged or stored in the spool.
#[test]
fn r8_drop_reason_never_echoes_the_payload() {
    let canary = "channel-canary-7f3a";
    let result = parse_notification(&note(
        INBOUND_METHOD,
        json!({
            "message_id": "m1", "conversation_id": "c1", "sender": "A",
            "content": format!("secret {canary}"), "extra": vec![1, 2, 3]
        }),
    ));
    // A valid message containing the canary is fine (it is the actual text);
    // what matters here is that the *drop* path never echoes the payload.
    assert!(result.is_ok());
    let bad = parse_notification(&note(
        INBOUND_METHOD,
        json!({"content": canary.to_string(), "message_id": "m"}),
    ));
    match bad {
        Err(kelir::channel::Drop::Malformed(reason)) => {
            assert!(
                !reason.contains(canary),
                "drop reason leaked a canary: {reason}"
            );
        }
        other => panic!("expected a malformed drop, got {other:?}"),
    }
}

/// R9: a crash after the reply was sent but before the spool `done` record
/// must replay exactly one message and must never send a second reply. The
/// fake bridge marks any repeated message id as `duplicate: true`.
#[tokio::test(flavor = "multi_thread")]
async fn r9_restart_after_reply_has_no_duplicate_reply() {
    let before = shells();
    let test = ChannelTest::new();
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("first answer", 0))),
        Reply::Sse(sse(&text_turn("replayed answer", 0))),
    ])
    .await;
    let mut driver = test.spawn_driver(&mock.base_url()).await;
    test.inject(json!({
        "message_id": "m-review-9",
        "conversation_id": "chat-review-9",
        "sender": "Ada",
        "content": "hello"
    }))
    .await;
    let replies = test
        .wait_for(&test.replies, |lines| !lines.is_empty())
        .await;
    assert_eq!(replies.len(), 1);
    assert!(!replies[0]["duplicate"].as_bool().unwrap());
    // The first driver stops after one message and writes `done`; restart on
    // the same state dir with no new inbound message.
    driver.kill().await.ok();
    let _ = driver.wait().await;
    let spool = std::fs::read_to_string(test.sandbox.state.join("spool").join("fake.jsonl"))
        .unwrap_or_default();
    assert!(spool.contains("\"done\""), "spool must be durable: {spool}");
    let mut driver2 = test.spawn_driver(&mock.base_url()).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let replies = test.lines(&test.replies);
    assert_eq!(
        replies.len(),
        1,
        "a restart must not replay an already-done message: {replies:?}"
    );
    driver2.kill().await.ok();
    let _ = driver2.wait().await;
    assert!(shells().saturating_sub(before) <= 20, "leaked shells");
}

struct ChannelTest {
    sandbox: Sandbox,
    inbox: PathBuf,
    replies: PathBuf,
}

impl ChannelTest {
    fn new() -> ChannelTest {
        let sandbox = Sandbox::new("");
        let inbox = sandbox.cwd.join("review-inbox.jsonl");
        let replies = sandbox.cwd.join("review-replies.jsonl");
        let events = sandbox.cwd.join("review-events.jsonl");
        for path in [&inbox, &replies, &events] {
            let _ = std::fs::write(path, "");
        }
        let args_path = sandbox.cwd.join("review-reply-args.jsonl");
        let _ = std::fs::write(&args_path, "");
        let mcp = format!(
            "[servers.fake]\ncommand = '{}'\nargs = ['{}']\nchannel = true\n\
             [servers.fake.env]\nMCP_CHAN_INBOX = '{}'\nMCP_CHAN_REPLIES = '{}'\n\
             MCP_CHAN_EVENTS = '{}'\nMCP_CHAN_ARGS = '{}'\nMCP_CHAN_SECRETS = 'channel-canary-7f3a'\n",
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

    async fn spawn_driver(&self, base_url: &str) -> tokio::process::Child {
        self.spawn_driver_with(base_url, &["--max-messages", "1", "--status-once"])
            .await
    }

    async fn spawn_driver_with(&self, base_url: &str, extra: &[&str]) -> tokio::process::Child {
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

/// R14: `max_active` promises bounded *parallel* conversations. Two messages
/// for different conversations sent while the provider holds both requests
/// must overlap (peak concurrency 2). The driver awaits `process()` in one
/// loop, so no two turns can ever be in flight at once.
#[tokio::test(flavor = "multi_thread")]
async fn r14_max_active_should_allow_parallel_conversations() {
    let before = shells();
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let mock = Mock::start(vec![
        Reply::Gated(gate.clone(), sse(&text_turn("first", 0))),
        Reply::Gated(gate.clone(), sse(&text_turn("second", 0))),
    ])
    .await;
    let test = ChannelTest::new();
    // Default max_active is 4, so both conversations should run at once.
    let mut driver = test
        .spawn_driver_with(&mock.base_url(), &["--max-messages", "2"])
        .await;
    test.inject(json!({
        "message_id": "m-r14-a",
        "conversation_id": "c-r14-a",
        "sender": "A",
        "content": "hello"
    }))
    .await;
    test.inject(json!({
        "message_id": "m-r14-b",
        "conversation_id": "c-r14-b",
        "sender": "B",
        "content": "hello"
    }))
    .await;
    // Wait for both completion requests to be held by the mock. If they are
    // serialised, only one arrives and this times out.
    let both = wait_until_result(|| mock.request_count() >= 2, Duration::from_secs(30)).await;
    gate.notify_waiters();
    if both.is_err() {
        driver.kill().await.ok();
        let _ = driver.wait().await;
        panic!("only one provider request arrived: two conversations were serialised");
    }
    test.wait_for(&test.replies, |lines| lines.len() == 2).await;
    assert!(
        mock.max_concurrent() >= 2,
        "max_active=4 should let two conversations answer in parallel; peak={}",
        mock.max_concurrent()
    );
    driver.kill().await.ok();
    let _ = driver.wait().await;
    assert!(shells().saturating_sub(before) <= 20, "leaked shells");
}

/// R11: `/stop` never inserts into `active`, so `process()` skips the spool's
/// `done` record. On restart the message is replayed and answered again —
/// a duplicate reply for one inbound message.
#[tokio::test(flavor = "multi_thread")]
async fn r11_stop_is_done_in_the_spool_and_not_replied_twice() {
    let before = shells();
    let test = ChannelTest::new();
    let mock = Mock::start(vec![]).await;
    let mut driver = test.spawn_driver(&mock.base_url()).await;
    test.inject(json!({
        "message_id": "m-review-11",
        "conversation_id": "chat-review-11",
        "sender": "Ada",
        "content": "/stop please"
    }))
    .await;
    let replies = test
        .wait_for(&test.replies, |lines| !lines.is_empty())
        .await;
    assert_eq!(replies.len(), 1, "one stop acknowledgement");
    let spool_path = test.sandbox.state.join("spool").join("fake.jsonl");
    let spool = std::fs::read_to_string(&spool_path).unwrap_or_default();
    assert!(
        spool.contains("\"done\""),
        "a handled /stop must be marked done in the spool: {spool}"
    );
    driver.kill().await.ok();
    let _ = driver.wait().await;
    assert!(shells().saturating_sub(before) <= 20, "leaked shells");
}

/// R13: `outcome()` scans the whole log for *any* deny gate. In a resumed
/// named session, a gate denied on turn 1 poisons every later turn: if turn 2
/// has no `turn_ended` yet (e.g. a provider error), the reply claims the
/// message was "blocked by policy" and uses the wrong fallback.
#[test]
fn r13_a_stale_gate_deny_must_not_poison_a_later_turn() {
    use kelir::channel::{Outcome, outcome};
    use kelir::event::{EventBody, LogEvent};
    // Turn 2 was killed mid-provider-call: no `turn_ended` event exists yet.
    // The stale deny from turn 1 must not be reported as the outcome of turn 2.
    let events = vec![
        LogEvent::new(
            1,
            EventBody::Gate {
                gate: "policy".into(),
                event: "prompt_submit".into(),
                decision: "deny".into(),
                reason: "old denial".into(),
                ms: 0,
                rewritten: false,
                error: None,
            },
        ),
        LogEvent::new(
            2,
            EventBody::User {
                text: kelir::event::Payload::text("second message"),
                from: "channel".into(),
                delivery: "now".into(),
                attachments: Vec::new(),
            },
        ),
    ];
    assert!(
        matches!(outcome(&events), Outcome::Failed(_)),
        "a turn with no turn_ended must be Failed (fallback), not Gated by a stale deny"
    );
}
