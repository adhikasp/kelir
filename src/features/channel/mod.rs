//! Channel mode: chat bridges through MCP, a durable spool, fallback replies.
//! A *channel server* is an MCP server with `channel = true`.
//! It sends inbound chat messages as `notifications/claude/channel`; kelir answers on the same
//! server with a configurable reply tool and tells it when a turn ends.
//! This module is the generic mechanism — a concrete bridge (Telegram or anything else) is

#[cfg(test)]
use crate::features::mcp::rpc::Notification;
#[cfg(test)]
use serde_json::{Value, json};
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::time::{Duration, Instant};

pub mod driver;
pub mod message;
pub mod run;

pub use driver::*;
pub use message::*;
pub use run::*;

#[cfg(test)]
mod state_tests {
    use super::*;

    fn message(conversation: &str, id: &str) -> Message {
        Message {
            id: id.into(),
            server: "demo".into(),
            conversation: conversation.into(),
            sender: "Ada".into(),
            text: "hello".into(),
            reply_to: None,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn rate_limits_are_per_conversation_and_global() {
        let mut limits = RateLimits::new(2, 3, Duration::from_secs(60));
        assert!(limits.allow("a"));
        assert!(limits.allow("a"));
        // c1 is at its per-conversation cap; c2 still has one global token.
        assert!(!limits.allow("a"));
        assert!(limits.allow("b"));
        assert!(!limits.allow("c"));
        assert_eq!(limits.remaining(), 0);
        // A new window frees everything.
        limits.started = Instant::now() - Duration::from_secs(61);
        assert!(limits.allow("a"));
    }

    #[test]
    fn a_second_message_waits_and_two_conversations_run_up_to_the_cap() {
        let mut dispatch = Dispatcher::new(2);
        assert!(dispatch.push(message("a", "1")));
        assert!(!dispatch.push(message("a", "2")), "single flight");
        assert!(dispatch.push(message("b", "3")));
        let first = dispatch.take_next().unwrap();
        assert_eq!(first.id, "1");
        let second = dispatch.take_next().unwrap();
        assert_eq!(second.id, "3");
        assert!(dispatch.take_next().is_none(), "max_active reached");
        assert_eq!(dispatch.queued(), 1);
        dispatch.finish("a");
        let third = dispatch.take_next().unwrap();
        assert_eq!(third.id, "2");
        assert_eq!(dispatch.queued(), 0);
    }

    #[test]
    fn the_dispatcher_keeps_one_slot_per_running_message() {
        let mut dispatch = Dispatcher::new(1);
        for n in 0..1000 {
            dispatch.push(message(&format!("c{n}"), &format!("m{n}")));
        }
        assert_eq!(dispatch.queued(), 1000);
        for _ in 0..1000 {
            let m = dispatch.take_next().unwrap();
            assert_eq!(dispatch.active(), 1);
            dispatch.finish(&m.conversation);
            assert_eq!(dispatch.active(), 0);
        }
        assert!(dispatch.take_next().is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kelir-channel-{name}-{}",
            ulid::Ulid::generate().to_string().to_lowercase()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn message(id: &str) -> Message {
        Message {
            id: id.into(),
            server: "demo".into(),
            conversation: "chat-1".into(),
            sender: "Ada".into(),
            text: format!("hello {id}"),
            reply_to: None,
            attachments: Vec::new(),
        }
    }

    fn notification(method: &str, params: Value) -> Notification {
        Notification {
            server: "demo".into(),
            method: method.into(),
            params,
        }
    }

    #[test]
    fn a_notification_parses_the_bridge_shape_and_the_generic_shape() {
        let note = notification(
            INBOUND_METHOD,
            json!({
                "message_id": "m1", "conversation_id": "c1", "sender": "Ada",
                "content": "hi", "reply_to": "m0",
                "attachments": [{"path": "a.png"}], "extra": "ignored"
            }),
        );
        let message = parse_notification(&note).unwrap();
        assert_eq!(message.id, "m1");
        assert_eq!(message.conversation, "c1");
        assert_eq!(message.sender, "Ada");
        assert_eq!(message.text, "hi");
        assert_eq!(message.reply_to.as_deref(), Some("m0"));
        assert_eq!(message.attachments.len(), 1);

        let note = notification(
            INBOUND_METHOD,
            json!({"id": "m2", "conversation": "c2", "text": "yo", "meta": {"chat": "c2", "sender": "Bob"}}),
        );
        let message = parse_notification(&note).unwrap();
        assert_eq!(message.id, "m2");
        assert_eq!(message.conversation, "c2");
        assert_eq!(message.sender, "Bob");
        assert_eq!(message.text, "yo");
    }

    #[test]
    fn the_envelope_marks_remote_text_untrusted() {
        // A bridge message is data, never an operator command.
        let mut note = message("m1");
        note.server = "demo".into();
        note.sender = "Ada".into();
        let text = note.envelope();
        assert!(text.contains("server=demo") && text.contains("conversation=chat-1"));
        assert!(text.contains("sender=Ada") && text.contains("remote chat user"));
        assert!(text.contains("not the operator") && text.contains("hello m1"));
        assert!(text.contains("untrusted data"), "{text}");
    }

    #[test]
    fn malformed_oversized_and_unknown_notifications_are_dropped() {
        assert_eq!(
            parse_notification(&notification("notifications/progress", json!({}))),
            Err(Drop::Other)
        );
        for params in [
            json!({"message_id": "m", "content": "hi"}), // no conversation
            json!({"message_id": "m", "conversation_id": "c", "content": ""}), // empty text
            json!({"message_id": "m", "conversation_id": "c", "content": "x".repeat(MAX_TEXT + 1)}),
            json!({"message_id": "x".repeat(129), "conversation_id": "c", "content": "hi"}),
            json!({"message_id": "m", "conversation_id": "c", "content": "hi", "attachments": [{"data": "x".repeat(17 * 1024 * 1024)}]}),
            json!([]),
        ] {
            assert!(matches!(
                parse_notification(&notification(INBOUND_METHOD, params)),
                Err(Drop::Malformed(_))
            ));
        }
    }

    #[test]
    fn spool_append_claim_done_and_replay_are_idempotent_by_id() {
        let dir = temp("spool");
        let mut spool = Spool::open(&dir, "demo").unwrap();
        assert!(spool.replay().is_empty());
        assert!(spool.inbound(message("m1")).unwrap());
        assert!(!spool.inbound(message("m1")).unwrap(), "dedupe by id");
        spool.claim("m1").unwrap();
        assert_eq!(spool.replay().len(), 1);
        spool.done("m1").unwrap();
        spool.done("m1").unwrap(); // idempotent
        assert!(spool.replay().is_empty());
        assert!(spool.is_done("m1"));
        let mut again = Spool::open(&dir, "demo").unwrap();
        assert!(again.replay().is_empty(), "done is durable");
        assert!(!again.inbound(message("m1")).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unfinished_message_is_replayed_after_a_crash() {
        let dir = temp("replay");
        {
            let mut spool = Spool::open(&dir, "demo").unwrap();
            spool.inbound(message("m1")).unwrap();
            spool.claim("m1").unwrap();
            spool.inbound(message("m2")).unwrap();
        }
        let spool = Spool::open(&dir, "demo").unwrap();
        let ids: Vec<&str> = spool.replay().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["m1", "m2"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_torn_tail_is_repaired_and_earlier_messages_survive() {
        let dir = temp("torn");
        let mut spool = Spool::open(&dir, "demo").unwrap();
        spool.inbound(message("m1")).unwrap();
        let path = dir.join("spool").join("demo.jsonl");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{\"seq\":2,\"op\":\"inbo");
        std::fs::write(&path, bytes).unwrap();
        let spool = Spool::open(&dir, "demo").unwrap();
        assert_eq!(spool.replay().len(), 1);
        assert!(path.with_extension("jsonl.torn").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compaction_bounds_the_file_and_keeps_unfinished_messages() {
        let dir = temp("compact");
        let mut spool = Spool::open(&dir, "demo").unwrap();
        for n in 0..40 {
            spool.inbound(message(&format!("m{n}"))).unwrap();
            spool.done(&format!("m{n}")).unwrap();
        }
        spool.inbound(message("live")).unwrap();
        let before = spool.bytes();
        assert!(spool.compact(256, "now").unwrap());
        assert!(spool.bytes() < before);
        let again = Spool::open(&dir, "demo").unwrap();
        let ids: Vec<&str> = again.replay().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["live"], "unfinished messages survive compaction");
        // A further append keeps monotonic seq after the rewrite.
        let mut again = again;
        again.inbound(message("next")).unwrap();
        let again = Spool::open(&dir, "demo").unwrap();
        assert_eq!(again.replay().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_names_are_safe_stable_and_length_bounded() {
        let a = message("m1").session_name();
        let b = message("m1").session_name();
        assert_eq!(a, b);
        crate::features::names::validate(&a).unwrap();
        let odd = Message {
            conversation: "Topic / with spaces!".into(),
            ..message("m2")
        }
        .session_name();
        crate::features::names::validate(&odd).unwrap();
        let long = Message {
            conversation: "x".repeat(100),
            ..message("m3")
        }
        .session_name();
        assert!(long.len() <= crate::features::names::MAX_LEN);
        crate::features::names::validate(&long).unwrap();
        assert_ne!(
            message("m").session_name(),
            Message {
                server: "other".into(),
                ..message("m")
            }
            .session_name(),
            "different servers must not collide"
        );
        // Characters the slug drops must not collapse onto one name either.
        assert_ne!(
            Message {
                conversation: "Topic / x".into(),
                ..message("m")
            }
            .session_name(),
            Message {
                conversation: "Topic - x".into(),
                ..message("m")
            }
            .session_name(),
        );
    }

    #[test]
    fn a_torn_tail_states_survive_and_a_compacted_id_stays_deduped_across_restarts() {
        let dir = temp("states");
        let mut spool = Spool::open(&dir, "demo").unwrap();
        spool.inbound(message("a")).unwrap(); // killed after the spool write
        spool.inbound(message("b")).unwrap();
        spool.claim("b").unwrap(); // killed mid-turn
        spool.inbound(message("c")).unwrap();
        spool.claim("c").unwrap();
        spool.done("c").unwrap(); // killed after the reply and done
        drop(spool);
        let spool = Spool::open(&dir, "demo").unwrap();
        let ids: Vec<&str> = spool.replay().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert!(spool.is_done("c"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
