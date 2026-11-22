//! STEER-6 fix round, reviewer tests (round 2): the one-route shell rule (S1),
//! `cmd.exe /C` and PowerShell (B1/N2), regressions around stdin, CRLF, unicode,
//! exit codes, and the S2/S3 capability gates. Real binary + scripted mock.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::*;
use serde_json::{Value, json};

fn settled<'a>(run: &'a Run, call: &str) -> &'a Value {
    run.events
        .iter()
        .find(|e| e["type"] == "call_settled" && e["id"] == call)
        .unwrap_or_else(|| panic!("no tool_settled for {call}: {:?}", run.event_types()))
}

fn result_of<'a>(run: &'a Run, call: &str) -> &'a str {
    settled(run, call)["result"].as_str().unwrap_or("")
}

async fn drive(calls: Vec<(String, Value)>, config: &str, env: &[(&str, &str)]) -> Run {
    let mut script = Vec::new();
    for (id, args) in &calls {
        script.push(Reply::Sse(sse(&tool_turn(id, "bash", args))));
    }
    script.push(Reply::Sse(sse(&text_turn("done", 0))));
    let mock = Mock::start(script).await;
    let run = run_task(&mock, &["run", "--json", "go"], env, None, config).await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    run
}

/// S1: a backslash run of every interesting length is byte-exact through a
/// quoted heredoc, on one route (the same bytes at 1 and at 9000).
#[tokio::test]
async fn s1_backslash_runs_are_byte_exact_at_every_length() {
    let lengths: Vec<usize> = (1..=40)
        .chain([
            63, 64, 100, 255, 256, 999, 1000, 4095, 4096, 5990, 5999, 6000, 6001, 6002, 6100, 8000,
            8185, 8190, 8191, 8192, 8200, 9000,
        ])
        .collect();
    let mut calls = Vec::new();
    for n in &lengths {
        let run_of = "\\".repeat(*n);
        let cmd = format!("cat > b{n}.txt <<'KELIR_EOF'\n{run_of}\nKELIR_EOF\n");
        calls.push((format!("c{n}"), json!({"command": cmd})));
    }
    let run = drive(calls, "", &[]).await;
    for n in &lengths {
        let got = std::fs::read(run.cwd.join(format!("b{n}.txt"))).unwrap();
        let want = format!("{}\n", "\\".repeat(*n));
        assert_eq!(
            got.len(),
            want.len(),
            "run of {n} backslashes came back as {}",
            got.len()
        );
        assert_eq!(got, want.as_bytes(), "run of {n}");
    }
}

/// S1: the same command *text* at the same meaning whether the padding makes it
/// 100 B or 7 KB (printf, sed, grep -E, Windows-looking paths).
#[tokio::test]
async fn s1_printf_sed_regex_and_paths_mean_the_same_short_and_long() {
    let body = concat!(
        "printf 'a\\\\nb\\n' > p.txt\n", // printf 'a\\nb\n'  -> a\nb literal backslash-n
        "printf 'x\\ty\\n' >> p.txt\n",  // tab
        "echo 'C:\\Users\\the maintainer\\x' >> p.txt\n",
        "echo 'a\\b' | sed 's/a\\\\/X/' >> p.txt\n",
        "echo 'a.b' | grep -E 'a\\.b' >> p.txt\n",
        "echo \"q\\\"uote\" >> p.txt\n",
        "echo \\\\\\\\ >> p.txt\n", // unquoted \\\\ -> bash gives \\
    );
    let short = body.to_string();
    let long = format!("# {}\n{body}", "z".repeat(7000)).replace("p.txt", "q.txt");
    let run = drive(
        vec![
            ("s".into(), json!({"command": short})),
            ("l".into(), json!({"command": long})),
        ],
        "",
        &[],
    )
    .await;
    let p = std::fs::read(run.cwd.join("p.txt")).unwrap();
    let q = std::fs::read(run.cwd.join("q.txt")).unwrap();
    assert_eq!(
        p,
        q,
        "short and long routes disagree:\nshort={:?}\nlong={:?}",
        String::from_utf8_lossy(&p),
        String::from_utf8_lossy(&q)
    );
    let text = String::from_utf8_lossy(&p);
    assert!(text.contains("a\\nb\n"), "{text:?}");
    assert!(text.contains("x\ty"), "{text:?}");
    assert!(text.contains("C:\\Users\\the maintainer\\x"), "{text:?}");
    assert!(text.contains("Xb"), "{text:?}");
    assert!(text.contains("a.b"), "{text:?}");
    assert!(text.contains("q\"uote"), "{text:?}");
    assert!(
        text.contains("\\\\\n") || text.contains("\\\\\r\n"),
        "{text:?}"
    );
}

/// Regressions on the always-file route: exit code, cwd, env, stdin, unicode,
/// CRLF, an empty command, a command with a trailing backslash-newline.
#[tokio::test]
async fn s1_short_command_regressions() {
    let run = drive(
        vec![
            ("ex".into(), json!({"command": "exit 3"})),
            ("env".into(), json!({"command": "printf '%s' \"$KR_T\" > env.txt; mkdir -p d; cd d; pwd > ../pwd.txt"})),
            ("stdin".into(), json!({"command": "cat > in.txt; echo after"})),
            ("uni".into(), json!({"command": "printf 'é日本語🎉' > uni.txt"})),
            ("crlf".into(), json!({"command": "echo one\r\necho two > crlf.txt\r\n"})),
            ("empty".into(), json!({"command": ""})),
            ("cont".into(), json!({"command": "echo a \\\n  b > cont.txt"})),
            ("last".into(), json!({"command": "echo $((1+1))"})),
            ("syn".into(), json!({"command": "if then ("})),
        ],
        "",
        &[("KR_T", "väl")],
    )
    .await;
    assert_eq!(settled(&run, "ex")["exit_code"], 3);
    assert_ne!(settled(&run, "ex")["outcome"], "ok");
    assert_eq!(
        std::fs::read_to_string(run.cwd.join("env.txt")).unwrap(),
        "väl"
    );
    assert!(
        std::fs::read_to_string(run.cwd.join("pwd.txt"))
            .unwrap()
            .trim()
            .ends_with("d")
    );
    assert_eq!(
        settled(&run, "stdin")["outcome"],
        "ok",
        "{}",
        result_of(&run, "stdin")
    );
    assert!(result_of(&run, "stdin").contains("after"));
    assert_eq!(
        std::fs::read_to_string(run.cwd.join("uni.txt")).unwrap(),
        "é日本語🎉"
    );
    // N10 (fixed): a CRLF command means the same on both routes and both
    // platforms — the file is `crlf.txt`, not `crlf.txt<CR>`, and it holds
    // `two`. Before the fix this passed on Windows and failed on Linux.
    assert_eq!(
        std::fs::read_to_string(run.cwd.join("crlf.txt"))
            .unwrap()
            .trim(),
        "two",
        "a CRLF command must reach the shell as LF on every route"
    );
    // The CR really is gone rather than being part of the name: no entry in the
    // work dir has a CR in it.
    let cr_named = std::fs::read_dir(&run.cwd)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .any(|entry| entry.file_name().to_string_lossy().contains('\r'));
    assert!(
        !cr_named,
        "a file named crlf.txt<CR>: the route kept the CR"
    );
    assert!(
        result_of(&run, "crlf").contains("one"),
        "{}",
        result_of(&run, "crlf")
    );
    eprintln!("empty -> {}", settled(&run, "empty"));
    assert_eq!(
        std::fs::read_to_string(run.cwd.join("cont.txt"))
            .unwrap()
            .trim(),
        "a b"
    );
    assert!(
        result_of(&run, "last").trim().ends_with('2'),
        "{}",
        result_of(&run, "last")
    );
    assert_ne!(settled(&run, "syn")["outcome"], "ok");
}

/// B1: `cmd.exe /C` runs short and long commands, keeps exit codes and cwd, does
/// not echo the batch lines into the result.
#[cfg(windows)]
#[tokio::test]
async fn b1_cmd_exe_short_long_exit_cwd() {
    let long = format!("rem {}\r\necho long> long.txt\r\n", "x".repeat(20_000));
    let run = drive(
        vec![
            ("s".into(), json!({"command": "echo hello-cmd"})),
            ("l".into(), json!({"command": long})),
            ("e".into(), json!({"command": "exit /b 5"})),
            ("w".into(), json!({"command": "cd > cwd.txt"})),
            ("bs".into(), json!({"command": "echo a\\\\b> bs.txt"})),
            ("pct".into(), json!({"command": "echo 100%%> pct.txt"})),
        ],
        "shell = [\"cmd.exe\", \"/C\"]\n",
        &[],
    )
    .await;
    let s = result_of(&run, "s");
    assert!(s.contains("hello-cmd"), "{s}");
    assert_eq!(settled(&run, "s")["outcome"], "ok");
    eprintln!("cmd short result: {s:?}");
    // N6: `@echo off` first, so the result is the output, not the prompt, the
    // command line echoed back (20 KB of it for the long call), or the blank
    // line cmd.exe prints before running a batch file.
    assert_eq!(s.trim(), "hello-cmd", "the .bat echoed itself: {s:?}");
    assert!(
        !s.starts_with(
            "
"
        ),
        "cmd.exe's pre-batch blank line: {s:?}"
    );
    // The long call: no prompt echo either. (cmd.exe itself breaks a line
    // past its own ~8 KB limit and echoes the fragments, which is its parser
    // and not kelir's echo; the prompt and the blank first line are gone.)
    let l = result_of(&run, "l");
    assert!(!l.contains("work>"), "cmd.exe echoed its prompt: {l:.200}");
    assert!(l.len() < 6_000, "{} bytes of echo back", l.len());
    assert!(
        std::fs::read_to_string(run.cwd.join("long.txt"))
            .unwrap()
            .contains("long")
    );
    assert_eq!(settled(&run, "e")["exit_code"], 5);
    assert!(
        std::fs::read_to_string(run.cwd.join("cwd.txt"))
            .unwrap()
            .trim()
            .ends_with("work")
    );
    eprintln!("bs: {:?}", std::fs::read_to_string(run.cwd.join("bs.txt")));
    eprintln!(
        "pct: {:?}",
        std::fs::read_to_string(run.cwd.join("pct.txt"))
    );
}

/// N2: PowerShell -File route, quoting, a backslash run, exit code.
#[cfg(windows)]
#[tokio::test]
async fn n2_powershell_backslashes_quotes_and_exit_code() {
    let run = drive(
        vec![
            ("a".into(), json!({"command": "Set-Content -NoNewline -Path ps.txt -Value 'a\\\\b \"q\" $x é'"})),
            ("b".into(), json!({"command": "exit 4"})),
            ("c".into(), json!({"command": format!("# {}\nSet-Content long.txt hi", "y".repeat(20_000))})),
        ],
        "shell = [\"powershell\", \"-NoProfile\", \"-Command\"]\n",
        &[],
    )
    .await;
    let got = std::fs::read(run.cwd.join("ps.txt")).unwrap();
    eprintln!("ps.txt = {:?}", String::from_utf8_lossy(&got));
    assert_eq!(settled(&run, "b")["exit_code"], 4);
    assert!(run.cwd.join("long.txt").exists());
}

/// N5 (fixed): a login-shell argv (`bash -lc`) is a short-flag cluster ending
/// in `c`, so it takes the file route and 4 backslashes stay 4. Before the fix
/// it kept the argument route and came back halved; this test was ignored as
/// the failing proof.
#[cfg(windows)]
#[tokio::test]
async fn n5_a_login_shell_cluster_takes_the_file_route() {
    // The shell kelir resolves, with `-c` spelled as the login cluster `-lc`: a
    // bare `bash` is whatever PATH finds first, which on this box is a WSL
    // launcher and not the shell kelir picked.
    let mut shell = kelir::config::default_shell();
    assert_eq!(shell.last().map(String::as_str), Some("-c"), "{shell:?}");
    *shell.last_mut().unwrap() = "-lc".into();
    let config = format!("shell = {}\n", serde_json::to_string(&shell).unwrap());
    let cmd = "cat > r.txt <<'E'\n\\\\\\\\\nE\n";
    let run = drive(vec![("c".into(), json!({"command": cmd}))], &config, &[]).await;
    let got = std::fs::read_to_string(run.cwd.join("r.txt")).unwrap_or_default();
    eprintln!("bash -lc: 4 backslashes -> {got:?}");
    eprintln!("settled: {}", settled(&run, "c"));
    assert_eq!(
        got.trim_end(),
        "\\\\\\\\",
        "four backslashes in, {got:?} out"
    );
}

/// Round-2 N6 (fixed): the `.bat` starts with `@echo off`, so a cmd.exe call
/// result is the command's output and not the prompt plus the echoed command.
/// Before the fix this test was ignored as the failing proof.
#[cfg(windows)]
#[tokio::test]
async fn n6_cmd_exe_result_has_no_prompt_echo() {
    let run = drive(
        vec![("s".into(), json!({"command": "echo hello-cmd"}))],
        "shell = [\"cmd.exe\", \"/C\"]
",
        &[],
    )
    .await;
    assert_eq!(
        result_of(&run, "s").trim(),
        "hello-cmd",
        "{:?}",
        result_of(&run, "s")
    );
}

// ------------------------------------------------------------ S2 / S3 (mock)

fn err400(field: &str) -> Reply {
    Reply::Http(
        400,
        format!(
            r#"{{"error":{{"message":"Unsupported parameter: '{field}'","param":"{field}"}}}}"#
        ),
    )
}

/// S3: a gateway that keeps answering 400 about the field after the retry is a
/// hard error, not a loop (the retry happens at most once).
#[tokio::test]
async fn s3_a_400_that_repeats_after_the_retry_is_an_error_not_a_loop() {
    let mock = Mock::start(vec![
        err400("reasoning_effort"),
        err400("reasoning_effort"),
        err400("reasoning_effort"),
        err400("reasoning_effort"),
        err400("reasoning_effort"),
        err400("reasoning_effort"),
    ])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "t"],
        &[],
        None,
        "[providers.openrouter]\nreasoning_effort = \"high\"\n",
    )
    .await;
    assert_ne!(run.code, 0, "should fail");
    assert!(
        mock.request_count() <= 4,
        "{} requests: the retry looped",
        mock.request_count()
    );
    assert!(mock.requests()[1].body.get("reasoning_effort").is_none());
}

/// S3: a 400 about some other field is not retried with anything dropped.
#[tokio::test]
async fn s3_a_400_about_another_field_does_not_drop_the_effort() {
    let mock = Mock::start(vec![
        err400("temperature"),
        err400("temperature"),
        err400("temperature"),
        err400("temperature"),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "t"], &[], None, "").await;
    assert_ne!(run.code, 0);
    for (i, r) in mock.requests().iter().enumerate() {
        assert_eq!(
            r.body["reasoning_effort"], "medium",
            "request {i} lost the field: {}",
            r.body
        );
    }
}

/// S3: where the provider lists the model and does not advertise effort, an
/// explicit config value is still sent (the brief: "unknown models only when
/// explicitly configured" — here the model is *known without* the parameter,
/// the implementer's rule is that it is then withheld; record which).
#[tokio::test]
async fn s3_explicit_effort_on_a_listed_model_without_the_parameter() {
    let models = ModelsAnswer::ok(
        r#"{"data":[{"id":"test/model","context_length":262144,"supported_parameters":["tools"]}]}"#,
    );
    let mock = Mock::start_with_models(vec![Reply::Sse(sse(&text_turn("ok", 0)))], models).await;
    let run = run_task(
        &mock,
        &["run", "--json", "t"],
        &[],
        None,
        "[providers.openrouter]\nreasoning_effort = \"high\"\n",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    eprintln!(
        "explicit effort on listed-without model: {:?}",
        mock.requests()[0].body.get("reasoning_effort")
    );
}

/// S3: an unlisted model (list answers but does not contain it): default is not
/// sent, explicit is.
#[tokio::test]
async fn s3_unknown_model_default_withheld_explicit_sent() {
    let models = ModelsAnswer::ok(
        r#"{"data":[{"id":"other/thing","context_length":1000,"supported_parameters":["tools"]}]}"#,
    );
    let mock =
        Mock::start_with_models(vec![Reply::Sse(sse(&text_turn("ok", 0)))], models.clone()).await;
    let run = run_task(
        &mock,
        &["run", "--json", "--model", "vendor/unlisted", "t"],
        &[],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        mock.requests()[0].body.get("reasoning_effort").is_none(),
        "{}",
        mock.requests()[0].body
    );
    let mock = Mock::start_with_models(vec![Reply::Sse(sse(&text_turn("ok", 0)))], models).await;
    let run = run_task(
        &mock,
        &["run", "--json", "--model", "vendor/unlisted", "t"],
        &[],
        None,
        "[providers.openrouter]\nreasoning_effort = \"low\"\n",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(mock.requests()[0].body["reasoning_effort"], "low");
}

/// S2: the provider's entry wins over the family table: deepseek listed without
/// `parallel_tool_calls` is not asked.
#[tokio::test]
async fn s2_a_listed_entry_without_the_parameter_beats_the_family_table() {
    let models = ModelsAnswer::ok(
        r#"{"data":[{"id":"deepseek/deepseek-v4.1-flash-1017","context_length":1000,"supported_parameters":["tools","reasoning"]}]}"#,
    );
    let mock = Mock::start_with_models(vec![Reply::Sse(sse(&text_turn("ok", 0)))], models).await;
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
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        mock.requests()[0].body.get("parallel_tool_calls").is_none(),
        "{}",
        mock.requests()[0].body
    );
}

/// S2: a 400 naming parallel_tool_calls is retried without it (and the field
/// stays out of the later requests).
#[tokio::test]
async fn s2_a_400_naming_parallel_tool_calls_is_retried_without_it() {
    let mock = Mock::start(vec![
        err400("parallel_tool_calls"),
        Reply::Sse(sse(&tool_turn(
            "c1",
            "bash",
            &json!({"command": "echo hi"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let run = run_task(&mock, &["run", "--json", "t"], &[], None, "").await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let r = mock.requests();
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].body["parallel_tool_calls"], true);
    assert!(r[1].body.get("parallel_tool_calls").is_none());
    assert!(r[2].body.get("parallel_tool_calls").is_none());
    assert_eq!(
        r[1].body["reasoning_effort"], "medium",
        "only the refused field is dropped"
    );
}

// ------------------------------------------------------------------ N1 / N4

/// N1: ordinary output that merely contains the marker, exit 0, in the middle:
/// no "cut" notice. A real cut heredoc (shell diagnostic) still gets it.
#[tokio::test]
async fn n1_marker_in_ordinary_output_is_quiet_and_a_real_cut_is_not() {
    let run = drive(
        vec![
            (
                "q".into(),
                json!({"command": "printf 'unexpected EOF here\nline2\nline3\nline4\nline5\nend\n'"}),
            ),
            ("t".into(), json!({"command": "printf 'tail unexpected EOF'"})),
            ("f".into(), json!({"command": "printf 'unexpected EOF\nx\n'; exit 2"})),
            ("cut".into(), json!({"command": "cat <<'WANTED'\nline one\nWRONG\n"})),
        ],
        "",
        &[],
    )
    .await;
    let notice = "may have been cut";
    assert!(
        !result_of(&run, "q").contains(notice),
        "{}",
        result_of(&run, "q")
    );
    eprintln!(
        "marker at the end, exit 0 -> notice: {}",
        result_of(&run, "t").contains(notice)
    );
    eprintln!(
        "marker + exit 2 -> notice: {}",
        result_of(&run, "f").contains(notice)
    );
    assert!(
        result_of(&run, "cut").contains(notice),
        "{}",
        result_of(&run, "cut")
    );
}

/// N4: KELIR_SHELL_RAW=cr keeps CRLF terminators on a POSIX script; default
/// normalises them. Observable through a `$'\r'` probe.
#[cfg(windows)]
#[tokio::test]
async fn n4_raw_cr_switch_keeps_crlf() {
    // `echo a\r` — under RAW the script line ends in CR, so bash sees `echo a<CR>`
    // and prints "a\r"; normalised it prints "a".
    let cmd = "printf '%s' \"$(echo a)\" | od -c | head -1\r\necho a > raw.txt\r\n";
    let norm = drive(vec![("c".into(), json!({"command": cmd}))], "", &[]).await;
    let raw = drive(
        vec![("c".into(), json!({"command": cmd}))],
        "",
        &[("KELIR_SHELL_RAW", "cr")],
    )
    .await;
    let script = |run: &Run| {
        let out = settled(run, "c")["out_path"].as_str().unwrap().to_string();
        std::fs::read(std::path::Path::new(&out).parent().unwrap().join("cmd.sh"))
            .unwrap_or_default()
    };
    let (n, r) = (script(&norm), script(&raw));
    let has_cr = |bytes: &[u8]| bytes.contains(&0x0du8);
    eprintln!(
        "normalised cmd.sh has CR: {}; RAW=cr has CR: {}",
        has_cr(&n),
        has_cr(&r)
    );
    assert!(!has_cr(&n));
    assert!(
        has_cr(&r),
        "KELIR_SHELL_RAW=cr did not keep the CRLF terminators"
    );
}

/// A state dir with a space and a non-ASCII letter reaches the script route on
/// bash and on cmd.exe (script path with a space).
#[tokio::test]
async fn s1_state_dir_with_a_space_runs_on_every_shell() {
    let base = tempfile::tempdir().unwrap();
    let state = base.path().join("sp ace é");
    let state = state.to_string_lossy().to_string();
    let run = drive(
        vec![("c".into(), json!({"command": "echo spaced > sp.txt"}))],
        "",
        &[("KELIR_STATE_DIR", state.as_str())],
    )
    .await;
    assert!(run.cwd.join("sp.txt").exists());
    #[cfg(windows)]
    {
        let run = drive(
            vec![("c".into(), json!({"command": "echo spaced> sp.txt"}))],
            "shell = [\"cmd.exe\", \"/C\"]\n",
            &[("KELIR_STATE_DIR", state.as_str())],
        )
        .await;
        assert!(
            run.cwd.join("sp.txt").exists(),
            "cmd.exe + a space in the script path did not run"
        );
    }
}
