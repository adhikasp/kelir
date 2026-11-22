//! Proof: render the idle screen and a long wrapped prompt as plain text.
use kelir::editor::Key;
use kelir::event::{EventBody, LogEvent, Payload};
use kelir::tui::{self, App, Press, View};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use tokio::sync::mpsc;

fn channels() -> (
    mpsc::UnboundedSender<View>,
    mpsc::UnboundedSender<kelir::ipc::Incoming>,
) {
    (mpsc::unbounded_channel().0, mpsc::unbounded_channel().0)
}

fn dump(app: &mut App, height: u16, label: &str) {
    let backend = TestBackend::new(80, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| tui::render(app, frame)).unwrap();
    println!("===== {label} =====");
    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell(Rect::new(x, y, 1, 1).as_position()) {
                line.push_str(cell.symbol());
            }
        }
        println!("|{}|", line.trim_end());
    }
}

fn main() {
    let (to_tui, verbs) = channels();
    let mut app = App::new(80, 24, to_tui, verbs, String::new());
    // One finished answer so the live area has content above the editor.
    app.apply(View::Log(LogEvent::new(
        1,
        EventBody::Assistant {
            step: 1,
            text: Payload::text("Here is the answer to your first question."),
            reasoning: None,
            tool_calls: Vec::new(),
            finish_reason: None,
            usage: None,
        },
    )));
    app.apply(View::Log(LogEvent::new(
        2,
        EventBody::TurnEnded {
            outcome: "done".into(),
            reason: None,
            final_text: Payload::text("Here is the answer to your first question."),
        },
    )));
    app.apply(View::Log(LogEvent::new(
        3,
        EventBody::SessionTitle {
            title: "Fix the input box gap".into(),
        },
    )));
    let screen: u16 = 24;
    dump(&mut app, screen, "idle with title");

    // A long prompt that wraps across several visual rows at width 80.
    let text = "Please refactor the editor so that the cursor moves by visual row when the prompt wraps across several lines and the input box grows to keep the whole message visible at all times. Also remove the redundant status prefix, add an automatic session title from the first prompt, show it in the facts row, and make the Herdr pane title match it too.";
    for c in text.chars() {
        app.press(Press::Edit(Key::Char(c)));
    }
    dump(&mut app, screen, "long wrapped prompt");
}
