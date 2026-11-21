//! tui2 end-to-end: the image flow (`kelir view`) and the markdown/prefix
//! promises it rests on.
//!
//! The real binary drives the scripted mock; assertions are on the request
//! bodies the mock received (the cached prefix must stay byte-identical) and on
//! the event log (the image is a tail `source`, never an edit).

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
mod support;

use serde_json::Value;
use support::*;

/// A minimal PNG (signature + IHDR for 1280x720), enough for the pointer and
/// the mime type.
fn png() -> Vec<u8> {
    let mut bytes: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&1280u32.to_be_bytes());
    bytes.extend_from_slice(&720u32.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes
}

fn messages_of(request: &support::Recorded) -> Vec<Value> {
    request.messages().to_vec()
}

/// The whole point of the pointer design: the model asks with `kelir view`, and
/// the image arrives as one part of the *last* message while every earlier
/// message stays byte-identical (so the provider's cached prefix still hits).
#[tokio::test]
async fn kelir_view_queues_an_image_at_the_tail_and_keeps_the_prefix_byte_identical() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let image = cwd.join("shot.png");
    std::fs::write(&image, png()).unwrap();
    let mock = Mock::start(vec![
        // Step 1: the model looks at the path the user pointed at.
        Reply::Sse(sse(&tool_turn(
            "call_view",
            "bash",
            // `$KELIR_BIN` is the binary the session exports to every bash child
            // (kelir is not on PATH inside a test sandbox).
            &serde_json::json!({"command": format!("\"$KELIR_BIN\" view \"{}\"", image.display())}),
        ))),
        // Step 2: the answer, after the image arrived.
        Reply::Sse(sse(&text_turn("a white circle on a dark background", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "look"], "").await;
    session.wait_for("turn_ended").await;

    let requests = mock.requests();
    let chat: Vec<&support::Recorded> = requests
        .iter()
        .filter(|r| r.path.ends_with("/chat/completions"))
        .collect();
    assert_eq!(chat.len(), 2, "two steps, two requests");
    let first = messages_of(chat[0]);
    let second = messages_of(chat[1]);

    // The new messages are appended at the tail; nothing before them changed.
    assert!(second.len() > first.len(), "the second request grew");
    assert_eq!(
        &second[..first.len()],
        &first[..],
        "the first request's messages are a byte-identical prefix of the second"
    );
    let last = second.last().expect("a last message");
    assert_eq!(last["role"], "user");
    let parts = last["content"].as_array().expect("an image part list");
    assert_eq!(parts.len(), 1, "{last}");
    assert_eq!(parts[0]["type"], "image_url");
    let url = parts[0]["image_url"]["url"].as_str().unwrap();
    assert!(
        url.starts_with("data:image/png;base64,"),
        "the part is the image itself: {}",
        &url[..url.len().min(40)]
    );

    // The log records the pointer, never the bytes.
    let log = session.log();
    let sources: Vec<&Value> = log
        .iter()
        .filter(|event| event["type"] == "source")
        .collect();
    assert_eq!(sources.len(), 1, "{log:#?}");
    assert_eq!(sources[0]["kind"], "attachment");
    let path = sources[0]["path"].as_str().unwrap();
    assert!(
        path.starts_with("attachments/"),
        "the event names the session's copy: {path}"
    );
    assert!(
        !log.iter().any(|event| event.to_string().contains("base64")),
        "no bytes in the log"
    );

    session.end().await;
}

/// An image that is not there says so and does not break the turn: the call
/// settles with the message, and the next request carries no image.
#[tokio::test]
async fn kelir_view_refuses_a_missing_image_with_a_clear_message() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_view",
            "bash",
            &serde_json::json!({"command": "\"$KELIR_BIN\" view nope.png"}),
        ))),
        Reply::Sse(sse(&text_turn("I will ask for the right path", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "look"], "").await;
    session.wait_for("turn_ended").await;

    let log = session.log();
    let settled: Vec<&Value> = log
        .iter()
        .filter(|event| event["type"] == "tool_settled")
        .collect();
    assert_eq!(settled.len(), 1);
    let result = settled[0]["result"].as_str().unwrap_or_default();
    assert!(result.contains("no such image"), "{result}");
    // The verb event records the refusal: the log says why the image is not
    // there, not just that the command failed.
    let verb = log
        .iter()
        .find(|event| event["type"] == "verb" && event["name"] == "view")
        .expect("the view verb");
    assert!(
        verb["boundary"] == "rejected",
        "a refused view is not queued: {verb}"
    );
    assert!(
        verb["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no such image")
    );
    assert!(
        !log.iter().any(|event| event["type"] == "source"),
        "nothing was queued"
    );
    session.end().await;
}
