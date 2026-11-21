//! bashdesc end-to-end: the model's `description` reaches the human in every
//! view, while the command stays in the event log.
//!
//! The real binary runs against the scripted mock; assertions are on the
//! event log, on the headless progress line, on the request the mock received
//! (schema and frozen prefix) and on the TUI's own line rendering.

mod support;

use kelir::tools_view::CallLine;
use serde_json::json;
use support::*;

const COMMAND: &str = "printf 'audit ok\\n'";
const DESC: &str = "run the lighthouse audit";

fn scripted() -> Vec<Reply> {
    vec![
        Reply::Sse(sse(&tool_turn(
            "call_desc",
            "bash",
            &json!({"command": COMMAND, "description": DESC}),
        ))),
        Reply::Sse(sse(&text_turn("audit done", 80))),
    ]
}

#[tokio::test]
async fn a_description_reaches_headless_output_but_the_command_stays_in_the_log() {
    let mock = Mock::start(scripted()).await;
    let run = run_task(&mock, &["run", "audit the page"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    // The headless progress line names the intent, not the means.
    let lines: Vec<&str> = run
        .stdout
        .lines()
        .filter(|line| line.starts_with("→ bash "))
        .collect();
    assert_eq!(lines.len(), 1, "stdout: {}", run.stdout);
    assert!(
        lines[0].starts_with(&format!("→ bash {DESC} · ok · ")),
        "{}",
        lines[0]
    );
    assert!(!lines[0].contains("printf"), "{}", lines[0]);

    // The log and `/expand` keep the whole call, description *and* command.
    let started = run.one_event("tool_started");
    assert_eq!(started["input"]["description"], DESC);
    assert_eq!(started["input"]["command"], COMMAND);
    let assistant = run.events_of("assistant")[0];
    let args = &assistant["tool_calls"][0]["args"];
    assert_eq!(args["description"], DESC);
    assert_eq!(args["command"], COMMAND);
    let settled = run.one_event("tool_settled");
    assert_eq!(settled["result"], "audit ok\n");
    let out_path = settled["out_path"].as_str().unwrap();
    assert_eq!(
        std::fs::read_to_string(out_path).unwrap(),
        "audit ok\n",
        "/expand reads the full output, not the label"
    );

    // The tool schema offers description beside command, and requires it; the
    // request prefix is the same on both steps.
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let tools = requests[0].body["tools"].as_array().expect("tools");
    for tool in tools {
        let required = tool["function"]["parameters"]["required"]
            .as_array()
            .expect("required");
        assert!(
            required.iter().any(|field| field == "description"),
            "{} must require a description",
            tool["function"]["name"]
        );
    }
    assert_eq!(requests[0].body["tools"], requests[1].body["tools"]);
    let first = requests[0].body["messages"].as_array().unwrap();
    let second = requests[1].body["messages"].as_array().unwrap();
    assert_eq!(&second[..first.len()], first.as_slice());
}

#[tokio::test]
async fn a_description_reaches_the_ndjson_tool_started_event() {
    let mock = Mock::start(scripted()).await;
    let run = run_task(&mock, &["run", "--json", "audit the page"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let lines = run.json_lines();
    let started = lines
        .iter()
        .find(|line| line["type"] == "tool_started")
        .expect("a tool_started NDJSON line");
    assert_eq!(started["name"], "bash");
    assert_eq!(started["input"]["description"], DESC);
    assert_eq!(started["input"]["command"], COMMAND);
}

#[test]
fn edit_and_compact_descriptions_are_the_head_in_every_view() {
    let theme = kelir::theme::Theme::plain();
    for (name, args, desc) in [
        (
            "edit",
            json!({"path": "src/a.rs", "description": "patch the parser"}),
            "patch the parser",
        ),
        (
            "compact",
            json!({"focus": "old history", "description": "trim the transcript"}),
            "trim the transcript",
        ),
    ] {
        let head = kelir::tui::call_head(name, &args);
        assert_eq!(head, desc, "{name} settled head");
        assert_eq!(kelir::tui::call_label(name, &args), desc, "{name} live row");
        // The headless renderer prefers the same description.
        assert_eq!(kelir::tools_view::description(&args).as_deref(), Some(desc));

        let call = CallLine {
            ordinal: 1,
            name: name.into(),
            head,
            outcome: "ok".into(),
            exit_code: Some(0),
            bytes: 0,
            lines: 0,
            spilled: false,
            out_path: String::new(),
            tail: String::new(),
            elapsed: None,
        };
        let rendered: String = call
            .line(theme, 60)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(rendered.starts_with(&format!("  {desc}")), "{rendered:?}");
        assert!(!rendered.contains("src/a.rs"), "{rendered:?}");
        assert!(!rendered.contains("old history"), "{rendered:?}");
    }
}

#[test]
fn a_description_reaches_the_tui_settled_line_and_live_row() {
    use ratatui::style::Color;

    let theme = kelir::theme::Theme::plain();
    let input = json!({"command": "printf 'audit ok\\n'", "description": DESC});
    let head = kelir::tui::call_head("bash", &input);
    assert_eq!(head, DESC);
    assert_eq!(kelir::tui::call_label("bash", &input), DESC);

    let call = CallLine {
        ordinal: 1,
        name: "bash".into(),
        head,
        outcome: "ok".into(),
        exit_code: Some(0),
        bytes: 9,
        lines: 1,
        spilled: false,
        out_path: String::new(),
        tail: String::new(),
        elapsed: None,
    };
    let rendered: String = call
        .line(theme, 60)
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(rendered.starts_with(&format!("  {DESC}")), "{rendered:?}");
    assert!(!rendered.contains("printf"), "{rendered:?}");
    assert!(rendered.ends_with("9 B"), "{rendered:?}");
    assert!(!rendered.contains("ok"), "{rendered:?}");

    // The label is dim, never prose weight; the verdict is still right-aligned.
    assert_eq!(
        kelir::scrollback::display_width(&rendered),
        60,
        "{rendered:?}"
    );

    // A call without a description keeps the day-one rendering.
    let old = json!({"command": "git --version"});
    assert_eq!(kelir::tui::call_head("bash", &old), "$ git --version");
    assert_eq!(kelir::tui::call_label("bash", &old), "bash: git --version");

    // A failing call's line is still red, with the label intact.
    let mut failed = call;
    failed.outcome = "error".into();
    failed.exit_code = Some(1);
    let line = failed.line(theme, 60);
    assert_eq!(line.spans[0].style.fg, Some(Color::Red));
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains(DESC), "{text:?}");
}
