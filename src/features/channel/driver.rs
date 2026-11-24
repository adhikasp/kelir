//! Channel mode: chat bridges through MCP, a durable spool, fallback replies.
//! A *channel server* is an MCP server with `channel = true`.
//! It sends inbound chat messages as `notifications/claude/channel`; kelir answers on the same
//! server with a configurable reply tool and tells it when a turn ends.
//! This module is the generic mechanism — a concrete bridge (Telegram or anything else) is

use std::path::Path;

use serde_json::{Value, json};

use crate::features::mcp::broker::Broker;

use super::*;

// ------------------------------------------------------------------ driver

/// The result of one channel turn, read from its session log.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Done(String),
    /// The turn failed, paused or timed out: the fallback reply applies.
    Failed(String),
    /// A gate denied the prompt: no answer is sent to the remote user.
    Gated(String),
}

/// Read a session's newest `turn_ended` and the final assistant text.
pub fn outcome(events: &[crate::log::event::LogEvent]) -> Outcome {
    if let Some(turn) = events
        .iter()
        .rev()
        .find(|event| matches!(event.body, crate::log::event::EventBody::TurnEnded { .. }))
        && let crate::log::event::EventBody::TurnEnded {
            outcome,
            final_text,
            ..
        } = &turn.body
    {
        let text = final_text.inline_str().unwrap_or("").to_string();
        return match outcome.as_str() {
            "done" => Outcome::Done(text),
            other => Outcome::Failed(format!("turn {other}: {text}")),
        };
    }
    // No `turn_ended`: a gate deny counts only when no `user` event follows it.
    for event in events.iter().rev() {
        match &event.body {
            crate::log::event::EventBody::User { .. } => break,
            crate::log::event::EventBody::Gate {
                decision, reason, ..
            } if decision == "deny" => return Outcome::Gated(reason.clone()),
            _ => {}
        }
    }
    Outcome::Failed("turn did not finish".into())
}

/// Rewrite `/stop` in a channel message to the `stop` verb, for the envelope
/// `text`. The log keeps the message the user actually sent.
pub fn stop_command(text: &str) -> Option<String> {
    let first = text.lines().next().unwrap_or("").trim();
    if first == STOP_COMMAND || first.starts_with(&format!("{STOP_COMMAND} ")) {
        Some(first[STOP_COMMAND.len()..].trim().to_string())
    } else {
        None
    }
}

/// Decode the base64 `data` attachment (no new dependency: attachments has
/// the encoder, the decoder is its exact inverse).
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) || bytes.len() > 16 * 1024 * 1024 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.as_chunks::<4>().0 {
        let mut word = 0u32;
        for (index, byte) in chunk.iter().enumerate() {
            let value = if *byte == b'=' && index >= 2 {
                0
            } else {
                ALPHABET.iter().position(|letter| letter == byte)? as u32
            };
            word |= value << (18 - index * 6);
        }
        out.push((word >> 16) as u8);
        if chunk[2] != b'=' {
            out.push((word >> 8) as u8);
        }
        if chunk[3] != b'=' {
            out.push(word as u8);
        }
    }
    Some(out)
}

/// Save attachments named by a message into its session directory, returning the pointers to put in the
/// envelope.
pub fn save_attachments(message: &Message, session_dir: &Path) -> Vec<String> {
    let mut pointers = Vec::new();
    for (index, item) in message.attachments.iter().enumerate() {
        let path = item.get("path").and_then(Value::as_str);
        let data = item.get("data").and_then(Value::as_str);
        let mime = item
            .get("mimeType")
            .or_else(|| item.get("mime"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let extension = if mime.contains("png") {
            "png"
        } else if mime.contains("jpeg") || mime.contains("jpg") {
            "jpg"
        } else if mime.contains("gif") {
            "gif"
        } else if mime.contains("webp") {
            "webp"
        } else {
            "bin"
        };
        let base = format!("channel-{}-{index}.{extension}", message.id);
        let target = session_dir.join("attachments").join(&base);
        let written = if let Some(data) = data {
            decode_base64(data).and_then(|bytes| std::fs::write(&target, bytes).ok())
        } else {
            path.and_then(|path| {
                let source = Path::new(path);
                if source.is_file() {
                    std::fs::copy(source, &target).ok().map(|_| ())
                } else {
                    None
                }
            })
        };
        if written.is_some() {
            let bytes = std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
            pointers.push(crate::features::attachments::pointer(
                &format!("attachments/{base}"),
                crate::features::attachments::dimensions(
                    &std::fs::read(&target).unwrap_or_default(),
                ),
                bytes,
            ));
        }
    }
    pointers
}

pub async fn reply(
    broker: &Broker,
    server: &str,
    tool: &str,
    message: &Message,
    text: &str,
) -> Result<(), String> {
    let arguments = json!({
        "conversation_id": message.conversation,
        "text": text,
        "message_id": message.id,
        "reply_to": message.reply_to.as_deref().unwrap_or(&message.id),
    });
    let output = broker
        .call(
            server,
            tool,
            arguments,
            false,
            &format!("chan_{}", message.id),
        )
        .await;
    if output.outcome == "ok" {
        Ok(())
    } else {
        Err(format!("channel reply failed: {}", output.stderr.trim()))
    }
}

/// Tell the bridge a turn ended (so a typing indicator stops). Failure is not
/// a turn failure: the answer itself is already on its way.
pub async fn turn_done(broker: &Broker, server: &str, conversation: &str) {
    let Ok((client, _)) = broker.ensure(server).await else {
        return;
    };
    let _ = client
        .notify(TURN_DONE_METHOD, json!({"conversation_id": conversation}))
        .await;
}
