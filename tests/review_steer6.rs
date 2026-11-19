//! STEER-6 reviewer tests: adversarial cases around the retro-driven loop
//! changes (F2 long commands, F3 multi-hunk edit, F4 parallel calls, F5 soft
//! compaction target). Written against the real binary and the scripted mock.

mod support;

use serde_json::{Value, json};
use support::*;

fn settled<'a>(run: &'a Run, call: &str) -> &'a Value {
    run.events
        .iter()
        .find(|e| e["type"] == "tool_settled" && e["call_id"] == call)
        .unwrap_or_else(|| panic!("no tool_settled for {call}: {:?}", run.event_types()))
}

fn result_of<'a>(run: &'a Run, call: &str) -> &'a str {
    settled(run, call)["result"].as_str().unwrap_or("")
}

/// Run a series of single-tool turns in one session.
async fn drive(calls: Vec<(&str, &str, Value)>, env: &[(&str, &str)]) -> Run {
    let mut script = Vec::new();
    for (id, tool, args) in &calls {
        script.push(Reply::Sse(sse(&tool_turn(id, tool, args))));
    }
    script.push(Reply::Sse(sse(&text_turn("done", 0))));
    let mock = Mock::start(script).await;
    let run = run_task(&mock, &["run", "--json", "go"], env, None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    run
}

fn heredoc_to(file: &str, body: &str) -> String {
    format!("cat > {file} <<'KELIR_EOF'\n{body}KELIR_EOF\nwc -c < {file}\n")
}

// ------------------------------------------------------------------- F2

#[tokio::test]
async fn f2_a_100_kb_heredoc_writes_every_byte() {
    let mut body = String::new();
    for line in 0..2_700 {
        body.push_str(&format!(
            "line {line:05} of a very long heredoc body.....\n"
        ));
    }
    assert!(body.len() > 100_000, "{}", body.len());
    let cmd = heredoc_to("big100.txt", &body);
    let run = drive(vec![("c1", "bash", json!({"command": cmd}))], &[]).await;
    let written = std::fs::read(run.cwd.join("big100.txt")).unwrap();
    assert_eq!(written, body.as_bytes(), "the 100 KB heredoc was altered");
    assert_eq!(settled(&run, "c1")["outcome"], "ok");
    assert!(
        result_of(&run, "c1")
            .trim()
            .ends_with(&body.len().to_string()),
        "{}",
        result_of(&run, "c1")
    );
}

#[tokio::test]
async fn f2_quotes_unicode_dollar_and_backticks_survive_byte_for_byte() {
    let mut body = String::new();
    for i in 0..300 {
        body.push_str(&format!(
            "row {i}: \"double\" 'single' $HOME `tick` \\back\\slash $(echo no) é 日本語 🎉 %PATH% !bang\n"
        ));
    }
    assert!(body.len() > 20_000);
    let cmd = heredoc_to("uni.txt", &body);
    let run = drive(vec![("c1", "bash", json!({"command": cmd}))], &[]).await;
    let written = std::fs::read(run.cwd.join("uni.txt")).unwrap();
    assert_eq!(written, body.as_bytes());
}

#[tokio::test]
async fn f2_crlf_command_runs_and_does_not_break_the_terminator() {
    let mut cmd = String::from("cat > crlf.txt <<'KELIR_EOF'\r\n");
    for i in 0..400 {
        cmd.push_str(&format!("crlf row {i} padding padding padding\r\n"));
    }
    cmd.push_str("KELIR_EOF\r\nwc -l < crlf.txt\r\n");
    assert!(cmd.len() > 6_000);
    let run = drive(vec![("c1", "bash", json!({"command": cmd}))], &[]).await;
    assert_eq!(
        settled(&run, "c1")["outcome"],
        "ok",
        "{}",
        result_of(&run, "c1")
    );
    let written = std::fs::read_to_string(run.cwd.join("crlf.txt")).unwrap();
    assert_eq!(written.lines().count(), 400);
}

#[tokio::test]
async fn f2_cwd_env_and_exit_code_are_preserved() {
    let pad = "# ".to_string() + &"x".repeat(7_000);
    let cmd = format!(
        "{pad}\nmkdir -p sub\npwd > pwd1.txt\nprintf '%s' \"$KELIR_REVIEW_T\" > env.txt\ncd sub\npwd > pwd2.txt\nexit 7\n"
    );
    let run = drive(
        vec![
            ("c1", "bash", json!({"command": cmd})),
            ("c2", "bash", json!({"command": "pwd > pwd3.txt"})),
        ],
        &[("KELIR_REVIEW_T", "env-value-ünï")],
    )
    .await;
    let s = settled(&run, "c1");
    assert_eq!(s["exit_code"], 7, "{s}");
    assert_ne!(s["outcome"], "ok");
    assert_eq!(
        std::fs::read_to_string(run.cwd.join("env.txt")).unwrap(),
        "env-value-ünï"
    );
    // pwd1 is the session cwd (basename `work`), pwd2 is inside sub, pwd3 (a
    // later call) is back at the session cwd: no shell state carries over.
    let p1 = std::fs::read_to_string(run.cwd.join("pwd1.txt")).unwrap();
    let p2 = std::fs::read_to_string(run.cwd.join("sub").join("pwd2.txt"))
        .or_else(|_| std::fs::read_to_string(run.cwd.join("pwd2.txt")));
    let p3 = std::fs::read_to_string(run.cwd.join("pwd3.txt")).unwrap();
    assert!(p1.trim().ends_with("work"), "{p1}");
    assert!(p2.unwrap().trim().ends_with("work/sub"));
    assert!(p3.trim().ends_with("work"), "{p3}");
}

#[tokio::test]
async fn f2_a_syntax_error_in_a_long_command_is_not_ok() {
    let pad = "# ".to_string() + &"y".repeat(7_000);
    let cmd = format!("{pad}\nif then fi (\n");
    let run = drive(vec![("c1", "bash", json!({"command": cmd}))], &[]).await;
    let s = settled(&run, "c1");
    assert_ne!(s["outcome"], "ok", "{s}");
    assert!(s["exit_code"].as_i64().unwrap_or(0) != 0, "{s}");
}

#[tokio::test]
async fn f2_every_length_around_the_threshold_and_the_windows_cut_is_intact() {
    // Content is a heredoc of exactly N bytes of payload; the file must have it all.
    let lens = [
        5_900usize, 5_999, 6_000, 6_001, 6_100, 8_000, 8_190, 8_191, 8_192, 8_300, 9_000, 16_000,
    ];
    let mut calls = Vec::new();
    let mut expect = Vec::new();
    let names: Vec<String> = lens.iter().map(|n| format!("len{n}.txt")).collect();
    for (i, n) in lens.iter().enumerate() {
        let head = format!("cat > {} <<'KELIR_EOF'\n", names[i]);
        let tail = "\nKELIR_EOF\n";
        let payload_len = n.saturating_sub(head.len() + tail.len());
        let payload = "p".repeat(payload_len);
        let cmd = format!("{head}{payload}{tail}");
        assert_eq!(cmd.len(), *n);
        expect.push(payload);
        calls.push((format!("c{i}"), cmd));
    }
    let owned: Vec<(&str, &str, Value)> = calls
        .iter()
        .map(|(id, cmd)| (id.as_str(), "bash", json!({"command": cmd})))
        .collect();
    let run = drive(owned, &[]).await;
    for (i, n) in lens.iter().enumerate() {
        let written = std::fs::read_to_string(run.cwd.join(&names[i])).unwrap();
        assert_eq!(
            written.trim_end_matches('\n'),
            expect[i],
            "length {n}: payload altered"
        );
    }
}

/// A command UNDER the 6000-byte threshold can still blow the Windows command
/// line limit once every `"` and `\` is escaped for CreateProcess. This is the
/// quote-dense case.
#[tokio::test]
async fn f2_a_quote_dense_command_just_under_the_threshold_is_not_cut() {
    for (id, ch, count) in [("q1", '"', 5_000usize), ("q2", '\\', 5_500)] {
        let payload: String = std::iter::repeat_n(ch, count).collect();
        let cmd = format!("cat > {id}.txt <<'KELIR_EOF'\n{payload}\nKELIR_EOF\n");
        assert!(cmd.len() <= 6_000, "{}", cmd.len());
        let run = drive(vec![(id, "bash", json!({"command": cmd}))], &[]).await;
        let written = std::fs::read_to_string(run.cwd.join(format!("{id}.txt"))).unwrap();
        assert_eq!(
            written.trim_end().len(),
            payload.len(),
            "{id}: a {}-byte command with {count} '{ch}' was cut or altered; result: {}",
            cmd.len(),
            result_of(&run, id)
        );
        assert_eq!(written.trim_end(), payload);
    }
}

// ------------------------------------------------------------------- F3

fn edit_call(id: &str, args: Value) -> (String, Value) {
    (id.to_string(), args)
}

#[tokio::test]
async fn f3_edit_semantics() {
    let cwd_files: Vec<(&str, &[u8])> = vec![
        ("lf.txt", b"alpha\nbeta\ngamma\n"),
        ("crlf.txt", b"alpha\r\nbeta\r\ngamma\r\n"),
        ("dup.txt", b"x x x\nfoo\nx\n"),
        ("bin.bin", &[0xff, 0xfe, 0x00, 0x41]),
    ];
    // Seed files with a bash call first (short commands), then run edits.
    let mut seed = String::new();
    for (name, bytes) in &cwd_files {
        let hex: String = bytes.iter().map(|b| format!("\\x{b:02x}")).collect();
        seed.push_str(&format!("printf '{hex}' > {name}\n"));
    }
    let calls: Vec<(String, &str, Value)> = vec![
        ("seed".into(), "bash", json!({"command": seed})),
        // order: second hunk depends on the first one's output
        edit_call(
            "ord",
            json!({"path": "lf.txt", "edits": [
                {"old": "alpha", "new": "ALPHA beta"},
                {"old": "ALPHA beta", "new": "done"}]}),
        )
        .pipe("edit"),
        // failing last hunk: file must be untouched
        edit_call(
            "fail",
            json!({"path": "lf.txt", "edits": [
                {"old": "beta", "new": "BETA"},
                {"old": "nonexistent", "new": "x"}]}),
        )
        .pipe("edit"),
        // ambiguity after the first hunk makes the second fail: untouched
        edit_call(
            "amb",
            json!({"path": "dup.txt", "edits": [
                {"old": "foo", "new": "x"},
                {"old": "x", "new": "y"}]}),
        )
        .pipe("edit"),
        // replace_all in a hunk
        edit_call(
            "all",
            json!({"path": "dup.txt", "edits": [
                {"old": "x", "new": "z", "replace_all": true},
                {"old": "foo", "new": "bar"}]}),
        )
        .pipe("edit"),
        // CRLF file, LF patterns
        edit_call(
            "crlf",
            json!({"path": "crlf.txt", "edits": [
                {"old": "alpha\nbeta", "new": "one\ntwo\nthree"},
                {"old": "gamma", "new": "four"}]}),
        )
        .pipe("edit"),
        // both forms -> error, untouched
        edit_call(
            "both",
            json!({"path": "lf.txt", "old": "done", "new": "q", "edits": [{"old": "beta", "new": "b"}]}),
        )
        .pipe("edit"),
        edit_call("empty", json!({"path": "lf.txt", "edits": []})).pipe("edit"),
        edit_call("missing_new", json!({"path": "lf.txt", "edits": [{"old": "beta"}]})).pipe("edit"),
        edit_call("stringy", json!({"path": "lf.txt", "edits": "[{\"old\":\"beta\",\"new\":\"b\"}]"}))
            .pipe("edit"),
        edit_call("binary", json!({"path": "bin.bin", "edits": [{"old": "A", "new": "B"}]}))
            .pipe("edit"),
        // create + refine in one call
        edit_call(
            "create",
            json!({"path": "sub/new.txt", "edits": [
                {"old": "", "new": "hello world\n"},
                {"old": "world", "new": "there"}]}),
        )
        .pipe("edit"),
        // create with a missing hunk after: file must not exist
        edit_call(
            "createfail",
            json!({"path": "nope.txt", "edits": [
                {"old": "", "new": "hello\n"},
                {"old": "zzz", "new": "q"}]}),
        )
        .pipe("edit"),
        // a replacement that contains the pattern with replace_all must not loop
        edit_call(
            "selfref",
            json!({"path": "dup.txt", "edits": [{"old": "bar", "new": "bar bar", "replace_all": true}]}),
        )
        .pipe("edit"),
        // an empty `old` on an existing file inside edits
        edit_call(
            "emptyold",
            json!({"path": "lf.txt", "edits": [{"old": "", "new": "x"}]}),
        )
        .pipe("edit"),
    ];
    let owned: Vec<(&str, &str, Value)> = calls
        .iter()
        .map(|(id, tool, args)| (id.as_str(), *tool, args.clone()))
        .collect();
    let run = drive(owned, &[]).await;
    let read = |n: &str| std::fs::read(run.cwd.join(n)).unwrap();

    assert_eq!(
        settled(&run, "ord")["outcome"],
        "ok",
        "{}",
        result_of(&run, "ord")
    );
    // after ord lf.txt = "done\nbeta\ngamma\n"; "fail" must not touch it
    assert_eq!(settled(&run, "fail")["outcome"], "error");
    assert_eq!(read("lf.txt"), b"done\nbeta\ngamma\n");

    assert_eq!(
        settled(&run, "amb")["outcome"],
        "error",
        "{}",
        result_of(&run, "amb")
    );
    // dup.txt after amb untouched, then `all` ran: x->z everywhere, foo->bar
    assert_eq!(
        settled(&run, "all")["outcome"],
        "ok",
        "{}",
        result_of(&run, "all")
    );
    assert_eq!(settled(&run, "selfref")["outcome"], "ok");
    assert_eq!(read("dup.txt"), b"z z z\nbar bar\nz\n");

    assert_eq!(
        settled(&run, "crlf")["outcome"],
        "ok",
        "{}",
        result_of(&run, "crlf")
    );
    assert_eq!(read("crlf.txt"), b"one\r\ntwo\r\nthree\r\nfour\r\n");

    for id in [
        "both",
        "empty",
        "missing_new",
        "stringy",
        "binary",
        "createfail",
        "emptyold",
    ] {
        assert_eq!(
            settled(&run, id)["outcome"],
            "error",
            "{id}: {}",
            result_of(&run, id)
        );
    }
    assert_eq!(read("bin.bin"), vec![0xff, 0xfe, 0x00, 0x41]);
    assert!(!run.cwd.join("nope.txt").exists());
    assert_eq!(read("lf.txt"), b"done\nbeta\ngamma\n");
    assert_eq!(settled(&run, "create")["outcome"], "ok");
    assert_eq!(read("sub/new.txt"), b"hello there\n");
}

/// Little helper so the table above reads as (id, args).pipe(tool).
trait Pipe {
    fn pipe(self, tool: &'static str) -> (String, &'static str, Value);
}
impl Pipe for (String, Value) {
    fn pipe(self, tool: &'static str) -> (String, &'static str, Value) {
        (self.0, tool, self.1)
    }
}

// ------------------------------------------------------------------- F4

fn models_with(params: &str) -> ModelsAnswer {
    ModelsAnswer::ok(format!(
        r#"{{"data":[{{"id":"test/model","context_length":262144,"supported_parameters":{params}}}]}}"#
    ))
}

#[tokio::test]
async fn f4_parallel_tool_calls_absent_when_not_advertised_or_unknown() {
    for (label, models) in [
        ("not advertised", models_with(r#"["tools"]"#)),
        (
            "model not in list",
            ModelsAnswer::ok(
                r#"{"data":[{"id":"other/different","supported_parameters":["parallel_tool_calls"]}]}"#,
            ),
        ),
        ("models endpoint down", ModelsAnswer::http(500, "boom")),
    ] {
        let mock =
            Mock::start_with_models(vec![Reply::Sse(sse(&text_turn("ok", 0)))], models).await;
        let run = run_task(&mock, &["run", "--json", "hi"], &[], None, "").await;
        assert_eq!(run.code, 0, "{label}: {}", run.stderr);
        assert!(
            mock.requests()[0].body.get("parallel_tool_calls").is_none(),
            "{label}: {}",
            mock.requests()[0].body
        );
    }
}

// ------------------------------------------------------------------- F1

#[tokio::test]
async fn f1_the_system_message_is_deterministic_and_the_prefix_is_stable() {
    let mut systems = Vec::new();
    for _ in 0..2 {
        let mock = Mock::start(vec![
            Reply::Sse(sse(&tool_turn(
                "c1",
                "bash",
                &json!({"command": "echo hi"}),
            ))),
            Reply::Sse(sse(&text_turn("ok", 0))),
        ])
        .await;
        let run = run_task(&mock, &["run", "--json", "hi"], &[], None, "").await;
        assert_eq!(run.code, 0, "{}", run.stderr);
        let reqs = mock.requests();
        let first = reqs[0].messages()[0].clone();
        let second = reqs[1].messages()[0].clone();
        assert_eq!(first, second, "system message changed between steps");
        // step 2's messages start with step 1's messages (append-only)
        let a = reqs[0].messages();
        let b = reqs[1].messages();
        assert_eq!(&b[..a.len()], a, "the prefix was rewritten");
        assert_eq!(reqs[0].body["tools"], reqs[1].body["tools"]);
        assert_eq!(reqs[0].body["reasoning_effort"], "medium");
        assert_eq!(reqs[1].body["reasoning_effort"], "medium");
        let text = first["content"].as_str().unwrap().to_string();
        let head = text.split("Directories:").next().unwrap().to_string();
        systems.push((head, reqs[0].body["tools"].clone()));
    }
    assert_eq!(
        systems[0], systems[1],
        "system head/tools differ between two runs"
    );
}

#[tokio::test]
async fn f1_the_effort_is_also_sent_on_the_compaction_request() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "c1",
            "bash",
            &json!({"command": "echo hi"}),
        ))),
        Reply::Sse(sse(&text_turn("SUMMARY", 0))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let sandbox = Sandbox::new(
        "window = 100\ncompact_keep = 0\n[providers.openrouter]\nreasoning_effort = \"high\"\n",
    );
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "t"], &[])
        .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let reqs = mock.requests();
    assert_eq!(reqs.len(), 3);
    for (i, r) in reqs.iter().enumerate() {
        assert_eq!(r.body["reasoning_effort"], "high", "request {i}");
    }
}

// ------------------------------------------------------------------- F5

/// Boundary of the soft target on a real session: a step reporting exactly
/// target-1 prompt tokens does not compact, exactly target does.
#[tokio::test]
async fn f5_the_soft_target_boundary_is_exact() {
    for (reported, expect_compact) in [(149_999u64, false), (150_000u64, true)] {
        let mut chunks = tool_call_chunks("c1", "bash", &json!({"command": "echo ran"}));
        chunks.push(finish("tool_calls"));
        chunks.push(usage_chunk(reported, 10, 0, 0));
        let mut script = vec![Reply::Sse(sse(&chunks))];
        if expect_compact {
            script.push(Reply::Sse(sse(&text_turn("SUMMARY: ran", 0))));
        }
        script.push(Reply::Sse(sse(&text_turn("done", 0))));
        let mock = Mock::start(script).await;
        let sandbox = Sandbox::new("window = 1048576\ncompact_keep = 0\n");
        let run = sandbox
            .kelir(&mock.base_url(), &["run", "--json", "t"], &[])
            .await;
        assert_eq!(run.code, 0, "{}", run.stderr);
        let log = sandbox.log(&sandbox.only_session());
        let n = log.iter().filter(|e| e["type"] == "compacted").count();
        assert_eq!(
            n,
            usize::from(expect_compact),
            "reported {reported}: {log:?}"
        );
    }
}

#[tokio::test]
async fn f5_zero_disables_the_soft_target() {
    let mut chunks = tool_call_chunks("c1", "bash", &json!({"command": "echo ran"}));
    chunks.push(finish("tool_calls"));
    chunks.push(usage_chunk(400_000, 10, 0, 0));
    let mock = Mock::start(vec![
        Reply::Sse(sse(&chunks)),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let sandbox = Sandbox::new("window = 1048576\ncompact_keep = 0\ncompact_at_tokens = 0\n");
    let run = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "t"], &[])
        .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let log = sandbox.log(&sandbox.only_session());
    assert_eq!(log.iter().filter(|e| e["type"] == "compacted").count(), 0);
}

/// STEER-6 review S1, kept as a regression: on Windows the *argument* route
/// halved runs of backslashes (msys argv parsing). One route now — a file, on
/// Windows — so the same payload means the same bytes at any length. This was
/// the failing test that proved it; the fix un-ignored it.
#[tokio::test]
async fn f2_backslashes_and_dollars_are_literal_in_a_quoted_heredoc_short_and_long() {
    // Rust source `\\` is one real backslash: this payload holds runs of 2, 1, 3, 4.
    let payload = "a\\\\b \\n \\\\\\ c:\\\\dir \\\\\\\\ $HOME `x` \"q\" 'r'\n";
    let short = format!("cat > s.txt <<'KELIR_EOF'\n{payload}KELIR_EOF\n");
    let pad = format!("# {}\n", "z".repeat(7000));
    let long = format!("{pad}cat > l.txt <<'KELIR_EOF'\n{payload}KELIR_EOF\n");
    let run = drive(
        vec![
            ("s", "bash", json!({"command": short})),
            ("l", "bash", json!({"command": long})),
        ],
        &[],
    )
    .await;
    let s = std::fs::read_to_string(run.cwd.join("s.txt")).unwrap();
    let l = std::fs::read_to_string(run.cwd.join("l.txt")).unwrap();
    println!("PAYLOAD {payload:?}\nSHORT   {s:?}\nLONG    {l:?}");
    assert_eq!(l, payload, "the file route altered the payload");
    assert_eq!(s, payload, "the argument route altered the payload");
}

/// The cmd.exe fallback shell (`shell = ["cmd.exe", "/C"]`, what kelir picks on
/// Windows with no bash): the command is written to `cmd.bat` and must be RUN.
/// `cmd.exe cmd.bat` (no `/C`) starts an interactive shell on a null stdin, runs
/// nothing, and exits 0 — that was the B1 blocker; the switch now stays with the
/// file, and this test is no longer ignored.
#[cfg(windows)]
#[tokio::test]
async fn f2_cmd_exe_runs_a_long_command_from_its_bat_file() {
    let short = "echo short> short.txt".to_string();
    let mut long = String::new();
    for i in 0..400 {
        long.push_str(&format!(
            "REM padding line {i} to pass the six thousand byte threshold\r\n"
        ));
    }
    long.push_str("echo long> long.txt\r\n");
    assert!(long.len() > 6_000);
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn("s", "bash", &json!({"command": short})))),
        Reply::Sse(sse(&tool_turn("l", "bash", &json!({"command": long})))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "go"],
        &[],
        None,
        "shell = [\"cmd.exe\", \"/C\"]\n",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert!(
        run.cwd.join("short.txt").is_file(),
        "the short command did not run: {}",
        result_of(&run, "s")
    );
    assert_eq!(settled(&run, "l")["exit_code"], 0);
    assert!(
        run.cwd.join("long.txt").is_file(),
        "a long cmd.exe command reported exit 0 but never ran: {}",
        result_of(&run, "l")
    );
}
