//! tui pty images tests: a_csi_u_shift_enter_adds_a_new, a_pasted_image_path_becomes_a_, a_typed_image_path_is_a_chip_i.
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::{Mock, Reply, sse, text_turn};

use super::tui_output::plain;
use super::tui_pty::{PtyInput, drive_in_a_pty, drive_pty_in_idle_or_turn, tiny_png};

/// A bracketed paste of a PNG path (which is what a terminal sends for a drag
/// and drop or a clipboard path) becomes a chip, the prompt carries the
/// pointer, and no image bytes are in it.
#[tokio::test]
async fn a_pasted_image_path_becomes_a_pointer_in_the_prompt() {
    let answer = "described";
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn(answer, 0)))]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let image = cwd.join("shot.png");
    std::fs::write(&image, tiny_png()).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();
    let state = temp.path().join("state");
    let base_url = mock.base_url();
    // A quoted path, as a Windows drag and drop (or a clipboard paste of a
    // file) puts on the input line. It is typed as keystrokes here: crossterm
    // 0.29 only decodes `ESC[200~` on Unix, so on Windows a bracketed paste
    // arrives as characters. The submit-time path scan is what makes both
    // platforms a pointer (`App::absorb_image_paths`).
    let paste = format!("\"{}\"", image.display());
    let run = tokio::task::spawn_blocking(move || {
        drive_in_a_pty(cwd, config, state, base_url, paste.as_bytes(), vec![])
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    // The saved file is in the session's attachments, unmodified.
    let log = std::fs::read_to_string(run.session_dir.join("events.jsonl")).unwrap();
    let attachments = run.session_dir.join("attachments");
    if !attachments.is_dir() {
        panic!(
            "no attachments dir.
log: {log}
screen: {:?}",
            run.screen
        );
    }
    let files: Vec<_> = std::fs::read_dir(&attachments)
        .expect("an attachments dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(std::fs::read(&files[0]).unwrap(), tiny_png());
    // The `user` event carries the pointer, not the path and not the bytes.
    let log = std::fs::read_to_string(run.session_dir.join("events.jsonl")).unwrap();
    let user = log
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|event| event["type"] == "user")
        .expect("a user event");
    let text = user["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("[image: attachments/1-shot.png"),
        "the prompt carries the pointer: {text:?}"
    );
    assert!(
        text.contains("hello there"),
        "the typed text is still there: {text:?}"
    );
    assert!(!text.contains("shot.png\""), "no raw path: {text:?}");
    assert!(
        !text.contains("iVBOR"),
        "no image bytes in the prompt: {text:?}"
    );
}

/// The Windows paste path end to end, in a pty: the kernel delivers the pasted
/// path as a stream of key events there too, so the chip has to appear before
/// Enter — the review's finding was that on Windows it appeared only at submit,
/// as raw path text (`S8`).
#[tokio::test]
async fn a_typed_image_path_is_a_chip_in_the_editor_before_it_is_sent() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("PARROT", 0)))]).await;
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let image = cwd.join("shot.png");
    std::fs::write(&image, tiny_png()).unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"openrouter\"\nmodel = \"test/model\"\n",
    )
    .unwrap();
    let state = temp.path().join("state");
    let base_url = mock.base_url();
    let typed = image.display().to_string();
    let run = tokio::task::spawn_blocking(move || {
        drive_in_a_pty(cwd, config, state, base_url, typed.as_bytes(), vec![])
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    // The screen showed the chip (the path itself was replaced), which is what
    // a Backspace then removes whole; the submit-time scan is no longer the only
    // thing that makes the pointer.
    let plain = plain(&run.screen);
    assert!(plain.contains("[image 1]"), "the chip was drawn: {plain:?}");
    let user = std::fs::read_to_string(run.session_dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|event| event["type"] == "user")
        .expect("a user event");
    let text = user["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("[image: attachments/1-shot.png"),
        "the prompt carries the pointer once: {text:?}"
    );
    assert!(!text.contains("iVBOR"), "no bytes: {text:?}");
}

#[tokio::test]
async fn a_csi_u_shift_enter_adds_a_newline_instead_of_submitting() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("OK", 0)))]).await;
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
    // "first line" + Shift-Enter + "second line", then a plain Enter to
    // submit the whole thing as one message.
    let message = b"first line\x1b[13;2usecond line";
    let run = tokio::task::spawn_blocking(move || {
        drive_pty_in_idle_or_turn(
            cwd,
            config,
            state,
            base_url,
            Some(message),
            b"",
            vec!["OK".into()],
            PtyInput {
                submit: b"\r",
                message_echo: "first line",
            },
        )
    })
    .await
    .expect("the pty driver");

    assert_eq!(run.code, 0, "screen: {:?}", run.screen);
    // The TUI negotiated kitty keyboard mode on startup (this is what makes
    // Herdr forward Shift-Enter instead of flattening it to a bare CR).
    // On Windows crossterm's push is a no-op (no ANSI keyboard protocol).
    #[cfg(not(windows))]
    assert!(
        run.screen.contains("\u{1b}[>1u"),
        "no kitty keyboard push on startup: {:?}",
        run.screen
    );
    let text = std::fs::read_to_string(run.session_dir.join("events.jsonl")).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let user = events
        .iter()
        .find(|event| event["type"] == "user")
        .expect("a user event");
    assert_eq!(user["text"], "first line\nsecond line");
}
