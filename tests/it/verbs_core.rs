//! M1 end-to-end: verbs over the session socket.
//!
//! The real binary runs as a `--keep` session against the scripted mock, and
//! `kelir ctl` drives it from a second process, exactly as a human or a script
//! would. Assertions are on the event log, on the mock's requests, and on the
//! session process itself.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::*;
use serde_json::Value;

pub(super) fn of_type<'a>(log: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|event| event["type"] == event_type)
        .collect()
}

pub(super) fn seq(event: &Value) -> u64 {
    event["seq"].as_u64().expect("seq")
}

/// The window compaction uses is the provider's answer, fetched once and
/// cached — not a table in kelir's source.
#[tokio::test]
async fn the_window_comes_from_the_providers_model_list() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;

    let answer = session.ctl(&["tools", "--disable", "bash"]).await;
    assert_eq!(answer.code, 0, "{}{}", answer.stdout, answer.stderr);
    let state = &answer.response()["state"];
    // The test config runs `test/model`; the mock lists it at 262144 tokens.
    assert_eq!(state["model"], "test/model");
    assert_eq!(
        state["window"], 262_144,
        "from the provider's list, not a table"
    );
    assert_eq!(
        state["window_source"], "api",
        "fetched before the first step"
    );
}
/// Poll until a call has written `needle` to its output file, and return the
/// forward-slashed path the event log uses.
pub(super) async fn wait_for_output(session: &LiveSession, needle: &str) -> String {
    let commands = session.session_dir.join("calls");
    let mut found = None;
    eventually(
        &format!(
            "a call output containing {needle:?} under {}",
            commands.display()
        ),
        || {
            if let Ok(entries) = std::fs::read_dir(&commands) {
                for entry in entries.flatten() {
                    let out = entry.path().join("out");
                    if let Ok(text) = std::fs::read_to_string(&out)
                        && text.contains(needle)
                    {
                        found = Some(out.to_string_lossy().replace('\\', "/"));
                        return true;
                    }
                }
            }
            false
        },
        || session.dump(),
    )
    .await;
    found.expect("found")
}
