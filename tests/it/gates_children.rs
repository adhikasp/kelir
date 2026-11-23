//! Nested gate behaviour in child sessions.
#![allow(clippy::pedantic, clippy::nursery)]

use super::gates_core::*;
use crate::support::*;
use serde_json::{Value, json};

/// A gate whose command runs kelir with the same config (and so the same gate).
/// Without the guard that is unbounded recursion; the script stops itself at 5
/// so a missing guard shows up as a counter, not a fork bomb.
#[tokio::test]
async fn a_gate_that_starts_kelir_does_not_start_gates_again() {
    let reenter = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/it/support/gate_reenter.py"
    );
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("inner done", 0))),
        Reply::Sse(sse(&text_turn("outer done", 0))),
    ])
    .await;
    let config = format!(
        "[gates]
session_start = {{ name = 'again', command = '{} \"{}\" \"{}\"' }}
",
        python(),
        fwd(reenter),
        fwd(dir.path())
    );
    let sandbox = Sandbox::new(&config);
    let plan = json!({
        "bin": env!("CARGO_BIN_EXE_kelir"),
        "args": ["run", "--json", "inner"],
        "env": {
            "KELIR_CONFIG": sandbox.config.to_string_lossy(),
            "KELIR_CONFIG_DIR": sandbox.state.join("no-config").to_string_lossy(),
            "KELIR_STATE_DIR": sandbox.state.to_string_lossy(),
            "KELIR_BASE_URL": mock.base_url(),
            "KELIR_HOME": sandbox.state.join("home").to_string_lossy(),
            "OPENROUTER_API_KEY": "test-openrouter-key",
        },
    });
    std::fs::write(dir.path().join("reenter.json"), plan.to_string()).unwrap();
    let out = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "outer"], &[])
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let runs = std::fs::read_to_string(dir.path().join("count")).unwrap_or_default();
    assert_eq!(runs.lines().count(), 1, "the inner kelir ran gates again");
    let ids = sandbox.session_ids();
    assert_eq!(
        ids.len(),
        2,
        "the gate's kelir should have made a second session"
    );
    let with_gates = ids
        .iter()
        .filter(|id| sandbox.log(id).iter().any(|e| e["type"] == "gate"))
        .count();
    assert_eq!(with_gates, 1, "only the outer session runs gates");
}

#[tokio::test]
async fn a_child_session_runs_its_own_scrubbed_gates_and_status_and_observe_show_them() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "c1",
            "bash",
            &json!({"command": "\"$KELIR_BIN\" run --child \"kid task\""}),
        ))),
        Reply::Sse(sse(&text_turn("kid done", 0))),
        Reply::Sse(sse(&tool_turn(
            "c2",
            "bash",
            &json!({"command": "\"$KELIR_BIN\" ctl \"$KELIR_ROOT\" status"}),
        ))),
        Reply::Sse(sse(&text_turn("root done", 0))),
    ])
    .await;
    let probe = Probe::new(json!({}));
    let events = [
        ("session_start", None),
        ("before_tool", None),
        ("session_end", None),
    ];
    let config = probe.config(&events);
    let canaries = [
        ("OPENROUTER_API_KEY", "canary-openrouter-77aa"),
        ("MY_OWN_KEY_ENV", "canary-custom-91bc"),
        ("KELIR_TOKEN", "canary-token-5d2e"),
    ];
    let done = drive(&mock, &config, "root task", &canaries).await;
    assert_eq!(done.out.code, 0, "{}", done.out.stderr);
    let ids = done.sandbox.session_ids();
    assert_eq!(ids.len(), 2, "{ids:?}");
    let logs: Vec<(String, Vec<Value>)> = ids
        .iter()
        .map(|id| (id.clone(), done.sandbox.log(id)))
        .collect();
    for (id, log) in &logs {
        let fired: Vec<&Value> = log.iter().filter(|e| e["type"] == "gate").collect();
        assert!(
            fired.iter().any(|e| e["event"] == "session_start"),
            "{id} never ran its session_start gate"
        );
        assert!(fired.iter().any(|e| e["event"] == "session_end"), "{id}");
        assert!(
            fired.iter().all(|e| e.get("error").is_none()),
            "{id}: {fired:?}"
        );
    }
    // The last session_start the probe saw is the child's: its own session dir,
    // and nothing secret in its environment.
    let child_id = logs
        .iter()
        .find(|(_, log)| log[0]["parent"].is_string())
        .map(|(id, _)| id.clone())
        .expect("a child session");
    let env = probe.env_of("session_start");
    assert!(
        env["KELIR_SESSION_DIR"]
            .as_str()
            .unwrap()
            .contains(&child_id),
        "{env}"
    );
    for event in ["session_start", "before_tool", "session_end"] {
        let text = probe.env_of(event).to_string();
        for (name, value) in canaries {
            assert!(!text.contains(value), "{event}: {name} reached a gate");
        }
    }
    // The root's `ctl status` result (a tool result in its log) carries the gate counters.
    let root_log = &logs.iter().find(|(id, _)| *id != child_id).unwrap().1;
    let status = root_log
        .iter()
        .filter(|e| e["type"] == "call_settled")
        .map(|e| e.to_string())
        .find(|text| text.contains("failed open"))
        .unwrap_or_else(|| {
            panic!(
                "no gate counters in ctl status: {:?}",
                root_log
                    .iter()
                    .filter(|e| e["type"] == "call_settled")
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        status.contains("3 declared") && status.contains("run(s)"),
        "{status}"
    );
    // `observe` on the child lists what fired in it.
    let out = done
        .sandbox
        .kelir(&mock.base_url(), &["observe", &child_id], &[])
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("== gates fired"), "{}", out.stdout);
    assert!(
        out.stdout.contains("session_start gate_probe"),
        "{}",
        out.stdout
    );
}
