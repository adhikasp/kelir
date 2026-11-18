//! Herdr's agent status: one OSC 7501 record per status change.
//!
//! Herdr (the desktop app whose terminals run in `herdr-core`) shows a pane's
//! agent state from what the program writes to its terminal. The format was
//! taken from what `claude-code` emits and checked against the parser in the
//! Herdr core on this machine: `ESC ] 7501 ;` followed by `;`-separated
//! `key=value` pairs and terminated by ST (`ESC \`).
//!
//! Keys: `state` (`idle`, `working`, `done`, `blocked`, `error`), `app`, `id`
//! (the root session id, which Herdr's resume mods read as the resume label),
//! `msg` (base64: the payload may not contain spaces or `;`; Herdr decodes it,
//! as `hc get` shows for claude-code's messages). A record with no `id` is the
//! program's *root* record — that is what `hc get` prints as `status`.
//!
//! kelir never reports `blocked`: it never waits on a human.

use std::io::{IsTerminal as _, Write};

/// The app word every kelir record carries.
pub const APP: &str = "kelir";
/// OSC introducer and the status number Herdr parses.
pub const OSC_7501: &str = "\u{1b}]7501;";
/// String Terminator: what ends the sequence (safer than BEL inside a TUI).
pub const ST: &str = "\u{1b}\\";
/// Longest `msg` sent: an error message is a summary, not a log.
pub const MAX_MSG: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Working,
    Done,
    Idle,
    Error,
}

impl State {
    pub fn word(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Done => "done",
            State::Idle => "idle",
            State::Error => "error",
        }
    }
}

/// One status record, as the bytes written to the terminal.
///
/// `id` adds a second record carrying the session id: the root record (no id)
/// is the status Herdr shows, the id record is the resume label.
pub fn record(state: State, msg: Option<&str>, id: Option<&str>) -> String {
    let mut out = String::from(OSC_7501);
    out.push_str("state=");
    out.push_str(state.word());
    out.push_str(";app=");
    out.push_str(APP);
    if let Some(id) = id.filter(|id| !id.is_empty()) {
        out.push_str(";id=");
        out.push_str(&sanitise_id(id));
    }
    if let Some(msg) = msg.map(str::trim).filter(|msg| !msg.is_empty()) {
        out.push_str(";msg=");
        out.push_str(&base64(&truncate(msg, MAX_MSG)));
    }
    out.push_str(ST);
    out
}

/// `done` is the "finished while you were looking" word; a Telegram bridge
/// pane has nobody looking, so there kelir reports `idle`.
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
pub struct Herdr {
    id: String,
    telegram: bool,
    out: Box<dyn Write + Send>,
}

impl Herdr {
    /// Open the writer, or `None` when this session should not report status.
    pub fn detect(id: &str, json: bool) -> Option<Self> {
        let herdr_env = std::env::var("KELIR_HERDR").ok();
        let tty = std::io::stdout().is_terminal();
        if !wanted(herdr_env.as_deref(), tty, json) {
            return None;
        }
        let telegram = is_telegram(std::env::var("TERM_PROGRAM").ok().as_deref());
        Some(Self {
            id: id.to_string(),
            telegram,
            out: Box::new(std::io::stdout()),
        })
    }

    /// A writer over any sink, for tests.
    pub fn with_writer(id: &str, telegram: bool, out: impl Write + Send + 'static) -> Self {
        Self {
            id: id.to_string(),
            telegram,
            out: Box::new(out),
        }
    }

    /// Write one status. A terminal that has gone away is not an error worth
    /// reporting: the status is a courtesy, never a reason to fail a turn.
    pub fn report(&mut self, state: State, msg: Option<&str>) {
        let state = state_for(state, self.telegram);
        let root = record(state, msg, None);
        let labelled = record(state, msg, Some(&self.id));
        let _ = self.out.write_all(root.as_bytes());
        let _ = self.out.write_all(labelled.as_bytes());
        let _ = self.out.flush();
    }

    pub fn telegram(&self) -> bool {
        self.telegram
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

/// An id is a `ses_<ulid>`, but a record must not be broken by anything odd.
fn sanitise_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect()
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding (RFC 4648). The status payload is a
/// delimiter-free alphabet, so a message never has to be escaped.
pub fn base64(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[(triple >> 18) as usize & 0x3f] as char);
        out.push(B64[(triple >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(B64[(triple >> 6) as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64[triple as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
    out
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
    fn base64_matches_the_rfc_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("Waiting for M2", "V2FpdGluZyBmb3IgTTI="),
        ] {
            assert_eq!(base64(input), expected, "{input}");
        }
    }

    #[test]
    fn a_record_is_esc_7501_pairs_between_st() {
        assert_eq!(
            record(State::Working, None, None),
            "\u{1b}]7501;state=working;app=kelir\u{1b}\\"
        );
        assert_eq!(
            record(State::Done, None, Some("ses_01ABC")),
            "\u{1b}]7501;state=done;app=kelir;id=ses_01ABC\u{1b}\\"
        );
    }

    #[test]
    fn the_message_is_base64_and_comes_last() {
        let text = record(State::Error, Some("provider said 500"), None);
        assert!(
            text.ends_with(";msg=cHJvdmlkZXIgc2FpZCA1MDA=\u{1b}\\"),
            "{text:?}"
        );
        assert!(!text.contains(' '), "a payload may not contain a space");
        // The trailing `=` padding is part of the allowed alphabet.
        assert!(text.contains(";msg=cHJvdmlkZXIgc2FpZCA1MDA="));
    }

    #[test]
    fn a_blocked_message_is_never_sent() {
        // kelir has no `blocked` word; only the four states exist.
        let states = [State::Working, State::Done, State::Idle, State::Error];
        for state in states {
            let text = record(state, None, None);
            assert!(!text.contains("blocked"), "{text}");
        }
    }

    #[test]
    fn a_long_message_is_cut_on_a_char_boundary() {
        let long = "é".repeat(MAX_MSG);
        let text = record(State::Error, Some(&long), None);
        let msg = text.split(";msg=").nth(1).unwrap();
        let msg = msg.trim_end_matches(ST);
        // 2048 bytes of "é" is `max/2` chars, and base64 of 2048 bytes is a
        // fixed length; what matters is that encoding a cut string worked.
        assert!(msg.len() <= MAX_MSG.div_ceil(3) * 4, "{msg}");
        assert!(!msg.contains(' '));
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
    fn status_is_off_for_json_and_on_for_a_terminal_or_the_env() {
        assert!(!wanted(None, false, false));
        assert!(wanted(None, true, false));
        assert!(wanted(Some("1"), false, false));
        assert!(!wanted(Some("0"), true, false));
        // NDJSON on stdout stays parseable whatever the environment says.
        assert!(!wanted(Some("1"), true, true));
    }

    #[test]
    fn a_report_writes_the_root_record_then_the_labelled_one() {
        let sink = Sink::default();
        let mut herdr = Herdr::with_writer("ses_01", false, sink.clone());
        herdr.report(State::Working, None);
        let text = sink_text(&sink);
        let expected = format!(
            "{}{}",
            record(State::Working, None, None),
            record(State::Working, None, Some("ses_01"))
        );
        assert_eq!(text, expected);
        assert_eq!(text.matches(OSC_7501).count(), 2);
        // In `records` by id, and as the terminal's status (no id).
        assert!(text.contains(";id=ses_01"));
        assert!(text.starts_with("\u{1b}]7501;state=working;app=kelir\u{1b}\\"));
    }

    #[test]
    fn a_telegram_report_says_idle() {
        let sink = Sink::default();
        let mut herdr = Herdr::with_writer("ses_01", true, sink.clone());
        herdr.report(State::Done, Some("all done"));
        let text = sink_text(&sink);
        assert!(text.contains("state=idle"), "{text:?}");
        assert!(!text.contains("state=done"), "{text:?}");
        assert!(herdr.telegram());
        assert_eq!(herdr.id(), "ses_01");
    }

    #[test]
    fn an_id_that_is_not_a_session_id_cannot_break_the_record() {
        assert_eq!(sanitise_id("ses_01;state=error"), "ses_01stateerror");
        let text = record(State::Idle, None, Some("a;b\nc"));
        assert_eq!(text.matches(';').count(), 3, "{text:?}");
    }
}
