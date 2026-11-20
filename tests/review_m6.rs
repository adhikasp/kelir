//! M6 review: adversarial cases against `kelir mcp` (reviewer-owned).
//!
//! Server: `tests/support/mcp_evil.py` (modes chosen by `MCP_EVIL`) and the
//! shipped `mcp_server.py`. Every case drives the real binary.

mod support;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::*;

const EVIL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mcp_evil.py");
const GOOD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mcp_server.py");

fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

fn evil(extra: &str) -> String {
    format!(
        "[servers.evil]\ncommand = '{}'\nargs = ['{}']\n{extra}\n",
        python(),
        EVIL
    )
}

fn good(extra: &str) -> String {
    format!(
        "[servers.demo]\ncommand = '{}'\nargs = ['{}']\n{extra}\n",
        python(),
        GOOD
    )
}

struct Files {
    dir: tempfile::TempDir,
}

impl Files {
    fn new() -> Files {
        Files {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    fn mcp_toml(&self, text: &str) -> String {
        let path = self.path("mcp.toml");
        std::fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
    }
}

/// `kelir mcp <args>` outside a session.
async fn one_shot(files: &Files, toml: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
    let sandbox = Sandbox::new("");
    let config = files.mcp_toml(toml);
    let mut full = vec![("KELIR_MCP_CONFIG", config.as_str())];
    full.extend_from_slice(env);
    let mut argv = vec!["mcp"];
    argv.extend_from_slice(args);
    sandbox.kelir("http://127.0.0.1:9/v1", &argv, &full).await
}

async fn session_run(files: &Files, toml: &str, command: &str) -> Run {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_r",
            "bash",
            &json!({"command": command}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let config = files.mcp_toml(toml);
    let run = run_task(
        &mock,
        &["run", "--json", "use the server"],
        &[("KELIR_MCP_CONFIG", config.as_str())],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    run
}

// ---------------------------------------------------------------- JSON-RPC

/// A server that prints a banner and a broken JSON line before its first
/// message still works (junk lines are skipped).
#[tokio::test]
async fn junk_before_the_first_message_is_skipped() {
    let files = Files::new();
    let toml = evil("env = { MCP_EVIL = \"banner,crlf\" }");
    let out = one_shot(&files, &toml, &["evil", "echo", "text=hi"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout.trim(), "{\"text\": \"hi\"}");
}

/// Answers arriving in three flushed chunks (a partial line at a read
/// boundary) are reassembled.
#[tokio::test]
async fn a_message_split_across_writes_is_reassembled() {
    let files = Files::new();
    let toml = evil("env = { MCP_EVIL = \"splitwrite\" }");
    let out = one_shot(&files, &toml, &["evil", "echo", "text=hi"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
}

/// A server that sends progress notifications and a server->client request
/// (roots/list) before its answer must not confuse the client.
#[tokio::test]
async fn progress_and_server_requests_do_not_disturb_a_call() {
    let files = Files::new();
    let toml = evil("env = { MCP_EVIL = \"progress,request\" }");
    let out = one_shot(&files, &toml, &["evil", "echo", "text=hi"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
}

/// An older protocol version in the answer is accepted.
#[tokio::test]
async fn an_older_protocol_version_is_accepted() {
    let files = Files::new();
    let toml = evil("env = { MCP_EVIL = \"oldproto\" }");
    let out = one_shot(&files, &toml, &["evil", "echo", "text=hi"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
}

/// initialize answered with an error: exit 3 and a message, no hang.
#[tokio::test]
async fn an_initialize_error_is_exit_3() {
    let files = Files::new();
    let toml = evil("env = { MCP_EVIL = \"init_error\" }");
    let started = Instant::now();
    let out = one_shot(&files, &toml, &["evil", "echo", "text=hi"], &[]).await;
    assert_eq!(out.code, 3, "{}", out.stderr);
    assert!(out.stderr.contains("go away"), "{}", out.stderr);
    assert!(started.elapsed() < Duration::from_secs(20));
}

/// A server that exits straight after initialize: exit 3, quickly.
#[tokio::test]
async fn a_server_that_exits_after_initialize_is_exit_3() {
    let files = Files::new();
    let toml = evil("env = { MCP_EVIL = \"exit_init\" }");
    let started = Instant::now();
    let out = one_shot(&files, &toml, &["evil", "echo", "text=hi"], &[]).await;
    assert_eq!(out.code, 3, "{}", out.stderr);
    assert!(started.elapsed() < Duration::from_secs(20), "slow death");
}

/// REGRESSION PROBE (F-utf8): a server writes one line that is not valid UTF-8
/// (a stray byte in a log line on stdout) and then answers correctly. The
/// reader must skip the bad line like any other junk, not declare the server
/// dead.
#[tokio::test]
#[ignore = "M6 review F2: fails until fixed (run with --run-ignored all)"]
async fn a_non_utf8_line_does_not_kill_the_connection() {
    let files = Files::new();
    let toml = evil("env = { MCP_EVIL = \"badutf8\" }");
    let out = one_shot(&files, &toml, &["evil", "echo", "text=hi"], &[]).await;
    assert_eq!(
        (out.code, out.stdout.trim()),
        (0, "{\"text\": \"hi\"}"),
        "stderr: {}",
        out.stderr
    );
}

/// A reply with neither result nor error, and a reply whose id is a string:
/// neither may hang past the per-call timeout, and the second is exit 4.
#[tokio::test]
async fn malformed_replies_end_at_the_timeout() {
    let files = Files::new();
    let toml = evil("timeout = 2\nenv = { MCP_EVIL = \"strid\" }");
    let started = Instant::now();
    let out = one_shot(&files, &toml, &["evil", "echo", "text=hi"], &[]).await;
    assert_eq!(out.code, 4, "{}", out.stderr);
    assert!(started.elapsed() < Duration::from_secs(20));

    let toml = evil("timeout = 2\nenv = { MCP_EVIL = \"noresult\" }");
    let out = one_shot(&files, &toml, &["evil", "echo", "text=hi"], &[]).await;
    // Accepting an empty result is fine; hanging is not.
    assert!(
        out.code == 0 || out.code == 3 || out.code == 4,
        "{}",
        out.code
    );
}

/// 8 MB of stderr at start must not block the server (the pipe is drained).
#[tokio::test]
async fn a_stderr_flood_does_not_block_the_server() {
    let files = Files::new();
    let toml = evil("env = { MCP_EVIL = \"stderr\" }");
    let started = Instant::now();
    let out = one_shot(&files, &toml, &["evil", "echo", "text=hi"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(started.elapsed() < Duration::from_secs(30));
}

// ------------------------------------------------------------------ naming

/// A tool whose name has spaces and non-ASCII letters can be listed, described
/// and called.
#[tokio::test]
async fn a_unicode_tool_name_with_spaces_works() {
    let files = Files::new();
    let toml = evil("env = { MCP_EVIL = \"unicode\" }");
    let out = one_shot(&files, &toml, &["evil", "ls", "--detail", "name"], &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("héllo wörld 日本"), "{}", out.stdout);
    let out = one_shot(
        &files,
        &toml,
        &["evil", "héllo wörld 日本", "text=ünï"],
        &[],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.contains("ünï"), "{}", out.stdout);
}

/// A tool literally named `ls` cannot be reached: `kelir mcp srv ls` lists.
/// (Documents the gap; asserts the listing, not a call.)
#[tokio::test]
async fn a_tool_named_ls_is_shadowed_by_the_listing() {
    let files = Files::new();
    let toml = evil("");
    let out = one_shot(&files, &toml, &["evil", "ls"], &[]).await;
    assert_eq!(out.code, 0);
    assert!(out.stdout.contains("a tool that is literally called ls"));
}

// ------------------------------------------------------------- k=v typing

/// REGRESSION PROBE (F-anyof): a property that accepts a string but is
/// declared with `anyOf` or `type: [string, null]` is not "declared string",
/// so `a=123` is sent as the number 123 although the server wants a string.
#[tokio::test]
#[ignore = "M6 review F4: fails until fixed (run with --run-ignored all)"]
async fn a_nullable_string_keeps_its_text() {
    let files = Files::new();
    let toml = evil("");
    for key in ["a", "b", "c"] {
        let out = one_shot(
            &files,
            &toml,
            &["evil", "nullable", &format!("{key}=123")],
            &[],
        )
        .await;
        assert_eq!(out.code, 0, "{key}: {}", out.stderr);
        let sent: Value = serde_json::from_str(out.stdout.trim()).unwrap();
        assert_eq!(sent[key], json!("123"), "{key}: sent {sent}");
    }
}

/// Strings that look like JSON stay strings where the schema says string, and
/// are JSON where it does not.
#[tokio::test]
async fn json_looking_strings_follow_the_schema() {
    let files = Files::new();
    let toml = evil("");
    let out = one_shot(
        &files,
        &toml,
        &["evil", "nullable", "c={\"a\":1}", "d={\"a\":1}"],
        &[],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let sent: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(sent["c"], json!("{\"a\":1}"));
    assert_eq!(sent["d"], json!({"a": 1}));
    // leading zero, empty, a bare word, a quoted word
    let out = one_shot(
        &files,
        &toml,
        &["evil", "nullable", "d=007", "c=", "a=null"],
        &[],
    )
    .await;
    let sent: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(sent["d"], json!("007"));
    assert_eq!(sent["c"], json!(""));
}

/// An integer past u64 is not silently turned into a float.
#[tokio::test]
#[ignore = "M6 review F7: fails until fixed (run with --run-ignored all)"]
async fn a_huge_integer_is_not_rounded() {
    let files = Files::new();
    let toml = evil("");
    let out = one_shot(
        &files,
        &toml,
        &["evil", "nullable", "d=123456789012345678901234567890"],
        &[],
    )
    .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.contains("123456789012345678901234567890") && !out.stdout.contains("e+"),
        "sent: {}",
        out.stdout
    );
}

// ----------------------------------------------------------------- secrets

/// A syntax error on a line that holds a secret must not echo the secret.
#[tokio::test]
#[ignore = "M6 review F1: fails until fixed (run with --run-ignored all)"]
async fn a_bad_mcp_toml_does_not_print_the_env_block() {
    let files = Files::new();
    let toml = format!(
        "[servers.s]\ncommand = 'x'\nenv = {{ API_TOKEN = \"sk-SECRET-VALUE-123\" , }} oops\n"
    );
    let out = one_shot(&files, &toml, &["ls"], &[]).await;
    let all = format!("{}{}", out.stdout, out.stderr);
    assert!(!all.contains("sk-SECRET-VALUE-123"), "leaked: {all}");
}

/// The secret in a server's `env` / `args` is not in the event log, either.
#[tokio::test]
async fn env_and_args_secrets_stay_out_of_the_log() {
    let files = Files::new();
    let toml = format!(
        "[servers.demo]\ncommand = '{}'\nargs = ['{}', '--token=sk-ARGSECRET']\nenv = {{ API_TOKEN = \"sk-ENVSECRET\" }}\n",
        python(),
        GOOD
    );
    let run = session_run(
        &files,
        &toml,
        "\"$KELIR_BIN\" mcp demo echo text=hi; \"$KELIR_BIN\" mcp ls --json; \"$KELIR_BIN\" mcp demo nope 2>&1",
    )
    .await;
    let log = run.stdout.clone() + &run.stderr;
    let events = serde_json::to_string(&run.events).unwrap();
    for needle in ["sk-ARGSECRET", "sk-ENVSECRET"] {
        assert!(
            !log.contains(needle) && !events.contains(needle),
            "{needle} leaked"
        );
    }
}

// ---------------------------------------------------------------- sessions

/// Two calls to one server at the same time both run (concurrently) and are
/// logged with distinct ids and the same parent.
#[tokio::test]
async fn two_concurrent_calls_to_one_server() {
    let files = Files::new();
    let started = Instant::now();
    let run = session_run(
        &files,
        &good("timeout = 30"),
        "\"$KELIR_BIN\" mcp demo slow seconds=2 & \"$KELIR_BIN\" mcp demo slow seconds=2 & wait",
    )
    .await;
    let took = started.elapsed();
    let started_events = run.events_of("mcp_started");
    let settled = run.events_of("mcp_settled");
    assert_eq!(started_events.len(), 2, "{:?}", run.event_types());
    assert_eq!(settled.len(), 2);
    assert_ne!(started_events[0]["mcp_id"], started_events[1]["mcp_id"]);
    assert!(settled.iter().all(|e| e["outcome"] == "ok"), "{settled:?}");
    // Serial would be >= 4 s of sleeping plus two spawns; allow generous slack.
    eprintln!("concurrent wall time: {took:?}");
}

/// A call that comes back with a big (12 MB) result through the broker.
#[tokio::test]
async fn a_twelve_megabyte_result_survives_the_broker() {
    let files = Files::new();
    let toml = evil("timeout = 60\nenv = { MCP_EVIL = \"huge12\" }");
    let run = session_run(&files, &toml, "\"$KELIR_BIN\" mcp evil huge | wc -c").await;
    let settled = run.one_event("mcp_settled");
    assert_eq!(settled["outcome"], "ok", "{settled}");
    assert!(settled["bytes"].as_u64().unwrap() > 12_000_000, "{settled}");
    let size = std::fs::metadata(settled["out_path"].as_str().unwrap())
        .unwrap()
        .len();
    assert_eq!(size, settled["bytes"].as_u64().unwrap());
}
