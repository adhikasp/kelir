//! Gate visibility through `observe`.
#![allow(clippy::pedantic, clippy::nursery)]

use super::gates_core::*;
use crate::support::*;
use serde_json::json;

#[tokio::test]
async fn observe_lists_the_gates_and_what_fired_without_secrets() {
    let mock = Mock::start(one_call_then("done", "echo hi")).await;
    let probe = Probe::new(json!({"before_tool": {"mode": "crash"}}));
    let config = probe.config(&[("before_tool", None), ("turn_end", None)]);
    let done = drive(&mock, &config, "go", &[]).await;
    let id = done.sandbox.only_session();
    let out = done
        .sandbox
        .kelir(&mock.base_url(), &["observe", &id], &[])
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("== gates (2) =="), "{}", out.stdout);
    assert!(out.stdout.contains("before_tool") && out.stdout.contains("gate_probe"));
    assert!(
        out.stdout.contains("== gates fired (2) =="),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("failed open: exited with 3"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("turn_end gate_probe-2 -> allow"),
        "{}",
        out.stdout
    );
}

#[tokio::test]
async fn observe_prints_env_names_of_a_gate_but_never_their_values() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let config = "[gates]\nsession_end = { name = 'sync', command = 'true', env = { GATE_SECRET_CANARY = 'canary-value-7731' } }\n";
    let done = drive(&mock, config, "go", &[]).await;
    let id = done.sandbox.only_session();
    let out = done
        .sandbox
        .kelir(&mock.base_url(), &["observe", &id], &[])
        .await;
    assert!(
        out.stdout.contains("GATE_SECRET_CANARY") || out.stdout.contains("sync"),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains("canary-value-7731"), "{}", out.stdout);
    let log = std::fs::read_to_string(done.sandbox.session_dir(&id).join("events.jsonl")).unwrap();
    assert!(!log.contains("canary-value-7731"));
}

#[tokio::test]
async fn a_gate_gets_no_token_no_socket_and_no_provider_key() {
    let mock = Mock::start(one_call_then("done", "echo hi")).await;
    let probe = Probe::new(json!({}));
    let events = [
        ("session_start", None),
        ("prompt_submit", None),
        ("before_tool", None),
        ("after_tool", None),
        ("turn_end", None),
        ("session_end", None),
    ];
    let config = probe.config(&events);
    let canaries = [
        ("OPENROUTER_API_KEY", "canary-openrouter-5c1e"),
        ("KELIR_GO_API_KEY", "canary-go-8d2a"),
        ("MY_OWN_KEY_ENV", "canary-custom-3b7f"),
        ("KELIR_TOKEN", "canary-token-1a9c"),
        ("KELIR_SOCK", "canary-sock-6e4d"),
    ];
    let done = drive(&mock, &config, "go", &canaries).await;
    assert_eq!(done.out.code, 0, "{}", done.out.stderr);
    for (event, _) in events {
        let env = probe.env_of(event);
        let names: Vec<String> = env
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.to_ascii_uppercase())
            .collect();
        for banned in [
            "OPENROUTER_API_KEY",
            "KELIR_GO_API_KEY",
            "MY_OWN_KEY_ENV",
            "KELIR_TOKEN",
            "KELIR_SOCK",
        ] {
            assert!(
                !names.contains(&banned.to_string()),
                "{event}: {banned} reached the gate"
            );
        }
        let text = env.to_string();
        for (_, value) in canaries {
            assert!(
                !text.contains(value),
                "{event}: a canary value reached the gate's environment"
            );
        }
        assert_eq!(env["KELIR_GATE_EVENT"], event);
        assert!(
            env["KELIR_GATE_NAME"]
                .as_str()
                .unwrap()
                .starts_with("gate_probe")
        );
        assert!(env.get("KELIR_SESSION_DIR").is_some());
        for doc in probe.seen(event) {
            let text = doc.to_string();
            for (_, value) in canaries {
                assert!(
                    !text.contains(value),
                    "{event}: a canary reached the gate's stdin"
                );
            }
        }
    }
    // And nothing the session wrote or printed holds a canary either.
    let id = done.sandbox.only_session();
    let log = std::fs::read_to_string(done.sandbox.session_dir(&id).join("events.jsonl")).unwrap();
    for (_, value) in canaries {
        assert!(
            !log.contains(value)
                && !done.out.stdout.contains(value)
                && !done.out.stderr.contains(value)
        );
    }
}
