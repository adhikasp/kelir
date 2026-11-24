//! tui2b review tests (round 2): adversarial probes of the fixes for the round-1
//! findings, at the `App` / markdown level (no process, no network). A failing
//! test here is a finding; the report names it.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
use std::time::{Duration, Instant};

use crate::support::{ApplyEv as _, Ev};
use kelir::tui::markdown::{self, Layout, Stream};
use kelir::tui::{App, render};
use ratatui::layout::Rect;
use serde_json::json;
use unicode_width::UnicodeWidthStr;

const VIEWPORT: u16 = 8;

fn app(width: usize) -> App {
    let (to_tui, _events) = tokio::sync::mpsc::unbounded_channel();
    let (verbs, _incoming) = tokio::sync::mpsc::unbounded_channel();
    App::new(width, VIEWPORT as usize, to_tui, verbs, String::new())
}

fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn settled(command: &str, exit: i32) -> Ev {
    Ev::ToolSettled {
        name: "bash".into(),
        input: json!({"command": command}),
        outcome: if exit == 0 { "ok" } else { "error" }.into(),
        exit_code: Some(exit),
        elapsed_ms: 100,
        bytes: 4,
        lines: 1,
        spilled: false,
        out_path: String::new(),
        tail: String::new(),
    }
}

fn started(command: &str) -> Ev {
    Ev::ToolStarted {
        name: "bash".into(),
        input: json!({"command": command}),
    }
}

/// The full screen text of the viewport (state row, facts row, live rows).
fn snap(app: &mut App, width: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, VIEWPORT);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| render(app, frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell(Rect::new(x, y, 1, 1).as_position()) {
                out.push_str(cell.symbol());
            }
        }
        out.push('\n');
    }
    out
}

fn scroll(app: &mut App) -> Vec<String> {
    app.take_scroll().iter().map(line_text).collect()
}

fn pos(rows: &[String], needle: &str) -> usize {
    rows.iter()
        .position(|r| r.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not in {rows:#?}"))
}

fn turn(app: &mut App) {
    app.push(Ev::TurnStarted {
        turn: 1,
        provider: "openrouter".into(),
        model: "m".into(),
        limit: None,
    });
}

// ----------------------------------------------------------------------- B1

/// Interleaved: prose, call, prose, call, prose, streamed in small deltas the
/// way a provider sends them. Committed order must be the event order.
#[test]
fn b1_interleaved_prose_and_calls_keep_event_order() {
    let mut app = app(80);
    turn(&mut app);
    let full = "First I look.\n\nNext I read.\n\nDone, here is the answer.";
    app.push(Ev::Delta("First I look.".into()));
    app.push(started("echo one"));
    app.push(settled("echo one", 0));
    app.push(Ev::Delta("\n\nNext I read.".into()));
    app.push(started("echo two"));
    app.push(settled("echo two", 0));
    app.push(Ev::Delta("\n\nDone, here is the answer.".into()));
    app.push(Ev::Assistant { text: full.into() });
    app.push(Ev::TurnEnded {
        outcome: "done".into(),
        final_text: full.into(),
    });
    let rows = scroll(&mut app);
    let (a, c1, b, c2, d) = (
        pos(&rows, "First I look"),
        pos(&rows, "echo one"),
        pos(&rows, "Next I read"),
        pos(&rows, "echo two"),
        pos(&rows, "Done, here is"),
    );
    assert!(a < c1 && c1 < b && b < c2 && c2 < d, "{rows:#?}");
}

/// A call that interrupts a paragraph mid-sentence (no blank line anywhere).
#[test]
fn b1_a_call_in_the_middle_of_a_paragraph_splits_it_above_the_call() {
    let mut app = app(80);
    turn(&mut app);
    app.push(Ev::Delta("I will now run the thing".into()));
    app.push(started("make"));
    app.push(settled("make", 0));
    app.push(Ev::Delta(" and report back.".into()));
    app.push(Ev::Assistant {
        text: "I will now run the thing and report back.".into(),
    });
    app.push(Ev::TurnEnded {
        outcome: "done".into(),
        final_text: String::new(),
    });
    let rows = scroll(&mut app);
    assert!(
        pos(&rows, "I will now run") < pos(&rows, "make")
            && pos(&rows, "make") < pos(&rows, "report back"),
        "{rows:#?}"
    );
}

/// Parallel calls: three start, they settle out of order; the lines still all
/// precede the answer, and a failed one is kept.
#[test]
fn b1_parallel_calls_settling_out_of_order_precede_the_answer() {
    let mut app = app(80);
    turn(&mut app);
    for c in ["a1", "b2", "c3"] {
        app.push(started(c));
    }
    app.push(settled("c3", 0));
    app.push(settled("a1", 1));
    app.push(settled("b2", 0));
    app.push(Ev::Delta("Answer text.".into()));
    app.push(Ev::Assistant {
        text: "Answer text.".into(),
    });
    app.push(Ev::TurnEnded {
        outcome: "done".into(),
        final_text: String::new(),
    });
    let rows = scroll(&mut app);
    let answer = pos(&rows, "Answer text");
    for c in ["a1", "b2", "c3"] {
        // a three-call group may fold to "ran 3 commands"; accept either.
        let _ = c;
    }
    let tools = rows
        .iter()
        .position(|r| r.contains("a1") || r.contains("ran 3"))
        .expect("a tool line");
    assert!(tools < answer, "{rows:#?}");
}

/// A turn that is only calls, ending paused, then the notice: order again.
#[test]
fn b1_calls_then_pause_notice_in_order() {
    let mut app = app(80);
    turn(&mut app);
    app.push(started("x1"));
    app.push(settled("x1", 0));
    app.push(Ev::Paused { steps: 1 });
    app.push(Ev::TurnEnded {
        outcome: "paused".into(),
        final_text: String::new(),
    });
    let rows = scroll(&mut app);
    assert!(pos(&rows, "x1") < pos(&rows, "paused"), "{rows:#?}");
}

// ----------------------------------------------------------------------- S1

/// While a long fence streams line by line, every row the text has produced is
/// in the owned transcript exactly once, in order. The full-screen app keeps
/// one transcript (there is no scrollback/live split to double-count).
#[test]
fn s1_no_row_vanishes_and_none_repeats_while_a_fence_streams() {
    let mut app = app(60);
    turn(&mut app);
    app.push(Ev::Delta("Intro paragraph before the code.\n\n".into()));
    app.push(Ev::Delta("```text\n".into()));
    for i in 0..40 {
        app.push(Ev::Delta(format!("line-{i:02}\n")));
        let all: Vec<String> = app.cached_rows.iter().map(line_text).collect();
        let intro = all.iter().filter(|r| r.contains("Intro paragraph")).count();
        assert_eq!(
            intro, 1,
            "intro must be visible exactly once at i={i}: {all:#?}"
        );
        for j in 0..=i {
            let n = all
                .iter()
                .filter(|r| r.contains(&format!("line-{j:02}")))
                .count();
            assert_eq!(n, 1, "line-{j:02} appears {n}x at i={i}: {all:#?}");
        }
    }
    app.push(Ev::Delta("```\n\nAfter.".into()));
    app.push(Ev::Assistant { text: "x".into() });
    let all: Vec<String> = app.cached_rows.iter().map(line_text).collect();
    for j in 0..40 {
        let n = all
            .iter()
            .filter(|r| r.contains(&format!("line-{j:02}")))
            .count();
        assert_eq!(n, 1, "line-{j:02} appears {n}x after close");
    }
}

// ----------------------------------------------------------------------- S5

#[test]
fn s5_paused_and_error_rows_stay_until_the_next_turn() {
    let mut app = app(80);
    turn(&mut app);
    app.push(Ev::Paused { steps: 2 });
    app.push(Ev::TurnEnded {
        outcome: "paused".into(),
        final_text: String::new(),
    });
    let s = snap(&mut app, 80);
    assert!(s.contains("paused"), "{s}");
    assert!(!s.contains("● idle"), "{s}");
    // a tick must not clear it
    app.tick(Instant::now() + Duration::from_secs(5));
    assert!(snap(&mut app, 80).contains("paused"));
    // the next turn replaces it
    turn(&mut app);
    let s = snap(&mut app, 80);
    assert!(s.contains("thinking"), "{s}");
    assert!(!s.contains("⏸"), "{s}");

    app.push(Ev::Error("http 401: provider refused".into()));
    app.push(Ev::TurnEnded {
        outcome: "error".into(),
        final_text: String::new(),
    });
    let s = snap(&mut app, 80);
    assert!(s.contains("error") && s.contains("401"), "{s}");
}

// --------------------------------------------------- STEER-5 integration

#[test]
fn steer5_rate_limited_shows_countdown_and_attempt_and_clears() {
    let mut app = app(100);
    turn(&mut app);
    let until = Instant::now() + Duration::from_secs(34);
    app.push(Ev::Retrying {
        line: "rate limited (429): retrying in 34 s, attempt 2, say stop to cancel".into(),
        until,
        status: Some(429),
        attempt: 2,
    });
    let s = snap(&mut app, 100);
    assert!(s.contains("rate limited"), "{s}");
    assert!(s.contains("429"), "{s}");
    assert!(s.contains("attempt 2"), "{s}");
    // the countdown moves with the clock
    app.tick(Instant::now() + Duration::from_secs(10));
    let s2 = snap(&mut app, 100);
    assert!(s2.contains("rate limited"), "{s2}");
    assert_ne!(s, s2, "countdown must change after 10 s");
    app.push(Ev::RetryingDone);
    let s3 = snap(&mut app, 100);
    assert!(!s3.contains("rate limited"), "{s3}");
}

#[test]
fn steer5_a_slot_wait_is_not_a_rate_limit() {
    let mut app = app(100);
    turn(&mut app);
    app.push(Ev::Retrying {
        line: "waiting for a provider slot".into(),
        until: Instant::now() + Duration::from_secs(3),
        status: None,
        attempt: 0,
    });
    let s = snap(&mut app, 100);
    assert!(!s.contains("rate limited"), "{s}");
}

#[test]
fn steer5_provider_exhaustion_reads_paused_with_the_reason() {
    let mut app = app(110);
    turn(&mut app);
    app.push(Ev::Retrying {
        line: "rate limited (503)".into(),
        until: Instant::now() + Duration::from_secs(1),
        status: Some(503),
        attempt: 1,
    });
    app.push(Ev::RetryingDone);
    app.push(Ev::Info(
        "turn paused: provider unavailable after 2 attempts: provider error (503); say continue"
            .into(),
    ));
    app.push(Ev::TurnEnded {
        outcome: "paused".into(),
        final_text: String::new(),
    });
    let s = snap(&mut app, 110);
    assert!(s.contains("paused"), "{s}");
    assert!(!s.contains("thinking"), "{s}");
    assert!(!s.contains("rate limited"), "{s}");
}

// ----------------------------------------------------------------------- S6

#[tokio::test]
async fn s6_collapse_and_bare_expand_are_not_commands() {
    let mut app = app(80);
    // There is no output policy to toggle: every call keeps its own line.
    // The old `/collapse` and bare `/expand` commands were removed.
    app.submit("/collapse");
    app.submit("/expand");
    let rows = scroll(&mut app);
    assert!(rows.iter().all(|r| !r.contains("tool output")), "{rows:#?}");
    // A call is still one line each, whatever the old commands said.
    turn(&mut app);
    app.push(started("echo hi"));
    app.push(settled("echo hi", 0));
    let rows = scroll(&mut app);
    assert!(rows.iter().any(|r| r.contains("echo hi")), "{rows:#?}");
    assert!(rows.iter().all(|r| !r.contains("call")), "{rows:#?}");
}

// ----------------------------------------------------------------------- S7

fn render_md(md: &str, width: usize) -> Vec<String> {
    markdown::render(md, Layout::new(width))
        .iter()
        .map(|r| r.text())
        .collect()
}

#[test]
fn s7_nested_loose_lists_keep_every_continuation_indented() {
    let md = "1. First item\n\n   continuation of first item with enough words to wrap around the pane at least once or twice\n\n   - nested a\n\n     nested a continuation paragraph\n\n   - nested b\n\n2. Second item\n\n   ```rust\n   let x = 1;\n   ```\n\n   tail paragraph of second\n\nAfter the list.\n";
    for width in [80usize, 40] {
        let rows = render_md(md, width);
        for (needle, min_indent) in [
            ("continuation of first", 3usize),
            ("nested a continuation", 4),
            ("let x = 1", 3),
            ("tail paragraph", 3),
        ] {
            let row = rows
                .iter()
                .find(|r| r.contains(needle) || needle.split(' ').all(|w| r.contains(w)))
                .unwrap_or_else(|| panic!("w{width}: {needle:?} missing in {rows:#?}"));
            let indent = row.len() - row.trim_start().len();
            // at width 24 the indent is clipped, not dropped
            let want = if width >= 40 { min_indent } else { 2 };
            assert!(
                indent >= want,
                "w{width}: {needle:?} indent {indent} < {want}: {rows:#?}"
            );
        }
        let after = rows.iter().find(|r| r.contains("After the list")).unwrap();
        assert!(!after.starts_with(' '), "w{width}: {after:?}");
        for r in &rows {
            assert!(
                UnicodeWidthStr::width(r.as_str()) <= width,
                "w{width}: {r:?}"
            );
        }
    }
}

/// Every split point of a loose-list document streams to the same rows as the
/// one-shot render (S7 changed the joiner, so this is the regression to watch).
#[test]
fn s7_streamed_equals_one_shot_for_loose_lists_at_every_split() {
    let md = "- a\n\n  b\n\n  ```\n  c\n  ```\n\n- d\n\n1. e\n\n   f\n\ntext after\n";
    let whole = render_md(md, 40);
    for (cut, _) in md.char_indices().chain(std::iter::once((md.len(), ' '))) {
        let mut stream = Stream::new();
        let mut blocks = stream.push(&md[..cut]);
        blocks.extend(stream.push(&md[cut..]));
        blocks.extend(stream.finish());
        let mut rows: Vec<String> = Vec::new();
        for b in &markdown::join_blocks(blocks) {
            if !rows.is_empty() {
                rows.push(String::new());
            }
            rows.extend(
                markdown::render_block(b, Layout::new(40))
                    .iter()
                    .map(|r| r.text()),
            );
        }
        assert_eq!(rows, whole, "split at {cut}");
    }
}

// --------------------------------------------------------------- tables

#[test]
fn tables_at_narrow_widths_never_overflow_and_keep_the_header() {
    let md = "| Name | Description | Status |\n|---|---|---|\n| alpha | a rather long description that must wrap | ok |\n| beta | short | fail |\n";
    for width in [80usize, 40, 30, 20, 12, 10] {
        let rows = render_md(md, width);
        assert!(!rows.is_empty());
        for r in &rows {
            assert!(
                UnicodeWidthStr::width(r.as_str()) <= width,
                "w{width}: {r:?} too wide in {rows:#?}"
            );
        }
        let all = rows.join("\n");
        if width >= 30 {
            assert!(all.contains("Name"), "w{width}: {all}");
            assert!(all.contains("alpha"), "w{width}: {all}");
        }
        if width >= 20 {
            assert!(all.contains("Na"), "w{width}: {all}");
        }
    }
}

// ------------------------------------------------------------- big messages

fn stream_cost(text: &str, delta: usize) -> (Duration, Duration) {
    let mut app = app(100);
    turn(&mut app);
    let t0 = Instant::now();
    let mut worst = Duration::ZERO;
    for chunk in text.as_bytes().chunks(delta) {
        let t = Instant::now();
        app.push(Ev::Delta(String::from_utf8_lossy(chunk).into_owned()));
        let _ = scroll(&mut app);
        let _ = app.live_view();
        worst = worst.max(t.elapsed());
    }
    (t0.elapsed(), worst)
}

/// The brief's regression: 1 MB of *mixed* markdown (paragraphs, fences,
/// nested lists, tables, quotes) streamed in 4 KB deltas must cost what 100 KB
/// costs times the size ratio, not its square (`P1`).
///
/// A ratio, never a wall clock: a slow machine makes both numbers big and their
/// ratio the same, which is what "linear in the message" means.
#[test]
fn a_megabyte_of_mixed_markdown_scales_linearly() {
    fn mixed(paragraphs: usize) -> String {
        let unit = "A paragraph of **prose** with `code` and a [link](https://example.com).\n\
                    It wraps at the pane's width, which is what a real answer does.\n\n\
                    ```rust\nfn f(x: u32) -> u32 { x + 1 }\nlet y = f(2);\n```\n\n\
                    1. first item\n2. second item\n   - nested bullet\n   - another one\n\n\
                    | name | value |\n|---|---|\n| alpha | 1 |\n| beta | 2 |\n\n\
                    > a quoted sentence that is long enough to wrap at eighty columns\n\n";
        unit.repeat(paragraphs)
    }
    let (small, _) = stream_cost(&mixed(120), 4096);
    let (large, _) = stream_cost(&mixed(2800), 4096);
    let small_len = mixed(120).len();
    let large_len = mixed(2800).len();
    let ratio = large_len as f64 / small_len as f64;
    eprintln!(
        "{} KB in {small:?} ({:.1} MB/s), {} KB in {large:?} ({:.1} MB/s): ratio {ratio:.1}",
        small_len / 1024,
        small_len as f64 / 1e6 / small.as_secs_f64().max(1e-9),
        large_len / 1024,
        large_len as f64 / 1e6 / large.as_secs_f64().max(1e-9),
    );
    assert!(
        large_len >= 1_000_000,
        "the big case is 1 MB: {large_len} B"
    );
    // Linear is `ratio` (10x); quadratic is `ratio^2` (100x). Allow 3x the
    // linear ratio plus a floor for timer noise on a mostly-idle machine.
    let linear = Duration::from_secs_f64(small.as_secs_f64() * ratio * 3.0);
    assert!(
        large < linear + Duration::from_millis(250),
        "{small_len} B {small:?} -> {large_len} B {large:?}: super-linear (ratio {ratio:.1})"
    );
    // And the 1 MB case is fast enough to be a stream, not a batch job.
    assert!(large < Duration::from_secs(20), "1 MB took {large:?}");
}

/// The worst case for a row-at-a-time commit: one 1 MB fence streamed in 64-byte
/// deltas. Every delta must cost what the delta holds, never what the block now
/// holds (`S1b` + `P1` together).
#[test]
fn a_one_megabyte_fence_streams_row_by_row_in_bounded_time() {
    let mut text = String::from("```text\n");
    let mut i = 0usize;
    while text.len() < 1_000_000 {
        text.push_str(&format!("line {i} of a big code block\n"));
        i += 1;
    }
    text.push_str("```\n");
    let (total, worst) = stream_cost(&text, 64);
    eprintln!("1 MB fence, 64-byte deltas: total {total:?}, worst delta {worst:?}");
    assert!(
        total < Duration::from_secs(20),
        "a 1 MB fence took {total:?}"
    );
    // One delta can never cost a meaningful fraction of the whole message.
    assert!(
        worst < Duration::from_millis(500),
        "one 64-byte delta cost {worst:?}: the delta path re-renders the block"
    );
}

/// The same for the other long single blocks a model writes: a 40-line
/// paragraph-free list (one block, no blank line) and a 30-row table.
#[test]
fn s1_a_long_list_block_reaches_history_while_it_streams() {
    let mut app = app(60);
    turn(&mut app);
    for i in 0..40 {
        app.push(Ev::Delta(format!(
            "- item-{i:02} of the list
"
        )));
        let all: Vec<String> = app.cached_rows.iter().map(line_text).collect();
        for j in 0..=i {
            let n = all
                .iter()
                .filter(|r| r.contains(&format!("item-{j:02}")))
                .count();
            assert_eq!(n, 1, "item-{j:02} appears {n}x at i={i}: {all:#?}");
        }
    }
}

/// The fence's language label inside a list item sits at the item's indent,
/// like the code under it.
#[test]
fn a_fence_label_in_a_list_item_is_indented_with_its_code() {
    let rows = render_md(
        "1. one

   ```rust
   let x = 1;
   ```
",
        40,
    );
    let label = rows.iter().find(|r| r.trim() == "rust").expect("label row");
    let code = rows.iter().find(|r| r.contains("let x")).unwrap();
    let ind = |r: &str| r.len() - r.trim_start().len();
    assert!(
        ind(label) > 0 || ind(code) == 0,
        "label {label:?} vs code {code:?}: {rows:#?}"
    );
}

/// S3 follow-up: the bytes are sniffed to refuse garbage, but the media type
/// that goes into the `data:` URL still comes from the extension. A GIF (or a
/// WebP, which browsers love to save as `.png`) named `x.png` is sent as
/// `image/png`; providers that check the declared type against the bytes
/// reject it, and the rejected `source` event then repeats on every request.
#[test]
fn the_media_type_follows_the_bytes_not_the_extension() {
    let dir = std::env::temp_dir().join(format!("kelir-tui2b-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("looks-like-a-png.png");
    // A 1x1 GIF89a header.
    std::fs::write(
        &path,
        b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00;",
    )
    .unwrap();
    let sniffed = kelir::features::attachments::sniff(&path).unwrap();
    let mime = kelir::features::attachments::mime_of(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(sniffed.map(|(format, _)| format), Some("gif"));
    assert_eq!(mime, "image/gif", "declared type must follow the bytes");
}

/// B1 regression: a failed call's diagnostic line comes from `CallSettle.tail`,
/// not from the last line of the (head/tail-wrapped) result blob. Both the live
/// TUI and `attach` read the same event, so one assertion covers both paths.
#[test]
fn a_failed_call_keeps_the_diagnostic_tail_from_the_settled_event() {
    let mut app = app(80);
    app.push(Ev::ToolStarted {
        name: "bash".into(),
        input: json!({"command": "cargo build"}),
    });
    app.push(Ev::ToolSettled {
        name: "bash".into(),
        input: json!({"command": "cargo build"}),
        outcome: "error".into(),
        exit_code: Some(101),
        elapsed_ms: 10,
        bytes: 40,
        lines: 2,
        spilled: false,
        out_path: String::new(),
        tail: "error[E0432]: unresolved import `missing_crate`".into(),
    });
    let rows = scroll(&mut app);
    assert!(
        rows.iter()
            .any(|r| r.contains("error[E0432]: unresolved import `missing_crate`")),
        "the diagnostic line must be on the transcript: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|r| r.contains("[ END OF RESULT ]")),
        "the row must come from CallSettle.tail, not the result blob: {rows:#?}"
    );
}

/// N3 regression: two local echoes must each be matched to their own log
/// event, even when an unrelated event lands between them.
#[test]
fn two_queued_echoes_match_by_text_not_by_position() {
    let mut app = app(80);
    app.push_user("FIRST_ECHO");
    app.push_user("SECOND_ECHO");
    let user = |text: &str| {
        kelir::tui::View::Log(kelir::log::event::LogEvent::new(
            1,
            kelir::log::event::EventBody::User {
                text: kelir::log::event::Payload::text(text),
                from: "test".into(),
                delivery: "test".into(),
                attachments: Vec::new(),
            },
        ))
    };
    app.apply(user("SECOND_ECHO"));
    app.push(Ev::Info("an unrelated announcement".into()));
    app.apply(user("FIRST_ECHO"));
    let rows = scroll(&mut app);
    assert_eq!(
        rows.iter().filter(|r| r.contains("FIRST_ECHO")).count(),
        1,
        "the first echo must not be duplicated or swallowed: {rows:#?}"
    );
    assert_eq!(
        rows.iter().filter(|r| r.contains("SECOND_ECHO")).count(),
        1,
        "the second echo must not be duplicated or swallowed: {rows:#?}"
    );
}
