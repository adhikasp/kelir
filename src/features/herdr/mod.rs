//! Herdr's agent status: one OSC 7501 record per status change.
//! Herdr (the desktop app whose terminals run in `herdr-core`) shows a pane's agent state
//! from what the program writes to its terminal.
//! The record this Herdr accepts is exactly

pub mod ctx;

use std::io::{IsTerminal as _, Write};

/// OSC introducer and the status number Herdr parses.
pub const OSC_7501: &str = "\u{1b}]7501;";
/// String Terminator: what ends the sequence (safer than BEL inside a TUI).
pub const ST: &str = "\u{1b}\\";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Working,
    Done,
    Idle,
    /// The turn paused on an explicit cap: the session waits for a human to
    /// say `continue` (the only state in which kelir waits on one).
    Blocked,
    Error,
}

impl State {
    pub fn word(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Done => "done",
            State::Idle => "idle",
            State::Blocked => "blocked",
            State::Error => "error",
        }
    }
}

pub fn record(state: State) -> String {
    let mut out = String::from(OSC_7501);
    out.push_str("state=");
    out.push_str(state.word());
    out.push_str(ST);
    out
}

/// One terminal-title record (OSC 0), so a Herdr pane/tab names the session the way the facts
/// row does. The tab-titles mod reads the pane's own title; this is the one source for both.
pub fn title_record(title: &str) -> String {
    // A title is derived from the first prompt and bounded to 60 chars; strip control characters
    // defensively so a crafted prompt cannot inject a new OSC sequence into the terminal.
    let clean: String = title.chars().filter(|c| !c.is_control()).take(80).collect();
    format!("\x1b]0;{clean}{ST}")
}

pub fn state_for(outcome: State, telegram: bool) -> State {
    match (outcome, telegram) {
        (State::Done, true) => State::Idle,
        (other, _) => other,
    }
}

/// Whether a status writer should be opened at all: never for `--json` (NDJSON on stdout must stay
/// parseable), otherwise when the session owns a terminal or the caller asked for it with
pub fn wanted(herdr_env: Option<&str>, stdout_tty: bool, json: bool) -> bool {
    if json {
        return false;
    }
    match herdr_env.map(str::trim) {
        Some("0") | Some("false") | Some("off") | Some("no") => false,
        Some(value) if !value.is_empty() => true,
        _ => stdout_tty,
    }
}

/// `TERM_PROGRAM=herdr-telegram` marks a pane read by a Telegram bridge.
pub fn is_telegram(term_program: Option<&str>) -> bool {
    matches!(term_program, Some(program) if program.trim() == "herdr-telegram")
}

pub struct Herdr {
    telegram: bool,
    out: Box<dyn Write + Send>,
}

impl Herdr {
    /// Open the writer, or `None` when this session should not report status.
    pub fn detect(json: bool) -> Option<Self> {
        let herdr_env = crate::envs::var("KELIR_HERDR");
        let tty = std::io::stdout().is_terminal();
        if !wanted(herdr_env.as_deref(), tty, json) {
            return None;
        }
        let telegram = is_telegram(crate::envs::var("TERM_PROGRAM").as_deref());
        Some(Self {
            telegram,
            out: Box::new(std::io::stdout()),
        })
    }

    pub fn with_writer(telegram: bool, out: impl Write + Send + 'static) -> Self {
        Self {
            telegram,
            out: Box::new(out),
        }
    }

    /// Write one status. A terminal that has gone away is not an error worth
    /// reporting: the status is a courtesy, never a reason to fail a turn.
    pub fn report(&mut self, state: State) {
        let state = state_for(state, self.telegram);
        let _ = self.out.write_all(record(state).as_bytes());
        let _ = self.out.flush();
    }

    /// Write a prebuilt terminal record (the session derives OSC titles as
    /// well as statuses so the two writers stay in one module).
    pub fn write_record(&mut self, record: &str) {
        let _ = self.out.write_all(record.as_bytes());
        let _ = self.out.flush();
    }

    pub fn telegram(&self) -> bool {
        self.telegram
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn sink_text(sink: &Sink) -> String {
        String::from_utf8(sink.0.lock().unwrap().clone()).unwrap()
    }

    /// `blocked` is the pause word: this Herdr accepts it and drops `paused`, so a paused turn's
    /// pane is visibly waiting instead of looking like one that never started.
    #[test]
    fn title_record_is_bounded_and_cannot_inject_a_second_osc() {
        assert_eq!(
            title_record("Fix the input box"),
            "\u{1b}]0;Fix the input box\u{1b}\\"
        );
        // A crafted first prompt must not escape the OSC string: stripped ESC means no second OSC.
        let record = title_record("ok\u{1b}]0;evil\u{1b}\\ tail");
        assert_eq!(record, "\u{1b}]0;ok]0;evil\\ tail\u{1b}\\");
        assert_eq!(record.matches("\u{1b}]").count(), 1);
        assert_eq!(record.matches(ST).count(), 1);
        let payload = title_record(&"x".repeat(200))
            .trim_start_matches("\u{1b}]0;")
            .trim_end_matches(ST)
            .len();
        assert_eq!(payload, 80);
    }

    #[test]
    fn a_paused_turn_reports_blocked_because_herdr_drops_paused() {
        let sink = Sink::default();
        let mut herdr = Herdr::with_writer(false, sink.clone());
        herdr.report(State::Blocked);
        assert_eq!(
            sink_text(&sink),
            "\u{1b}]7501;state=blocked\u{1b}\\",
            "the pause word this Herdr accepts"
        );
        // A telegram bridge keeps `blocked` for a pause; it only remaps `done`.
        assert_eq!(state_for(State::Blocked, true), State::Blocked);
    }

    #[test]
    fn telegram_panes_get_idle_instead_of_done() {
        assert_eq!(state_for(State::Done, false), State::Done);
        assert_eq!(state_for(State::Done, true), State::Idle);
        assert_eq!(state_for(State::Working, true), State::Working);
        assert_eq!(state_for(State::Error, true), State::Error);
        assert!(is_telegram(Some("herdr-telegram")));
        assert!(!is_telegram(Some("vscode")));
        assert!(!is_telegram(None));
    }
}
