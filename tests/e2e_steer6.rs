//! STEER-6 end-to-end: the retro's loop improvements, driven through the real
//! binary against the scripted mock.
//!
//! * **F2** a 40 KB heredoc is not cut (it ran from a file, and its line count
//!   proves every line arrived);
//! * **F1** `reasoning_effort` reaches the request body and is recorded in
//!   `session_started`;
//! * **F3** `edits` applies several hunks in one call, or none;
//! * **F4** `parallel_tool_calls` is set only for a model that advertises it;
//! * **F5** `compact_at_tokens` compacts a big window early.

mod support;

use serde_json::{Value, json};
use support::*;

fn of_type<'a>(log: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|event| event["type"] == event_type)
        .collect()
}

/// A heredoc far past the 8,191-character Windows cut: it writes the file
/// itself and prints the count, so "the file has N lines" is the shell's own
/// statement about what it ran, not kelir's about what it sent.
fn big_heredoc(lines: usize) -> String {
    heredoc("big.txt", lines, "past the 8 KB cut")
}

/// The same shape with the file and the wording as parameters, so one fixture
/// serves the 40 KB bash call and the `bg run` job.
fn heredoc(file: &str, lines: usize, tail: &str) -> String {
    let mut cmd = format!("cat > {file} <<'KELIR_EOF'\n");
    for line in 0..lines {
        cmd.push_str(&format!("line {line} of a heredoc written {tail}\n"));
    }
    cmd.push_str(&format!("KELIR_EOF\nwc -l < {file}\n"));
    cmd
}

/// F2: a command over 40 KB runs from a file and is not truncated. This is the
/// regression the retro ranks highest: three M5-A calls were cut mid-heredoc,
/// two of them with exit 0, and the model never found the cause.
#[tokio::test]
async fn a_40_kb_heredoc_runs_from_a_file_and_writes_every_line() {
    const LINES: usize = 1_200;
    assert!(
        big_heredoc(LINES).len() > 40_000,
        "the fixture is {} bytes",
        big_heredoc(LINES).len()
    );
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_big",
            "bash",
            &json!({"command": big_heredoc(LINES)}),
        ))),
        Reply::Sse(sse(&text_turn("wrote it", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "write a big file"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let written = std::fs::read_to_string(run.cwd.join("big.txt")).unwrap();
    assert_eq!(
        written.lines().count(),
        LINES,
        "the heredoc was cut: {} of {LINES} lines",
        written.lines().count()
    );
    assert!(written.contains(&format!("line {} of a heredoc", LINES - 1)));

    let settled = run.one_event("tool_settled");
    assert_eq!(settled["outcome"], "ok", "{}", settled["result"]);
    assert!(
        settled["result"]
            .as_str()
            .unwrap()
            .contains(&LINES.to_string()),
        "the shell's own line count is the result: {}",
        settled["result"]
    );

    // The command lives in the call's own directory, not a temp dir: the bytes
    // that ran stay readable, which is what makes a cut diagnosable at all.
    let script = std::path::PathBuf::from(settled["out_path"].as_str().unwrap())
        .parent()
        .unwrap()
        .join("cmd.sh");
    assert!(script.is_file(), "no {} written", script.display());
    assert_eq!(
        std::fs::read_to_string(&script).unwrap(),
        big_heredoc(LINES),
        "the script is the command, byte for byte"
    );

    let result = settled["result"].as_str().unwrap();
    assert!(!result.contains("may have been cut"), "{result}");
}

/// F2's belt and braces: when the shell says a heredoc was cut, the model is
/// told that it may have been cut. Without this, the dogfood model blamed its
/// own heredoc body for twelve steps (M5-A, seq 65-78, 208-210).
#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn a_truncated_heredoc_warning_gets_the_notice() {
    // A heredoc whose closing marker does not match what opens it: the shell
    // runs to EOF and says so, exactly like a cut command line does.
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_cut",
            "bash",
            &json!({"command": "cat <<'WANTED'\nline one\nWRONG\n"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "cut a heredoc"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let settled = run.one_event("tool_settled");
    let result = settled["result"].as_str().unwrap();
    assert!(
        result.contains("delimited by end-of-file"),
        "the shell's own warning must stay: {result}"
    );
    assert!(
        result.contains("[kelir: your command may have been cut"),
        "the notice is appended, not instead: {result}"
    );
    assert!(
        result.contains("`edit`"),
        "the notice says what to do instead: {result}"
    );
}

/// F1: `reasoning_effort` reaches the request body and is recorded in
/// `session_started`, so the log and the request agree.
#[tokio::test]
async fn reasoning_effort_is_sent_and_recorded() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "say hi"],
        &[],
        None,
        "[providers.openrouter]\nreasoning_effort = \"low\"\n",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let started = run.one_event("session_started");
    assert_eq!(started["reasoning_effort"], "low");
    assert_eq!(mock.requests()[0].body["reasoning_effort"], "low");
}

/// F1: with no config, the loop asks for `medium` (its own default) rather than
/// leaving the choice to the provider's default.
#[tokio::test]
async fn the_default_effort_is_medium() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = run_task(&mock, &["run", "--json", "say hi"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(
        run.one_event("session_started")["reasoning_effort"],
        "medium"
    );
    assert_eq!(mock.requests()[0].body["reasoning_effort"], "medium");
}

/// F1: `none` sends no field at all and records nothing, which is how a session
/// opts back into the provider's own behaviour.
#[tokio::test]
async fn reasoning_effort_none_sends_no_field() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "say hi"],
        &[],
        None,
        "[providers.openrouter]\nreasoning_effort = \"none\"\n",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        run.one_event("session_started")
            .get("reasoning_effort")
            .is_none(),
        "the key is absent, not null"
    );
    assert!(mock.requests()[0].body.get("reasoning_effort").is_none());
}

/// F3: `edits` applies several hunks to one file in one call, and a call with a
/// hunk that does not match writes nothing at all.
#[tokio::test]
async fn a_multi_hunk_edit_is_atomic() {
    let mock = Mock::start(vec![
        // One call, three hunks: two real changes and one that misses.
        Reply::Sse(sse(&tool_turn(
            "call_miss",
            "edit",
            &json!({"path": "a.txt", "edits": [
                {"old": "", "new": "one\ntwo\nthree\n"},
                {"old": "two", "new": "TWO"},
                {"old": "nope", "new": "x"}
            ]}),
        ))),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "edit it"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let settled = run.one_event("tool_settled");
    assert_eq!(settled["outcome"], "error");
    assert!(
        settled["result"].as_str().unwrap().contains("not found"),
        "{}",
        settled["result"]
    );
    assert!(
        !run.cwd.join("a.txt").exists(),
        "an all-or-none edit must not create a half-applied file"
    );
}

/// F4: `parallel_tool_calls` is asked for only when the model advertises it, and
/// a two-call step really runs both (the loop was already concurrent; the
/// request now says so).
#[tokio::test]
async fn parallel_tool_calls_is_requested_for_a_capable_model() {
    // The support module's default list names `test/model` without the
    // capability, so this test brings its own list (it uses
    // `start_with_models` rather than editing the shared fixture).
    let models = ModelsAnswer::ok(
        r#"{"data":[{"id":"test/model","context_length":262144,
            "supported_parameters":["tools","parallel_tool_calls"]}]}"#,
    );
    let mock = Mock::start_with_models(
        vec![
            Reply::Sse(sse(&two_tool_turn(
                ("call_a", "bash", json!({"command": "printf 'a' > a.txt"})),
                ("call_b", "bash", json!({"command": "printf 'b' > b.txt"})),
            ))),
            Reply::Sse(sse(&text_turn("both", 0))),
        ],
        models,
    )
    .await;
    let run = run_task(&mock, &["run", "--json", "two calls"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(mock.requests()[0].body["parallel_tool_calls"], true);
    assert_eq!(run.events_of("tool_settled").len(), 2);
    assert!(run.cwd.join("a.txt").is_file());
    assert!(run.cwd.join("b.txt").is_file());
}

/// F5: the absolute soft target fires on a window whose fraction would never
/// reach it. The script is the M3 auto-compaction script (one step, then the
/// summary, then the answer); only the window and the target differ.
#[tokio::test]
async fn the_soft_target_compacts_a_big_window_early() {
    let script = || {
        vec![
            Reply::Sse(sse(&tool_turn(
                "call_1",
                "bash",
                &json!({"command": "printf 'ran\n'"}),
            ))),
            Reply::Sse(sse(&text_turn("SUMMARY: ran one command", 0))),
            Reply::Sse(sse(&text_turn("done", 0))),
        ]
    };
    // A 1M window at 80 % is 800K: nothing here would ever reach it. With the
    // target at 100 and the mock reporting 100 prompt tokens after step 1, the
    // next boundary compacts.
    let mock = Mock::start(script()).await;
    let sandbox = Sandbox::new("window = 1000000\ncompact_keep = 0\ncompact_at_tokens = 100\n");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "the task"], &[])
        .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let log = sandbox.log(&sandbox.only_session());
    let compacted = of_type(&log, "compacted");
    assert_eq!(compacted.len(), 1, "{log:?}");
    assert_eq!(compacted[0]["by"], "auto");
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2]
            .messages()
            .iter()
            .any(|message| message["content"]
                .as_str()
                .unwrap_or("")
                .contains("SUMMARY: ran one command")),
        "the summary is not in the request after the soft-target compaction"
    );
}

/// F5, the other half: a small window's own fraction still wins, so the target
/// cannot delay a compaction the window asked for.
#[tokio::test]
async fn a_small_window_keeps_its_own_fraction() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &json!({"command": "printf 'ran\n'"}),
        ))),
        Reply::Sse(sse(&text_turn("SUMMARY: ran one command", 0))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    // 100 tokens at 80 % is 80: it fires, even though the target is far away.
    let sandbox = Sandbox::new("window = 100\ncompact_keep = 0\ncompact_at_tokens = 150000\n");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "the task"], &[])
        .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let log = sandbox.log(&sandbox.only_session());
    let compacted = of_type(&log, "compacted");
    assert_eq!(compacted.len(), 1, "{log:?}");
    assert_eq!(compacted[0]["by"], "auto");
}

/// F2 for `kelir bg run`: a job command in the band that used to be cut (over
/// the shell's ~8 KB limit, under the ~32 KB a Windows process's own command
/// line can carry) runs from a file. A command *past* the process limit cannot
/// reach `kelir bg run` at all — its own argv is the limit — so the model
/// writes the script with `edit` and runs it; that is a CLI ceiling, recorded
/// in DEVIATIONS.md.
#[tokio::test]
async fn a_long_bg_job_command_runs_from_a_file() {
    const LINES: usize = 300;
    let heredoc = heredoc("job-big.txt", LINES, "in a background job");
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let sandbox = Sandbox::new("");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "unused"], &[])
        .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let job = sandbox
        .kelir(&mock.base_url(), &["bg", "run", &heredoc], &[])
        .await;
    assert_eq!(job.code, 0, "stderr: {}", job.stderr);
    let id = job
        .stdout
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    assert!(id.starts_with("bg_"), "{}", job.stdout);

    let status = sandbox.state.join("bg").join(format!("{id}.status"));
    let mut waited = 0;
    let settled = loop {
        let text = std::fs::read_to_string(&status).unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if value["state"] != "running" && !text.is_empty() {
            break value;
        }
        assert!(waited < 120, "the job never settled: {text}");
        waited += 1;
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    };
    assert_eq!(settled["exit_code"], 0, "{settled}");

    let written = std::fs::read_to_string(sandbox.cwd.join("job-big.txt")).unwrap();
    assert_eq!(
        written.lines().count(),
        LINES,
        "the bg job's heredoc was cut"
    );
    // The status file keeps a short reference; the script keeps the bytes.
    assert!(settled["cmd_path"].is_string(), "{settled}");
    let script = std::path::Path::new(settled["cmd_path"].as_str().unwrap());
    assert!(script.is_file(), "{}", script.display());
    assert_eq!(std::fs::read_to_string(script).unwrap(), heredoc);
}

/// S2: `parallel_tool_calls` must not silently depend on a warm model list. A
/// first session whose list is slow (past the fetch bound) and one whose list
/// endpoint is down both still ask a model whose family takes batching, and
/// still refuse a model nobody knows.
#[tokio::test]
async fn parallel_tool_calls_does_not_wait_for_a_slow_or_missing_list() {
    for (label, models) in [
        (
            "a slow list",
            ModelsAnswer::ok(MODELS_JSON).slow(std::time::Duration::from_secs(8)),
        ),
        ("a list that never loads", ModelsAnswer::http(503, "later")),
    ] {
        let mock =
            Mock::start_with_models(vec![Reply::Sse(sse(&text_turn("ok", 0)))], models).await;
        // The harness's model is `test/model`, which the family table does not
        // know; the point is that the list is not needed for a known family, so
        // this runs `deepseek/deepseek-v4.1-flash-1017`.
        let run = run_task(
            &mock,
            &[
                "run",
                "--json",
                "--model",
                "deepseek/deepseek-v4.1-flash-1017",
                "t",
            ],
            &[],
            None,
            "",
        )
        .await;
        assert_eq!(run.code, 0, "{label}: {}", run.stderr);
        let requests = mock.requests();
        assert_eq!(
            requests[0].body["parallel_tool_calls"], true,
            "{label}: a known family must be asked even with a cold list: {}",
            requests[0].body
        );
    }

    // An unknown model is still not asked, with no list at all.
    let mock = Mock::start_with_models(
        vec![Reply::Sse(sse(&text_turn("ok", 0)))],
        ModelsAnswer::http(503, "later"),
    )
    .await;
    let sandbox = Sandbox::new("");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "t"], &[])
        .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        mock.requests()[0].body.get("parallel_tool_calls").is_none(),
        "an unknown model was asked: {}",
        mock.requests()[0].body
    );
}

/// S3: a provider that answers 400 naming `reasoning_effort` costs one retry,
/// not the session: the field is dropped, an `error` note is logged, and no
/// later request carries it. The retry is not a provider-attempt retry (a 400
/// is not transient), so the turn still finishes in one step.
#[tokio::test]
async fn a_400_naming_the_effort_is_retried_once_without_it() {
    let mock = Mock::start(vec![
        Reply::Http(
            400,
            r#"{"error":{"message":"reasoning_effort is not supported for this model","param":"reasoning_effort"}}"#
                .to_string(),
        ),
        // The retried request: one step, then the answer.
        Reply::Sse(sse(&tool_turn(
            "call_1",
            "bash",
            &json!({"command": "printf 'ran\n'"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "the task"],
        &[],
        None,
        "[providers.openrouter]\nreasoning_effort = \"high\"\n",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        3,
        "the 400 was not retried once: {requests:?}"
    );
    assert_eq!(requests[0].body["reasoning_effort"], "high");
    assert!(
        requests[1].body.get("reasoning_effort").is_none(),
        "the retry still carries the refused field: {}",
        requests[1].body
    );
    assert_eq!(
        requests[2].body.get("reasoning_effort"),
        None,
        "a later step re-sent the field the provider refused: {}",
        requests[2].body
    );
    // The log says what happened, at `error`, so a person reading it sees the
    // field was dropped rather than silently ignored.
    let notes: Vec<&Value> = run
        .events_of("error")
        .into_iter()
        .filter(|event| {
            event["message"]
                .as_str()
                .unwrap_or("")
                .contains("reasoning_effort")
        })
        .collect();
    assert_eq!(notes.len(), 1, "{:?}", run.events);
    assert!(
        notes[0]["message"]
            .as_str()
            .unwrap()
            .contains("retrying without")
    );
    // The session finished its work: the tool call ran.
    assert_eq!(run.events_of("tool_settled").len(), 1);
}

/// S3: the default `medium` does not go to a model the provider's list says
/// does not take the parameter; an explicit config still does (the person
/// asked, and a gateway that refuses it is caught by the 400 retry above).
#[tokio::test]
async fn the_default_effort_is_gated_on_the_advertised_parameter() {
    let models = ModelsAnswer::ok(
        r#"{"data":[{"id":"test/model","context_length":262144,
            "supported_parameters":["tools","parallel_tool_calls"]}]}"#,
    );
    let mock = Mock::start_with_models(vec![Reply::Sse(sse(&text_turn("ok", 0)))], models).await;
    let run = run_task(&mock, &["run", "--json", "t"], &[], None, "").await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        mock.requests()[0].body.get("reasoning_effort").is_none(),
        "the default went to a model that does not advertise it: {}",
        mock.requests()[0].body
    );
    assert!(
        run.one_event("session_started")
            .get("reasoning_effort")
            .is_none(),
        "the log must agree with the request"
    );
}

/// N2, live: a PowerShell shell (`shell = ["powershell", "-NoProfile",
/// "-Command"]`) gets `-File cmd.ps1`, and the script really runs. `-Command
/// <path>` would have run the path as PowerShell source; dropping the switch
/// would read stdin (null here) and run nothing. Skipped where PowerShell is
/// absent, and it uses the inbox `powershell.exe`: a Store `pwsh` is an AppX
/// alias, whose process Windows refuses to put in a Job Object
/// (`cannot assign it to the job: Access is denied`), which is a Windows
/// packaging limit, not a kelir one (the STEER-6 reviewer hit the same wall).
#[cfg(windows)]
#[tokio::test]
async fn a_powershell_shell_runs_its_ps1_through_minus_file() {
    if std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", "exit 0"])
        .status()
        .map(|status| !status.success())
        .unwrap_or(true)
    {
        eprintln!("powershell is not installed here: skipping the PowerShell route");
        return;
    }
    let cmd = "Write-Output 'ps1 ran' | Set-Content ps.txt".to_string();
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn("c1", "bash", &json!({"command": cmd})))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "t"],
        &[],
        None,
        "shell = [\"powershell\", \"-NoProfile\", \"-Command\"]\n",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let settled = run.one_event("tool_settled");
    assert_eq!(settled["outcome"], "ok", "the ps1 did not run: {settled}");
    let written = std::fs::read_to_string(run.cwd.join("ps.txt")).unwrap_or_default();
    assert!(
        written.contains("ps1 ran"),
        "ps.txt = {written:?}; settled: {settled}"
    );
    // The script lives beside the call's output, as `cmd.ps1`.
    let script = std::path::PathBuf::from(settled["out_path"].as_str().unwrap())
        .parent()
        .unwrap()
        .join("cmd.ps1");
    assert!(script.is_file(), "no {}", script.display());
    assert!(
        std::fs::read(&script)
            .unwrap()
            .windows(2)
            .all(|pair| pair != b"\n\n"),
        "a .ps1 gets CRLF, one terminator"
    );
}
