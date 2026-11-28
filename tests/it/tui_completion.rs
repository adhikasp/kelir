//! The `/` completion popup as a person drives it: what opens it, what it
//! lists, and what each key does. The ranking itself has unit tests in
//! `src/tui/complete.rs`; this file is the behaviour around them.
#![allow(clippy::pedantic, clippy::nursery)]

use super::tui_output::{render_into, snapshot};
use kelir::tui::complete::SkillInfo;
use kelir::tui::editor::Key;
use kelir::tui::{App, Press};

/// An app holding three discovered skills, the way a session hands them over.
fn app() -> App {
    let (to_tui, _events) = tokio::sync::mpsc::unbounded_channel();
    let (verbs, _incoming) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(60, 20, to_tui, verbs, String::new());
    app.set_skills(vec![
        SkillInfo {
            name: "sift".into(),
            about: "read the tail of a log".into(),
            path: "/skills/sift/SKILL.md".into(),
        },
        SkillInfo {
            name: "monthly-finance-close".into(),
            about: "close the month".into(),
            path: "/skills/mfc/SKILL.md".into(),
        },
        SkillInfo {
            name: "kelir".into(),
            about: "operate kelir itself".into(),
            path: String::new(),
        },
    ]);
    app
}

/// Type text a key at a time, as a terminal delivers it.
fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.press(Press::Edit(Key::Char(c)));
    }
}

fn labels(app: &App) -> Vec<String> {
    app.completion_rows()
        .into_iter()
        .map(|(label, _, _)| label)
        .collect()
}

fn line(app: &App) -> String {
    app.editor.text()
}

/// What Tab types for a catalogue row, derived from the label alone, so the
/// assertion cannot drift with the code it checks.
fn insert_for(label: &str) -> String {
    let words: Vec<&str> = label.split_whitespace().collect();
    match words.as_slice() {
        ["/skill", name, ..] if !name.starts_with('<') => format!("/skill {name} "),
        [command, ..] => format!("{command} "),
        [] => String::new(),
    }
}

// --------------------------------------------------------------------- rows

#[test]
fn a_bare_slash_lists_every_command_then_every_skill() {
    let mut app = app();
    type_text(&mut app, "/");
    let labels = labels(&app);
    assert!(!labels.is_empty(), "a bare `/` opens the popup");
    // Catalogue order with no query: the commands in `/help` order, then the
    // skills in discovery order.
    assert_eq!(labels.first().map(String::as_str), Some("/stop [reason]"));
    assert!(labels.contains(&"/skill <name>".to_string()), "{labels:?}");
    assert!(labels.contains(&"/quit".to_string()), "{labels:?}");
    // One row per skill, plus the `/skill` command itself — told apart by kind,
    // not by the label's shape.
    let kinds: Vec<&'static str> = app
        .completion_rows()
        .into_iter()
        .map(|(_, _, kind)| kind)
        .collect();
    assert_eq!(
        kinds.iter().filter(|kind| **kind == "skill").count(),
        3,
        "{labels:?}"
    );
    assert_eq!(
        kinds.iter().filter(|kind| **kind == "command").count(),
        labels.len() - 3
    );
}

#[test]
fn a_query_ranks_the_matches_best_first() {
    let mut app = app();
    type_text(&mut app, "/mod");
    let found = labels(&app);
    assert_eq!(
        found.first().map(String::as_str),
        Some("/model <p/m>"),
        "{found:?}"
    );
    assert!(
        !found.contains(&"/stop [reason]".to_string()),
        "`mod` does not match `/stop`: {found:?}"
    );
    // A subsequence still finds it: `cmp` → `/compact`.
    let mut app = self::app();
    type_text(&mut app, "/cmp");
    assert_eq!(
        labels(&app).first().map(String::as_str),
        Some("/compact [focus]"),
        "{:?}",
        labels(&app)
    );
    // A skill's initials work too: `mfc` → `monthly-finance-close`.
    let mut app = self::app();
    type_text(&mut app, "/mfc");
    assert_eq!(
        labels(&app).first().map(String::as_str),
        Some("/skill monthly-finance-close"),
        "{:?}",
        labels(&app)
    );
}

#[test]
fn a_query_that_matches_nothing_closes_the_popup() {
    let mut app = app();
    type_text(&mut app, "/zzzz");
    assert!(app.completion().is_none(), "no empty popup");
    // Backing out of the query brings the list back.
    for _ in 0..4 {
        app.press(Press::Edit(Key::Backspace));
    }
    type_text(&mut app, "st");
    assert_eq!(
        labels(&app).first().map(String::as_str),
        Some("/stop [reason]")
    );
}

// ---------------------------------------------------------------------- tab

#[test]
fn tab_types_the_highlighted_row() {
    let mut app = app();
    type_text(&mut app, "/sto");
    app.press(Press::Complete);
    // The name and a space, never the usage's placeholder: the argument stays
    // the person's to type.
    assert_eq!(line(&app), "/stop ");
    assert!(
        app.completion().is_none(),
        "a committed command closes the list"
    );

    let mut app = self::app();
    type_text(&mut app, "/mod");
    app.press(Press::Complete);
    assert_eq!(line(&app), "/model ");
    type_text(&mut app, "a/b");
    assert_eq!(line(&app), "/model a/b");
}

#[test]
fn tab_completes_a_skill_as_a_whole_command() {
    let mut app = app();
    type_text(&mut app, "/sif");
    app.press(Press::Complete);
    assert_eq!(line(&app), "/skill sift ");
    assert!(app.completion().is_none());
}

// ------------------------------------------------------------------- arrows

#[test]
fn the_arrows_walk_the_list_and_wrap() {
    let mut app = app();
    type_text(&mut app, "/s");
    let rows = labels(&app);
    assert!(rows.len() > 2, "{rows:?}");
    app.press(Press::Edit(Key::Down));
    app.press(Press::Complete);
    assert_eq!(line(&app), insert_for(&rows[1]), "down chose row two");

    // Up from the first row wraps to the last: a short list is a ring.
    let mut app = self::app();
    type_text(&mut app, "/s");
    let rows = labels(&app);
    app.press(Press::Edit(Key::Up));
    app.press(Press::Complete);
    assert_eq!(line(&app), insert_for(&rows[rows.len() - 1]), "up wrapped");
}

#[test]
fn tab_and_esc_are_ordinary_keys_without_a_popup() {
    let mut app = app();
    app.press(Press::Complete);
    assert_eq!(line(&app), "", "Tab types nothing with no popup");
    // And with nothing running, Esc does nothing at all.
    type_text(&mut app, "a message");
    app.press(Press::Stop);
    assert_eq!(line(&app), "a message", "Esc never edits the line");
}

#[test]
fn esc_closes_the_popup_and_leaves_the_line_alone() {
    let mut app = app();
    type_text(&mut app, "/s");
    assert!(app.completion().is_some());
    app.press(Press::Stop);
    assert!(app.completion().is_none(), "Esc closed the popup");
    assert_eq!(line(&app), "/s", "Esc did not touch the line");
    // Down after Esc scrolls the transcript, not a list that is gone: the popup
    // does not hold the key any more.
    app.press(Press::Edit(Key::Down));
    assert!(app.completion().is_none());
}

// ------------------------------------------------------------------ context

#[test]
fn the_skill_argument_lists_skill_names_only() {
    let mut app = app();
    type_text(&mut app, "/skill ");
    assert_eq!(
        labels(&app),
        vec![
            "/skill sift".to_string(),
            "/skill monthly-finance-close".to_string(),
            "/skill kelir".to_string()
        ],
        "only skills, in discovery order"
    );
    type_text(&mut app, "si");
    app.press(Press::Complete);
    // The command is already typed, so only the name goes in.
    assert_eq!(line(&app), "/skill sift ");
    let parsed = kelir::tui::commands::parse(line(&app).trim_end());
    assert!(
        matches!(
            parsed,
            Ok(kelir::tui::commands::Parsed::Action(
                kelir::tui::commands::Action::Skill { .. }
            ))
        ),
        "the completed line parses: {parsed:?}"
    );
}

#[test]
fn a_second_argument_is_not_a_name() {
    let mut app = app();
    type_text(&mut app, "/skill sift ");
    assert!(app.completion().is_none(), "the name is already given");
    // Nor is another command's argument a command.
    let mut app = self::app();
    type_text(&mut app, "/model ");
    assert!(app.completion().is_none());
}

#[test]
fn only_a_command_in_the_first_column_is_completed() {
    let mut app = app();
    type_text(&mut app, "look in /st");
    assert!(app.completion().is_none(), "a path is not a command");
    // Nor a second line: a command is the first line whole.
    let mut app = self::app();
    type_text(&mut app, "hello");
    app.press(Press::Edit(Key::Newline));
    type_text(&mut app, "/st");
    assert!(app.completion().is_none());
}

// ------------------------------------------------------------------- screen

#[test]
fn the_popup_takes_rows_from_the_transcript_above_the_editor() {
    let mut app = app();
    for n in 0..10 {
        app.push_text(&format!("transcript line {n}"));
    }
    let before = render_into(&mut app, 60, 20);
    type_text(&mut app, "/");
    let after = render_into(&mut app, 60, 20);
    // The screen keeps its size and the editor its row: the popup ate
    // transcript rows instead of pushing the fixed rows about.
    assert_eq!(before.lines().count(), after.lines().count());
    assert!(after.contains("› /"), "the editor is still there: {after}");
    assert!(after.contains("/stop [reason]"), "{after}");
    assert!(
        after.contains(&format!(
            "{}{}",
            kelir::tui::COMPLETION_MARK,
            "/stop [reason]"
        )) || after
            .lines()
            .any(|l| l.starts_with(kelir::tui::COMPLETION_MARK)),
        "the highlight is marked: {after}"
    );
    assert!(
        !after.contains("transcript line 0"),
        "the oldest rows gave up the space: {after}"
    );
    // Closing the popup and clearing the line gives the rows back.
    app.press(Press::Stop);
    app.press(Press::Edit(Key::Clear));
    assert_eq!(render_into(&mut app, 60, 20), before);
}

/// The popup rows a rendered screen shows: the ones carrying the highlight
/// mark, or starting a row like the list does.
fn popup_lines(screen: &str) -> Vec<&str> {
    screen
        .lines()
        .filter(|l| l.starts_with("  /") || l.starts_with(kelir::tui::COMPLETION_MARK))
        .collect()
}

#[test]
fn the_popup_is_windowed_and_marks_one_row() {
    let mut app = app();
    type_text(&mut app, "/");
    let screen = snapshot(&mut app);
    assert!(app.completion_rows().len() > 8, "more rows than the window");
    assert_eq!(
        popup_lines(&screen).len(),
        kelir::tui::POPUP_ROWS as usize,
        "the list is windowed: {screen}"
    );
    assert_eq!(
        screen.matches(kelir::tui::COMPLETION_MARK).count(),
        1,
        "exactly one highlighted row: {screen}"
    );
    // Walking past the window keeps the highlight on screen: the window
    // follows it instead of scrolling the mark away.
    for _ in 0..10 {
        app.press(Press::Edit(Key::Down));
    }
    let screen = snapshot(&mut app);
    assert_eq!(
        screen.matches(kelir::tui::COMPLETION_MARK).count(),
        1,
        "the window followed the highlight: {screen}"
    );
    assert_eq!(popup_lines(&screen).len(), kelir::tui::POPUP_ROWS as usize);
}

// ---------------------------------------------------------------- dispatch

/// `/skill <name>` for a skill the session *discovered* must carry the path
/// the catalogue holds: the verb has no discovery of its own, so a bare name
/// is a usage error and the keystroke would be lost.
#[tokio::test]
async fn a_discovered_skill_is_sent_with_the_path_the_list_showed() {
    let (to_tui, _events) = tokio::sync::mpsc::unbounded_channel();
    let (verbs, mut incoming) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(60, 20, to_tui, verbs, String::new());
    app.set_skills(vec![SkillInfo {
        name: "sift".into(),
        about: "read the tail of a log".into(),
        path: "/skills/sift/SKILL.md".into(),
    }]);

    type_text(&mut app, "/skill sif");
    app.press(Press::Complete);
    app.press(Press::Edit(Key::Enter));

    let posted = incoming.try_recv().expect("the verb was sent");
    assert_eq!(posted.request.verb, "skills");
    assert_eq!(posted.request.args["name"], "sift");
    assert_eq!(
        posted.request.args["path"], "/skills/sift/SKILL.md",
        "the catalogue's path travels with the name: {}",
        posted.request.args
    );
}

/// A builtin has no file: its body goes as `text`, and no path is invented.
#[tokio::test]
async fn a_builtin_skill_is_sent_as_its_text() {
    let (to_tui, _events) = tokio::sync::mpsc::unbounded_channel();
    let (verbs, mut incoming) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(60, 20, to_tui, verbs, String::new());
    app.set_skills(vec![SkillInfo {
        name: "kelir".into(),
        about: "operate kelir itself".into(),
        path: String::new(),
    }]);

    type_text(&mut app, "/skill kelir");
    app.press(Press::Edit(Key::Enter));

    let posted = incoming.try_recv().expect("the verb was sent");
    assert_eq!(posted.request.verb, "skills");
    assert_eq!(posted.request.args["name"], "kelir");
    assert!(posted.request.args["path"].is_null(), "no invented path");
    assert!(
        posted.request.args["text"]
            .as_str()
            .is_some_and(|t| !t.is_empty()),
        "the built-in body goes with the verb: {}",
        posted.request.args
    );
}
