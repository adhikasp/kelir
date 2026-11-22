//! tui2 review tests: adversarial probes of the markdown renderer and the TUI
//! app model (no process, no network). A failing test here is a finding; the
//! report names it.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
use std::time::{Duration, Instant};

use kelir::markdown::{self, Layout, Row, Stream};
use kelir::tui::App;
use serde_json::json;
use unicode_width::UnicodeWidthStr;

fn app(width: usize) -> App {
    let (to_tui, _events) = tokio::sync::mpsc::unbounded_channel();
    let (verbs, _incoming) = tokio::sync::mpsc::unbounded_channel();
    App::new(width, 8, to_tui, verbs, String::new())
}

fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn settled(command: &str) -> Ev {
    Ev::ToolSettled {
        name: "bash".into(),
        input: json!({"command": command}),
        outcome: "ok".into(),
        exit_code: Some(0),
        elapsed_ms: 100,
        bytes: 4,
        lines: 1,
        spilled: false,
        out_path: String::new(),
        tail: String::new(),
    }
}

fn texts(rows: &[Row]) -> Vec<String> {
    rows.iter().map(|r| r.text()).collect()
}

/// History order: calls that ran before a prose message must be written to the
/// scrollback before that message, never after it.
#[test]
fn tool_lines_land_in_the_scrollback_before_the_prose_that_followed_them() {
    let mut app = app(80);
    app.push(settled("git --version"));
    app.push(settled("python --version"));
    app.push(Ev::Delta("All good, both are installed.".into()));
    app.push(Ev::Assistant {
        text: "All good, both are installed.".into(),
    });
    let scroll: Vec<String> = app.take_scroll().iter().map(line_text).collect();
    let tool = scroll.iter().position(|l| l.contains("git --version"));
    let prose = scroll.iter().position(|l| l.contains("All good"));
    assert!(
        tool.is_some() && prose.is_some(),
        "both reach the scrollback: {scroll:?}"
    );
    assert!(
        tool < prose,
        "the calls ran first, so their lines come first: {scroll:?}"
    );
}

/// Four calls are four lines, and they all precede multi-block prose.
#[test]
fn a_folded_group_precedes_multi_block_prose() {
    let mut app = app(80);
    for c in ["a", "b", "c", "d"] {
        app.push(settled(c));
    }
    let prose = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.\n\nFourth.";
    app.push(Ev::Delta(prose.into()));
    app.push(Ev::Assistant { text: prose.into() });
    let scroll: Vec<String> = app.take_scroll().iter().map(line_text).collect();
    let last_call = scroll
        .iter()
        .rposition(|l| l.contains("4 B"))
        .expect("four call lines");
    let first = scroll.iter().position(|l| l.contains("First paragraph"));
    assert!(first.is_some(), "{scroll:?}");
    assert!(last_call < first.unwrap(), "{scroll:?}");
    assert_eq!(
        scroll.iter().filter(|l| l.contains("4 B")).count(),
        4,
        "{scroll:?}"
    );
    assert!(
        !scroll.iter().any(|l| l.contains("calls")),
        "no fold: {scroll:?}"
    );
}

/// Every row of a rendered table (CJK included) has the same display width.
#[test]
fn a_cjk_table_has_rows_of_equal_display_width() {
    let md = "| Name | Role | Notes |\n|---|---|---|\n| Ada | Engineer | writes **code** |\n| 李雷 | 工程师 | 宽字符 wide chars |\n| Grace | Admiral | `compiler` pioneer |\n";
    for width in [100usize, 60, 40, 30] {
        let rows = markdown::render(md, Layout::new(width));
        let widths: Vec<usize> = rows
            .iter()
            .filter(|r| !r.is_empty())
            .map(|r| UnicodeWidthStr::width(r.text().as_str()))
            .collect();
        assert!(!widths.is_empty());
        let max = *widths.iter().max().unwrap();
        assert!(max <= width, "width {width}: {:?}", texts(&rows));
        if width >= 40 {
            assert!(
                widths.iter().all(|w| *w == widths[0]),
                "width {width}: table rows differ in display width {widths:?}: {:#?}",
                texts(&rows)
            );
        }
    }
}

/// No row is ever wider than the layout, at any width, for a nasty corpus.
#[test]
fn nothing_is_wider_than_the_layout_at_any_width() {
    let corpus = [
        "# Heading with a very long title that wraps around the pane several times over",
        "- a\n  - b\n    - c with a long long long line of text to wrap around\n      - d",
        "1. one\n   ```\n   code in a list with a very long line that has to be clipped somewhere\n   ```\n2. two",
        "> quote\n> > nested quote with a very long line that wraps across more than one row at narrow widths",
        "| a | b |\n|---|---|\n| 你好世界你好世界 | 😀😀😀😀 |\n",
        "```\n日本語のとても長い行がここにあります、クリップされるはずです\n```",
        "a `very_long_inline_code_span_without_any_break_opportunity_at_all_1234567890` end",
        "https://example.com/a/very/long/url/that/has/no/spaces/at/all/and/keeps/going/and/going/and/going",
        "---\n\n***\n\n- [ ] task\n- [x] done task with a long long long long long text",
        "e\u{301}e\u{301}e\u{301} zero\u{200b}width\u{200d} joiners 👨‍👩‍👧‍👦 family",
        "<div>html block</div>\n\n<b>raw</b> `<` & > &amp; \\* \\_ escaped",
    ];
    // Width 1 is left out: a double-width cell cannot fit in one column.
    let mut bad = Vec::new();
    for width in [2usize, 3, 5, 10, 20, 29, 30, 31, 40, 60, 80, 120] {
        for (i, doc) in corpus.iter().enumerate() {
            let rows = markdown::render(doc, Layout::new(width));
            for row in &rows {
                if row.width() > width {
                    bad.push(format!(
                        "width {width} doc#{i}: {:?} is {} wide",
                        row.text(),
                        row.width()
                    ));
                    break;
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "rows wider than the layout:
{}",
        bad.join(
            "
"
        )
    );
}

/// Every split point of every corpus document renders the same as one pass.
#[test]
fn streamed_in_any_two_pieces_equals_one_pass() {
    let corpus = [
        "# T\n\npara one\nstill para one\n\n- a\n- b\n\n  continued paragraph of b\n\n- c\n",
        "1. one\n\n2. two\n\n10. ten\n",
        "> quote start\n\n> second quote\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n\ntext right after\n",
        "```rust\nfn x() {}\n\n\nfn y() {}\n```\n\nafter",
        "- item\n\n  ```\n  fenced in item\n\n  with a blank\n  ```\n- next\n",
        "Setext\n======\n\ntext\n---\n",
        "    indented code\n\n    more code\n",
        "line one  \nline two\\\nline three\n",
        "[ref]: https://example.com\n\nSee [ref] and ![img](x.png).\n",
    ];
    for doc in corpus {
        let whole = texts(&markdown::render(doc, Layout::new(50)));
        for (i, _) in doc.char_indices().chain(std::iter::once((doc.len(), ' '))) {
            let mut stream = Stream::new();
            let mut blocks = stream.push(&doc[..i]);
            blocks.extend(stream.push(&doc[i..]));
            blocks.extend(stream.finish());
            let mut rows = Vec::new();
            for b in &markdown::join_blocks(blocks) {
                if !rows.is_empty() {
                    rows.push(Row::default());
                }
                rows.extend(markdown::render_block(b, Layout::new(50)));
            }
            assert_eq!(texts(&rows), whole, "doc {doc:?} split at {i}");
        }
    }
}

/// Fidelity: a list item continued after a blank line stays inside the item
/// (CommonMark), and a loose ordered list keeps counting.
#[test]
fn a_loose_ordered_list_keeps_its_numbers_and_item_continuations_stay_indented() {
    let rows = texts(&markdown::render(
        "1. one\n\n2. two\n\n3. three\n",
        Layout::new(40),
    ));
    let joined = rows.join("\n");
    assert!(joined.contains("2. two"), "{joined}");
    assert!(joined.contains("3. three"), "{joined}");
    let rows = texts(&markdown::render(
        "- item\n\n  continued paragraph of item\n\n- next\n",
        Layout::new(40),
    ));
    let cont = rows
        .iter()
        .find(|r| r.contains("continued paragraph"))
        .unwrap();
    assert!(
        cont.starts_with("  "),
        "the continuation is indented under its bullet: {rows:?}"
    );
}

/// A 500 KB message (1 MB in release is 1.8 s) (one huge paragraph, and one huge fence) streamed in 4 KB
/// deltas must not slow down quadratically through the app model.
#[test]
fn a_one_megabyte_message_streams_in_bounded_time() {
    for (name, make) in [
        (
            "paragraph",
            (|i: usize| format!("word{i} ")) as fn(usize) -> String,
        ),
        ("fence", |i: usize| {
            format!("line {i} of a big code block\n")
        }),
    ] {
        let mut app = app(100);
        let mut total = 0usize;
        let mut i = 0usize;
        let started = Instant::now();
        let mut chunk = String::new();
        if name == "fence" {
            app.push(Ev::Delta("```\n".into()));
        }
        while total < 500_000 {
            chunk.clear();
            while chunk.len() < 4096 {
                chunk.push_str(&make(i));
                i += 1;
            }
            total += chunk.len();
            app.push(Ev::Delta(chunk.clone()));
            // A frame per delta: the worst case the paint loop can ask for.
            let _ = app.live_lines();
            let _ = app.take_scroll();
        }
        let streamed = started.elapsed();
        eprintln!("1MB {name} streamed in {streamed:?}");
        assert!(
            streamed < Duration::from_secs(20),
            "{name}: 1 MB took {streamed:?} (quadratic?)"
        );
    }
}

/// While a long block streams, nothing may vanish: every row of the message so
/// far is either in the scrollback queue or in the live view.
#[test]
fn no_row_is_hidden_while_a_later_block_streams() {
    let mut app = app(60);
    app.push(Ev::Delta(
        "Intro paragraph one.\n\nIntro paragraph two that is a bit longer and wraps around the sixty column pane.\n\n```\nl1\nl2\nl3\nl4\nl5\nl6\n".into(),
    ));
    let seen: Vec<String> = app.cached_rows.iter().map(line_text).collect();
    let joined = seen.join("\n");
    assert!(
        joined.contains("Intro paragraph two") && joined.contains("sixty column pane"),
        "the second paragraph is visible (scrollback or live area) while the fence streams:\n{joined}"
    );
}

// ------------------------------------------------------------ kelir view

use crate::support::*;

/// One bash call runs `kelir view` on a spread of awkward inputs; none panics,
/// each answers with a clear line, and only usable images become `source`s.
#[tokio::test]
async fn kelir_view_handles_awkward_paths_without_panicking() {
    let setup = concat!(
        "mkdir -p sub 'dir.png'; ",
        "printf '\\x89PNG\\r\\n\\x1a\\n\\0\\0\\0\\rIHDR\\0\\0\\0@\\0\\0\\0\\x30\\x08\\x06\\0\\0\\0' > 'my shot é 画.png'; ",
        "cp 'my shot é 画.png' sub/in-sub.png; ",
        "echo just text > notes.txt; echo 'not a png at all' > fake.png; : > empty.png; ",
        "cp 'my shot é 画.png' huge.png; head -c 11000000 /dev/zero >> huge.png; ",
        "ABS=\"$(pwd -W 2>/dev/null || pwd)/my shot é 画.png\"; ",
    );
    let cases: Vec<(&str, &str)> = vec![
        ("missing", "\"$KELIR_BIN\" view nope.png"),
        ("text", "\"$KELIR_BIN\" view notes.txt"),
        ("fake", "\"$KELIR_BIN\" view fake.png"),
        ("huge", "\"$KELIR_BIN\" view huge.png"),
        ("empty", "\"$KELIR_BIN\" view empty.png"),
        ("dir", "\"$KELIR_BIN\" view dir.png"),
        ("unicode", "\"$KELIR_BIN\" view \"my shot é 画.png\""),
        ("absolute", "\"$KELIR_BIN\" view \"$ABS\""),
        ("backslash", "\"$KELIR_BIN\" view \"sub\\in-sub.png\""),
        ("noarg", "\"$KELIR_BIN\" view"),
        (
            "traversal",
            "\"$KELIR_BIN\" view ../../../../../../Windows/win.ini",
        ),
        ("good", "\"$KELIR_BIN\" view sub/in-sub.png"),
    ];
    let script: String = cases
        .iter()
        .map(|(name, cmd)| format!("echo \"== {name}\"; {cmd} 2>&1; echo \"rc=$?\""))
        .collect::<Vec<_>>()
        .join("; ");
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_many",
            "bash",
            &serde_json::json!({ "command": format!("{setup}{script}") }),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let log = session.log();
    let call = log
        .iter()
        .find(|e| e["type"] == "call_settled")
        .expect("settled");
    let out = call.to_string();
    let spilled = log.iter().filter(|e| e["type"] == "call_settled").count();
    eprintln!(
        "---- settled event ({spilled}) ----
{out}
----"
    );
    assert!(!out.contains("panicked"), "{out}");
    let sources: Vec<String> = log
        .iter()
        .filter(|e| e["type"] == "source")
        .map(|e| e["path"].to_string())
        .collect();
    eprintln!("source events: {sources:?}");
    session.end().await;
}

/// STEER-10 req. 3: "Without vision support in the model the CLI says so."
#[tokio::test]
async fn kelir_view_refuses_when_the_model_cannot_take_images() {
    let models = r#"{"data":[{"id":"test/model","context_length":262144,"supported_parameters":["tools"],"capabilities":{"vision":false}}]}"#;
    let setup = "printf '\\x89PNG\\r\\n\\x1a\\n\\0\\0\\0\\rIHDR\\0\\0\\0@\\0\\0\\0\\x30\\x08\\x06\\0\\0\\0' > ok.png; ";
    let mock = Mock::start_with_models(
        vec![
            Reply::Sse(sse(&tool_turn(
                "call_v",
                "bash",
                &serde_json::json!({"command": format!("{setup}\"$KELIR_BIN\" view ok.png 2>&1; echo rc=$?")}),
            ))),
            Reply::Sse(sse(&text_turn("done", 0))),
        ],
        ModelsAnswer::ok(models),
    )
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let log = session.log();
    let settled = log
        .iter()
        .find(|e| e["type"] == "call_settled")
        .expect("settled");
    let result = settled["result"].as_str().unwrap_or_default();
    assert!(
        result.contains("cannot take images") || result.contains("vision"),
        "{result}"
    );
    assert!(
        !log.iter().any(|e| e["type"] == "source"),
        "nothing queued for a text-only model"
    );
    session.end().await;
}

/// A file that merely carries an image extension is not an image: queueing it
/// would put garbage in the request, and the provider's 400 would repeat on
/// every later request (the source event stays in the log).
#[tokio::test]
async fn kelir_view_refuses_a_file_that_is_not_really_an_image() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&tool_turn(
            "call_v",
            "bash",
            &serde_json::json!({"command": "echo 'not a png' > fake.png; : > empty.png; \"$KELIR_BIN\" view fake.png; echo rc=$?; \"$KELIR_BIN\" view empty.png; echo rc=$?"}),
        ))),
        Reply::Sse(sse(&text_turn("done", 0))),
    ])
    .await;
    let mut session = LiveSession::start(&mock, &["run", "--keep", "--json", "go"], "").await;
    session.wait_for("turn_ended").await;
    let sources = session
        .log()
        .iter()
        .filter(|e| e["type"] == "source")
        .count();
    assert_eq!(sources, 0, "garbage was queued as an image");
    session.end().await;
}

/// STEER-10 req. 3: "The TUI ... removes [the chip] on backspace." The real
/// Backspace key must remove the whole chip (and its file), not erase its
/// label one character at a time.
#[test]
fn backspace_removes_a_pasted_image_chip_whole() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(80);
    app.set_session_dir(dir.path().to_path_buf());
    let image = dir.path().join("shot.png");
    // A real PNG header: the chip path sniffs the bytes now, so a text file
    // with an image name is not a chip (review N-chip).
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&64u32.to_be_bytes());
    png.extend_from_slice(&48u32.to_be_bytes());
    png.extend_from_slice(&[8, 6, 0, 0, 0]);
    std::fs::write(&image, &png).unwrap();
    app.paste(&image.display().to_string());
    assert_eq!(app.editor.chips().len(), 1, "the paste became a chip");
    for _ in 0..3 {
        if let Some(press) =
            kelir::tui::translate(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
        {
            app.press(press);
        }
    }
    assert_eq!(app.editor.chips().len(), 0, "backspace removed the chip");
    assert_eq!(app.editor.text(), "", "no mangled label left behind");
    assert_eq!(
        dir.path().join("attachments").read_dir().unwrap().count(),
        0,
        "its file went too"
    );
}

/// STEER-10 req. 1: `paused` and `error` are states of the state row. After the
/// turn ends in either, the row must say so (and what to do), not go back to
/// `idle · ready` the same instant.
#[test]
fn the_state_row_keeps_paused_and_error_after_the_turn_ends() {
    use kelir::activity::Kind;
    let mut a = app(80);
    a.push(Ev::Paused { steps: 2 });
    a.push(Ev::TurnEnded {
        outcome: "paused".into(),
        final_text: String::new(),
    });
    assert_eq!(
        a.activity.kind,
        Kind::Paused,
        "paused stays on the state row"
    );
    let mut b = app(80);
    b.push(Ev::Error("http 401: bad key".into()));
    b.push(Ev::TurnEnded {
        outcome: "error".into(),
        final_text: String::new(),
    });
    assert_eq!(b.activity.kind, Kind::Error, "error stays on the state row");
}
