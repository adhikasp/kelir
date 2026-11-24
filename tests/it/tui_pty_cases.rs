//! PTY-driven TUI cases: turns, markdown, idle, Ctrl-C and panic recovery.
#![allow(clippy::pedantic, clippy::nursery)]

use super::tui_output::{herdr_records, plain};
use super::tui_pty::{drive_and_count_idle_bytes, drive_in_a_pty, panic_in_a_pty};
use crate::support::{Mock, Reply, sse, text_turn};

/// The facts row stays calm even with a cap: no step counter is ambient, and
/// the banner is where the cap is named.

#[tokio::test]
async fn the_tui_runs_a_turn_in_a_pty() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("PARROT", 0)))]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();
    let state = temp.path().join("state");
    let base_url = mock.base_url();
    let run = tokio::task::spawn_blocking(move || {
        drive_in_a_pty(cwd, config, state, base_url, b"", vec!["PARROT".into()])
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    // The answer was rendered (the scrollback ends up in the pty's output).
    assert!(run.screen.contains("PARROT"), "screen: {:?}", run.screen);
    // Every frame is wrapped in synchronised output, so a frame is never torn
    // (`ESC[?2026h` … `ESC[?2026l`). Terminals that do not know the pair ignore
    // it, so nothing has to probe for it.
    assert!(
        run.screen.contains("\u{1b}[?2026h"),
        "frames are not synchronised: {:?}",
        run.screen
    );
    assert!(
        run.screen.contains("\u{1b}[?2026l"),
        "a sync frame is never closed: {:?}",
        run.screen
    );
    // The banner no longer carries the window: the startup answer is silent
    // (it is still in `state`, for anyone who asks).
    assert!(
        !run.screen.contains("window 262144 tokens"),
        "the window is not ambient in the banner; screen: {:?}",
        run.screen
    );
    // Herdr's status: exactly one record per change, in the only form this
    // Herdr accepts (`state=<word>` and nothing else).
    assert_eq!(
        herdr_records(&run.screen),
        vec![
            "\u{1b}]7501;state=working\u{1b}\\".to_string(),
            "\u{1b}]7501;state=done\u{1b}\\".to_string(),
        ],
        "screen: {:?}",
        run.screen
    );

    // The log is the same as any other session's: a `say` from the TUI, the
    // answer, the turn, and the end.
    let text = std::fs::read_to_string(run.session_dir.join("events.jsonl")).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let types: Vec<&str> = events
        .iter()
        .map(|event| event["type"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(types.first(), Some(&"session_started"), "{types:?}");
    assert_eq!(
        types.iter().filter(|t| **t == "user").count(),
        1,
        "{types:?}"
    );
    let user = events
        .iter()
        .find(|event| event["type"] == "user")
        .expect("a user event");
    assert_eq!(user["text"], "hello there");
    assert_eq!(user["from"], "tui", "the message came from the TUI");
    assert!(types.contains(&"assistant"), "{types:?}");
    assert!(types.contains(&"assistant"), "{types:?}");
    assert!(types.contains(&"turn_ended"), "{types:?}");
    assert_eq!(types.last(), Some(&"session_ended"), "{types:?}");
    let ended = events
        .iter()
        .find(|event| event["type"] == "turn_ended")
        .unwrap();
    assert_eq!(ended["outcome"], "done");
    let verb = events
        .iter()
        .find(|event| event["type"] == "verb")
        .expect("the say was logged as a verb");
    assert_eq!(verb["from"], "tui");
    assert_eq!(verb["name"], "say");
}

/// A markdown answer arrives rendered: the `##`, `**` and `|---|` never reach
/// the screen, and the answer lands in the terminal's scrollback.
#[tokio::test]
async fn the_tui_renders_a_markdown_answer_and_commits_it() {
    let answer = "## Files\n\nSome **bold** words and a `code` span.\n\n\
                  | a | b |\n|---|---|\n| 1 | 2 |\n\n```sh\ncargo test\n```\n";
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn(answer, 0)))]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();
    let state = temp.path().join("state");
    let base_url = mock.base_url();
    let run = tokio::task::spawn_blocking(move || {
        drive_in_a_pty(
            cwd,
            config,
            state,
            base_url,
            b"",
            vec!["cargo test".into(), "Files".into()],
        )
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    for markdown in ["## Files", "**bold**", "|---", "```"] {
        assert!(
            !run.screen.contains(markdown),
            "raw markdown {markdown:?} reached the screen: {:?}",
            run.screen
        );
    }
    // The plain words are there, and the code block keeps its line. The pty
    // stream interleaves escape sequences with the text, so the assertion is
    // on the stripped text (a reader sees the stripped text).
    let text = plain(&run.screen);
    // A differential repaint only writes changed cells, so spaces that were
    // already on screen are absent from the byte stream. Compare words.
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(compact.contains("Someboldwords"), "{text:?}");
    assert!(compact.contains("cargotest"), "{text:?}");
    // A done turn ends on its prose: no check mark line.
    // (the state row's idle text is checked by the snapshot tests; the pty's
    // repaint diff only rewrites what changed, so it need not appear here)
}

/// Idle writes nothing: after the turn ends, the TUI writes nothing for two
/// seconds (no spinner, no elapsed tick). A constant-rate repaint would fail it.
#[tokio::test]
async fn an_idle_tui_writes_nothing_for_two_seconds() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("DONE", 0)))]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();
    let state = temp.path().join("state");
    let base_url = mock.base_url();
    let quiet = tokio::task::spawn_blocking(move || {
        drive_and_count_idle_bytes(cwd, config, state, base_url)
    })
    .await
    .expect("the pty driver");
    assert_eq!(quiet.code, 0, "{quiet:?}");
    assert_eq!(
        quiet.bytes_while_idle, 0,
        "the idle TUI wrote {} bytes in two seconds",
        quiet.bytes_while_idle
    );
}
/// Ctrl-C with nothing running is a no-op: it must not stop the next turn at
/// its first boundary (the verb is dropped in idle, not queued).
#[tokio::test]
async fn a_ctrl_c_before_the_first_message_does_not_stop_the_turn() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("PARROT", 0)))]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();
    let state = temp.path().join("state");
    let base_url = mock.base_url();
    let run = tokio::task::spawn_blocking(move || {
        drive_in_a_pty(cwd, config, state, base_url, b"\x03", vec!["PARROT".into()])
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    let text = std::fs::read_to_string(run.session_dir.join("events.jsonl")).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let stopped = events
        .iter()
        .any(|event| event["type"] == "turn_ended" && event["outcome"] == "stopped");
    assert!(!stopped, "the Ctrl-C stopped the first turn: {text}");
    let done = events
        .iter()
        .find(|event| event["type"] == "turn_ended")
        .expect("a turn ended");
    assert_eq!(done["outcome"], "done", "{text}");
    // The stop verb was still logged: it is a verb like any other.
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "verb" && event["name"] == "stop"),
        "{text}"
    );
}

/// N5: a panic while the TUI owns the terminal must leave the terminal usable.
/// `KELIR_TEST_PANIC` panics right after raw mode is on, so this drives the
/// real panic path: the hook has to turn bracketed paste off, show the cursor,
/// leave the alternate screen (ratatui's own hook) and drop raw mode.
#[tokio::test]
async fn a_panic_in_the_tui_restores_the_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();
    let state = temp.path().join("state");
    let (screen, code) = tokio::task::spawn_blocking(move || panic_in_a_pty(cwd, config, state))
        .await
        .expect("the pty driver");

    // The TUI panic hook restores the terminal before the panic is printed.
    // `cfg!(debug_assertions)` distinguishes the test build (`panic =
    // "unwind"`, session ends cleanly) from the release binary (`panic =
    // "abort"`, non-zero exit). Both must leave the terminal usable (#23).
    if cfg!(debug_assertions) {
        assert_eq!(
            code, 0,
            "a panic must not hang or crash the session: {screen:?}"
        );
    } else {
        assert_ne!(code, 0, "a release panic must abort: {screen:?}");
    }
    let message = screen
        .find("KELIR_TEST_PANIC: injected panic")
        .unwrap_or_else(|| panic!("no panic message: {screen:?}"));
    // The teardown ran: bracketed paste was turned off after being turned on,
    // before the message was printed. (Cursor visibility is a WinAPI call on
    // Windows, so it is not in the byte stream; the live drive in
    // `scripts/live/m2_status_check.sh` checks the shell is usable after an
    // exit, and `a_ctrl_c...`/`the_tui_runs_a_turn_in_a_pty` check the clean
    // path.)
    let paste_on = screen.rfind("\u{1b}[?2004h").expect("paste was never on");
    let paste_off = screen
        .rfind("\u{1b}[?2004l")
        .expect("bracketed paste left on");
    assert!(
        paste_on < paste_off && paste_off < message,
        "the teardown did not run before the panic message: {screen:?}"
    );
}
