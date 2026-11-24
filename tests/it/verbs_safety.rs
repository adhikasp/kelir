//! STEER-7 round 4 fixes: no verb logs request args wholesale. A canary secret in any argument of
//! any verb never appears in `events.jsonl`, the NDJSON stdout, `observe` output, or the answer.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::*;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn raw(session: &LiveSession, request: Value) -> Value {
    let stream = kelir::ipc::connect(&session.session_dir, &session.id)
        .await
        .expect("connect");
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read);
    let mut line = request.to_string();
    line.push('\n');
    let mut answer = String::new();
    write.write_all(line.as_bytes()).await.unwrap();
    write.flush().await.unwrap();
    lines.read_line(&mut answer).await.unwrap();
    serde_json::from_str(answer.trim()).unwrap_or(json!({"unparsed": answer}))
}

#[tokio::test]
async fn a_canary_in_any_argument_of_any_verb_is_never_logged_or_echoed() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let token = kelir::sys::trust::read_token(&session.session_dir).unwrap();
    let canary = "CANARY-VERB-SECRET-7e-91c2d";
    let bg = kelir::util::path_string(&job_root(&session.session_dir));
    let mut answers = Vec::new();
    for (verb, from) in [
        ("say", "ctl"),
        ("later", "ctl"),
        ("stop", "ctl"),
        ("model", "ctl"),
        ("tools", "ctl"),
        ("skills", "ctl"),
        ("view", "ctl"),
        ("compact", "ctl"),
        ("cancel", "ctl"),
        ("bg", "job"),
        ("bg-spawn", "job"),
        ("bg-claim", "job"),
        ("mcp", "mcp"),
        ("no-such-verb", "ctl"),
    ] {
        let args = json!({
            "text": "hello", "id": "bg_not_ours", "name": "bad name", "model": "a b",
            "path": "no/such/file.png", "disable": ["nope"], "dir": bg, "out": "",
            "cmd": "echo", "shell": [], "kind": "job", "cwd": "no-such-dir-7e",
            "token": canary, "key": canary, "capability": canary, "secret": canary,
            "env": [["SOME_API_KEY", canary]], "extra": canary
        });
        let answer = raw(
            &session,
            json!({"verb": verb, "from": from, "token": token, "args": args}),
        )
        .await;
        answers.push(answer.to_string());
    }
    let log = std::fs::read_to_string(session.session_dir.join("events.jsonl")).unwrap();
    let stderr = session.stderr();
    let (_, stdout) = session.end().await;
    assert!(!log.contains(canary), "the canary is in events.jsonl");
    assert!(
        !stdout.contains(canary),
        "the canary is in the NDJSON stdout"
    );
    assert!(!stderr.contains(canary), "the canary is in stderr");
    for answer in answers {
        assert!(!answer.contains(canary), "an answer echoes it: {answer}");
    }
}
