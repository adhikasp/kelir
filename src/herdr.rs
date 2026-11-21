//! Herdr's agent status: one OSC 7501 record per status change.
//!
//! Herdr (the desktop app whose terminals run in `herdr-core`) shows a pane's
//! agent state from what the program writes to its terminal. The record this
//! Herdr accepts is exactly
//!
//! ```text
//! ESC ] 7 5 0 1 ; s t a t e = <word> ESC \
//! ```
//!
//! and nothing else: any extra `;key=value` pair (`app`, `id`, `msg`, `title`,
//! …), a trailing `;`, or another separator makes the whole record invalid and
//! the pane's `status` stays `null`. That was measured, not assumed: see
//! `scripts/live/m2_osc_probe.py`, and `hc get <pane>` on `hc 0.1.21` with the
//! live drive in `scripts/live/m2_status_check.sh`. M2's first writer carried
//! `app=kelir`, `id=<session>` and a base64 `msg`; this Herdr ignored every
//! one of those, so kelir's panes never showed a status at all (the M2 review's
//! B1). DEVIATIONS records what is lost with them.
//!
//! States: `idle`, `working`, `done`, `error`, and `blocked` for a turn paused
//! on an explicit step cap — that is a turn waiting for a human to say
//! `continue`, the one case where kelir does wait on one. A `paused` word would
//! be clearer, but this Herdr does not know it: it accepts `blocked` and
//! silently drops any other word (`scripts/live/m2_osc_probe.py`, measured
//! 2026-11-19, status stayed `null` for `state=paused`, `state=waiting`).

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

/// One status record, as the bytes written to the terminal.
///
/// This is the only record kelir sends. Do not add a key: Herdr 0.1.21 drops
/// the whole record when it sees one.
pub fn record(state: State) -> String {
    let mut out = String::from(OSC_7501);
    out.push_str("state=");
    out.push_str(state.word());
    out.push_str(ST);
    out
}

/// One terminal-title record (OSC 0), so a Herdr pane/tab names the session
/// the way the facts row does. The tab-titles mod reads the pane's own title;
/// this is the one source for both. ST-terminated, like the status record.
pub fn title_record(title: &str) -> String {
    // A title is derived from the first prompt and bounded to 60 chars; strip
    // control characters defensively so a crafted prompt cannot inject a new
    // OSC sequence into the terminal.
    let clean: String = title.chars().filter(|c| !c.is_control()).take(80).collect();
    format!("\x1b]0;{clean}{ST}")
}

pub fn state_for(outcome: State, telegram: bool) -> State {
    match (outcome, telegram) {
        (State::Done, true) => State::Idle,
        (other, _) => other,
    }
}

/// Whether a status writer should be opened at all: never for `--json` (NDJSON
/// on stdout must stay parseable), otherwise when the session owns a terminal
/// or the caller asked for it with `KELIR_HERDR=1`.
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

/// The status writer of one session process.
///
/// The TUI does not write through this: its records go through the TUI task,
/// so nothing shares stdout with the painter (M2 review F2). `kelir run` and
/// `run --keep` use it.
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

    /// A writer over any sink, for tests.
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

    #[test]
    fn every_state_is_exactly_esc_7501_state_word_st() {
        // The exact bytes Herdr 0.1.21 accepts: no app word, no id, no msg, no
        // trailing separator. `state=working;app=kelir` is a *different*
        // record, and Herdr drops it (that was B1).
        for (state, word) in [
            (State::Working, "working"),
            (State::Done, "done"),
            (State::Idle, "idle"),
            (State::Blocked, "blocked"),
            (State::Error, "error"),
        ] {
            assert_eq!(
                record(state),
                format!("\u{1b}]7501;state={word}\u{1b}\\"),
                "{word}"
            );
        }
    }

    #[test]
    fn a_record_carries_no_key_but_state() {
        for state in [
            State::Working,
            State::Done,
            State::Idle,
            State::Blocked,
            State::Error,
        ] {
            let text = record(state);
            let payload = text.trim_start_matches(OSC_7501).trim_end_matches(ST);
            assert_eq!(payload, format!("state={}", state.word()));
            assert!(!payload.contains(';'), "{payload:?}");
            assert!(!payload.contains("app="), "{payload:?}");
            assert!(!payload.contains("id="), "{payload:?}");
            assert!(!payload.contains("msg="), "{payload:?}");
        }
    }

    /// `blocked` is the pause word: this Herdr accepts it and drops `paused`,
    /// so a paused turn's pane is visibly waiting instead of looking like one
    /// that never started (review N2).
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

    #[test]
    fn title_record_is_osc_zero_st_terminated() {
        assert_eq!(
            title_record("Fix the input box"),
            "\u{1b}]0;Fix the input box\u{1b}\\"
        );
    }

    #[test]
    fn title_record_strips_control_chars_so_a_prompt_cannot_inject_osc() {
        // A crafted title with an embedded ST must not escape the OSC string.
        // The embedded ESC bytes are stripped so no new OSC can start.
        let record = title_record("ok\u{1b}]0;evil\u{1b}\\ tail");
        assert_eq!(record, "\u{1b}]0;ok]0;evil\\ tail\u{1b}\\");
        // Exactly one introducer and one terminator: the injected ESC is gone.
        assert_eq!(record.matches("\u{1b}]").count(), 1);
        assert_eq!(record.matches(ST).count(), 1);
    }

    #[test]
    fn title_record_is_bounded() {
        let long = "x".repeat(200);
        let record = title_record(&long);
        let payload = record.trim_start_matches("\u{1b}]0;").trim_end_matches(ST);
        assert_eq!(payload.len(), 80);
    }

    #[test]
    fn write_record_writes_prebuilt_bytes() {
        let sink = Sink::default();
        let mut herdr = Herdr::with_writer(false, sink.clone());
        herdr.write_record("\u{1b}]0;hello\u{1b}\\");
        assert_eq!(sink_text(&sink), "\u{1b}]0;hello\u{1b}\\");
    }

    #[test]
    fn status_is_off_for_json_and_on_for_a_terminal_or_the_env() {
        assert!(!wanted(None, false, false));
        assert!(wanted(None, true, false));
        assert!(wanted(Some("1"), false, false));
        assert!(!wanted(Some("0"), true, false));
        // NDJSON on stdout stays parseable whatever the environment says.
        assert!(!wanted(Some("1"), true, true));
    }

    #[test]
    fn a_report_is_one_record_and_no_more() {
        let sink = Sink::default();
        let mut herdr = Herdr::with_writer(false, sink.clone());
        herdr.report(State::Working);
        let text = sink_text(&sink);
        assert_eq!(text, "\u{1b}]7501;state=working\u{1b}\\");
        assert_eq!(text.matches(OSC_7501).count(), 1);
    }

    #[test]
    fn a_telegram_report_says_idle() {
        let sink = Sink::default();
        let mut herdr = Herdr::with_writer(true, sink.clone());
        herdr.report(State::Done);
        let text = sink_text(&sink);
        assert_eq!(text, "\u{1b}]7501;state=idle\u{1b}\\");
        assert!(herdr.telegram());
    }
}
