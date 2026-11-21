//! What the agent is doing, as one value the state row renders and the tests can assert on.
//! The design note (TUI-v2 §3) asks for exactly this split: the coarse phase tells a supervisor
//! "working or not", while [`Activity`] carries the detail a human needs ("thinking for 12 s",
//! "running `cargo test`").
//! It is a pure state machine: transitions take an [`Instant`] and touch nothing else, so every

use std::time::{Duration, Instant};

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Waiting for the human; nothing animates.
    Idle,
    /// A request is in flight and no text has arrived yet.
    Thinking,
    /// Text is streaming.
    Writing,
    /// At least one tool call is running.
    Running,
    /// A retryable provider error is being waited out.
    RateLimited,
    /// An explicit step cap paused the turn: the session waits for a human.
    Paused,
    /// The turn failed.
    Error,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Idle => "idle",
            Kind::Thinking => "thinking",
            Kind::Writing => "writing",
            Kind::Running => "running",
            Kind::RateLimited => "rate limited",
            Kind::Paused => "paused",
            Kind::Error => "error",
        }
    }

    /// Is the agent (or the provider) busy? The spinner, the elapsed time and
    /// the dimmed prompt all key off this.
    pub fn working(self) -> bool {
        matches!(
            self,
            Kind::Thinking | Kind::Writing | Kind::Running | Kind::RateLimited
        )
    }
}

/// One state, its start time and the detail its row shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    pub kind: Kind,
    /// When this state started: elapsed time is `now - since`.
    pub since: Instant,
    /// Calls running right now (`running` states).
    pub tools: usize,
    /// The newest running call, one line (`bash: cargo test`).
    pub preview: String,
    /// The streamed character count of the current step, for the `tok`
    /// estimate of the writing state.
    pub written: usize,
    /// How long until the next retry (`rate limited`).
    pub retry_in: Option<Duration>,
    /// The retry attempt number (`rate limited`).
    pub attempt: u32,
    /// The HTTP status or short reason (`rate limited`), the failure (`error`),
    /// or the cap (`paused`).
    pub detail: String,
}

impl Activity {
    pub fn idle(now: Instant) -> Self {
        Self {
            kind: Kind::Idle,
            since: now,
            tools: 0,
            preview: String::new(),
            written: 0,
            retry_in: None,
            attempt: 0,
            detail: String::new(),
        }
    }

    fn enter(&mut self, kind: Kind, now: Instant) {
        if self.kind != kind {
            self.kind = kind;
            self.since = now;
            if kind != Kind::RateLimited {
                self.retry_in = None;
                self.attempt = 0;
            }
        }
    }

    pub fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.since)
    }

    // ---------------------------------------------------------- transitions

    /// A turn started (or the next step of one): back to `thinking`.
    pub fn step_started(&mut self, now: Instant) {
        self.written = 0;
        self.preview.clear();
        self.detail.clear();
        self.enter(Kind::Thinking, now);
    }

    /// A piece of assistant text: `thinking` becomes `writing`. Text that keeps
    /// arriving does not restart the clock.
    pub fn text(&mut self, now: Instant, chars: usize) {
        self.written += chars;
        if matches!(self.kind, Kind::Thinking | Kind::Running) {
            self.enter(Kind::Writing, now);
        }
    }

    /// One or more calls started running.
    pub fn tool_started(&mut self, now: Instant, label: &str) {
        self.tools += 1;
        self.preview = label.to_string();
        self.enter(Kind::Running, now);
    }

    /// One call settled; the last one leaves `running`.
    pub fn tool_settled(&mut self, now: Instant) {
        self.tools = self.tools.saturating_sub(1);
        if self.tools == 0 && self.kind == Kind::Running {
            // The next request is being prepared: thinking again, so the
            // elapsed clock measures the provider wait, not the batch.
            self.preview.clear();
            self.enter(Kind::Thinking, now);
        }
    }

    pub fn retry(&mut self, now: Instant, retry_in: Duration, attempt: u32, detail: &str) {
        self.retry_in = Some(retry_in);
        self.attempt = attempt;
        self.detail = detail.to_string();
        self.enter(Kind::RateLimited, now);
    }

    /// The countdown is redrawn: keep the state, update the remaining time.
    pub fn tick_retry(&mut self, retry_in: Duration) {
        if self.kind == Kind::RateLimited {
            self.retry_in = Some(retry_in);
        }
    }

    /// The wait is over and the retry is being sent (or a slot came free): the row goes back to waiting for
    /// the provider.
    pub fn retry_done(&mut self, now: Instant) {
        if self.kind == Kind::RateLimited {
            self.retry_in = None;
            self.attempt = 0;
            self.detail.clear();
            self.enter(Kind::Thinking, now);
        }
    }

    /// An explicit step cap: the turn is paused, waiting for `continue`.
    pub fn paused(&mut self, now: Instant, detail: &str) {
        self.tools = 0;
        self.preview.clear();
        self.detail = detail.to_string();
        self.enter(Kind::Paused, now);
    }

    pub fn failed(&mut self, now: Instant, detail: &str) {
        self.tools = 0;
        self.preview.clear();
        self.detail = detail.to_string();
        self.enter(Kind::Error, now);
    }

    /// The turn is over: `done` and `stopped` go back to idle (the summary line already said which), but a
    /// turn that ended **paused** or **in error** keeps that state on the row: it is the one thing a human
    pub fn turn_ended(&mut self, now: Instant, outcome: &str) {
        self.tools = 0;
        self.preview.clear();
        self.written = 0;
        self.retry_in = None;
        self.attempt = 0;
        if self.kind == Kind::RateLimited {
            // The wait's reason (`429`) belongs to the wait: the pause that
            // ended it has its own line in the scrollback.
            self.detail.clear();
        }
        match outcome {
            // The detail is what the pause or the failure put there (the step
            // cap, the provider's message): it is the hint the human needs.
            "paused" => self.enter(Kind::Paused, now),
            "error" => self.enter(Kind::Error, now),
            _ => {
                self.detail.clear();
                self.enter(Kind::Idle, now);
            }
        }
    }

    pub fn style(&self, theme: Theme) -> Style {
        match self.kind {
            Kind::Idle => theme.idle(),
            Kind::Paused | Kind::RateLimited => theme.waiting(),
            Kind::Error => theme.error(),
            Kind::Thinking | Kind::Writing | Kind::Running => theme.working(),
        }
    }

    /// The glyph before the word: a spinner while working, a still mark
    /// otherwise.
    pub fn glyph(&self, theme: Theme, frame: u64) -> &'static str {
        match self.kind {
            Kind::Idle => theme.glyphs.idle(),
            Kind::Thinking | Kind::Writing | Kind::Running | Kind::RateLimited => {
                theme.glyphs.spinner(frame)
            }
            Kind::Paused => theme.glyphs.paused(),
            Kind::Error => theme.glyphs.error(),
        }
    }

    /// The state row: glyph, word, elapsed (working only), then the detail.
    pub fn line(&self, theme: Theme, frame: u64, now: Instant, width: usize) -> Line<'static> {
        let style = self.style(theme);
        let glyph = Span::styled(format!(" {} ", self.glyph(theme, frame)), style);
        let head = self.head(now);
        let mut spans = vec![glyph, Span::styled(head.clone(), style)];
        let detail = self.detail_text(now);
        if !detail.is_empty() {
            let used = crate::scrollback::display_width(&format!(" {head} "));
            let room = width.saturating_sub(used + 3);
            if room >= 8 {
                let detail = crate::tui::fit(&detail, room);
                spans.push(Span::styled(format!(" · {detail}"), theme.dim()));
            }
        }
        Line::from(spans)
    }

    fn head(&self, now: Instant) -> String {
        let elapsed = short_elapsed(self.elapsed(now));
        match self.kind {
            Kind::Running if self.tools > 1 => format!("running {} tools {elapsed}", self.tools),
            Kind::Running if !self.preview.is_empty() => {
                format!("running {} {elapsed}", self.preview)
            }
            _ if self.kind.working() && self.kind != Kind::RateLimited => {
                format!("{} {elapsed}", self.kind.word())
            }
            _ => self.kind.word().to_string(),
        }
    }

    fn detail_text(&self, _now: Instant) -> String {
        match self.kind {
            Kind::Idle => "ready".to_string(),
            Kind::Thinking => "waiting for the provider · esc stops".to_string(),
            Kind::Writing => {
                if self.written == 0 {
                    "esc stops".to_string()
                } else {
                    // A real token count needs a tokenizer; four characters per
                    // token is the usual English estimate and the tilde says so.
                    format!("~{} tok · esc stops", self.written / 4)
                }
            }
            Kind::Running => {
                if self.tools > 1 && !self.preview.is_empty() {
                    format!("{} · esc stops", self.preview)
                } else if self.tools > 0 {
                    "esc stops".to_string()
                } else {
                    String::new()
                }
            }
            Kind::RateLimited => {
                // The same clock the facts row reads (`retry::seconds_text`),
                // so the two rows cannot disagree by a second (N-count).
                let seconds = self
                    .retry_in
                    .map(|left| left.as_secs_f64().round() as u64)
                    .unwrap_or(0);
                let mut parts = Vec::new();
                if !self.detail.is_empty() {
                    parts.push(self.detail.clone());
                }
                parts.push(format!("retrying in {seconds} s"));
                if self.attempt > 0 {
                    parts.push(format!("attempt {}", self.attempt));
                }
                // The TUI's key, not a word to say: typing `stop` while the turn runs is a `say` the model receives as
                // a message, and only Esc (or `/stop`) cancels (`R5`).
                parts.push("esc stops".to_string());
                parts.join(" · ")
            }
            Kind::Paused => {
                if self.detail.is_empty() {
                    "say continue".to_string()
                } else {
                    format!("{} · say continue", self.detail)
                }
            }
            Kind::Error => {
                if self.detail.is_empty() {
                    "say to retry".to_string()
                } else {
                    format!("{} · say to retry", self.detail)
                }
            }
        }
    }
}

/// `12s`, `1m05s`, `1h02m`: short, fixed shape, no clock formatting.
pub fn short_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// The one-line outcome committed to the scrollback for the turns that need a human: `⏸ paused · 3
/// steps · 4.8k tok`, `✗ error · …`, `⏹ stopped · …`.
pub fn summary_line(theme: Theme, outcome: &str, steps: u32, tokens: u64) -> Line<'static> {
    let (glyph, style) = match outcome {
        "stopped" => (theme.glyphs.stopped(), theme.waiting()),
        "paused" => (theme.glyphs.paused(), theme.waiting()),
        _ => (theme.glyphs.error(), theme.error()),
    };
    // One step is one step.
    let mut parts = vec![if steps == 1 {
        "1 step".to_string()
    } else {
        format!("{steps} steps")
    }];
    parts.push(format!("{} tok", crate::tui::tokens(tokens)));
    Line::from(vec![
        Span::styled(format!("{glyph} "), style),
        Span::styled(parts.join(" · "), theme.dim()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    static BASE: OnceLock<Instant> = OnceLock::new();

    fn at(ms: u64) -> Instant {
        *BASE.get_or_init(Instant::now) + Duration::from_millis(ms)
    }

    fn text(line: Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn thinking_writing_running_and_back_again() {
        let mut activity = Activity::idle(at(0));
        assert_eq!(activity.kind, Kind::Idle);
        assert!(!activity.kind.working());

        activity.step_started(at(100));
        assert_eq!(activity.kind, Kind::Thinking);
        assert_eq!(activity.elapsed(at(2100)), Duration::from_secs(2));

        // The first delta moves to writing, and the clock keeps running from
        // the state change, not from the step.
        activity.text(at(3000), 11);
        assert_eq!(activity.kind, Kind::Writing);
        assert_eq!(activity.written, 11);
        assert_eq!(activity.elapsed(at(4000)), Duration::from_secs(1));
        // More text does not restart the clock.
        activity.text(at(5000), 9);
        assert_eq!(activity.written, 20);
        assert_eq!(activity.elapsed(at(6000)), Duration::from_secs(3));

        // A tool call: running, with the call's label as the preview.
        activity.tool_started(at(7000), "bash: cargo test");
        assert_eq!(activity.kind, Kind::Running);
        assert_eq!(activity.tools, 1);
        assert_eq!(activity.preview, "bash: cargo test");
        // A second parallel call keeps the state; the preview is the newest.
        activity.tool_started(at(7500), "bash: cargo fmt");
        assert_eq!(activity.tools, 2);
        assert_eq!(activity.preview, "bash: cargo fmt");

        // One settles: still running.
        activity.tool_settled(at(8000));
        assert_eq!(activity.kind, Kind::Running);
        assert_eq!(activity.tools, 1);
        // The last one settles: back to thinking for the next request.
        activity.tool_settled(at(9000));
        assert_eq!(activity.kind, Kind::Thinking);
        assert!(activity.preview.is_empty());

        // The turn ends: idle, and nothing animates.
        activity.turn_ended(at(10000), "done");
        assert_eq!(activity.kind, Kind::Idle);
        assert!(!activity.kind.working());
    }

    #[test]
    fn a_retry_is_a_working_state_with_a_countdown() {
        let mut activity = Activity::idle(at(0));
        activity.step_started(at(0));
        activity.retry(at(500), Duration::from_secs(34), 5, "429");
        assert_eq!(activity.kind, Kind::RateLimited);
        assert!(activity.kind.working(), "a retry wait is still working");
        activity.tick_retry(Duration::from_secs(12));
        assert_eq!(activity.retry_in, Some(Duration::from_secs(12)));
        let row = text(activity.line(Theme::plain(), 0, at(1000), 100));
        assert!(row.contains("rate limited"), "{row}");
        assert!(row.contains("retrying in 12 s"), "{row}");
        assert!(row.contains("attempt 5"), "{row}");
        // The TUI's own key: `say stop` is the CLI's sentence, and in this pane
        // a typed `stop` would be a message to the model (`R5`).
        assert!(row.contains("esc stops"), "{row}");
        assert!(!row.contains("say stop"), "{row}");
        // A retry that succeeds goes back to thinking with a cleared countdown.
        activity.step_started(at(2000));
        assert_eq!(activity.kind, Kind::Thinking);
        assert_eq!(activity.retry_in, None);
        assert_eq!(activity.attempt, 0);
    }

    /// 's provider pause ends a retry wait: the pause must win the row,
    /// with no countdown left behind and no spinner.
    #[test]
    fn a_retry_that_ends_in_a_pause_reads_paused() {
        let mut activity = Activity::idle(at(0));
        activity.step_started(at(0));
        activity.retry(at(10), Duration::from_secs(4), 3, "503");
        assert_eq!(activity.kind, Kind::RateLimited);
        activity.turn_ended(at(1000), "paused");
        assert_eq!(activity.kind, Kind::Paused);
        assert!(!activity.kind.working(), "a pause does not spin");
        assert_eq!(activity.retry_in, None);
        assert_eq!(activity.attempt, 0);
        let row = text(activity.line(Theme::plain(), 0, at(2000), 100));
        assert!(row.contains("paused"), "{row}");
        assert!(row.contains("say continue"), "{row}");
        // The wait's own reason does not survive as the pause's detail.
        assert!(!row.contains("503"), "{row}");
    }

    /// The same for an error: a wait that ends in a failure says so, with no
    /// countdown and no spinner left over.
    #[test]
    fn a_retry_that_ends_in_an_error_reads_error() {
        let mut activity = Activity::idle(at(0));
        activity.step_started(at(0));
        activity.retry(at(10), Duration::from_secs(2), 2, "429");
        activity.turn_ended(at(1000), "error");
        assert_eq!(activity.kind, Kind::Error);
        assert!(!activity.kind.working());
        assert_eq!(activity.retry_in, None);
        // A step cap's detail is kept by the pause that follows it.
        let mut capped = Activity::idle(at(0));
        capped.paused(at(0), "step cap 3 reached");
        capped.turn_ended(at(1000), "paused");
        assert_eq!(capped.kind, Kind::Paused);
        assert_eq!(capped.detail, "step cap 3 reached");
    }

    /// A slot wait is not a retry: when it ends the row keeps the work it was
    /// doing, and an idle session stays idle.
    #[test]
    fn a_wait_that_is_not_a_retry_leaves_the_row_alone() {
        let mut activity = Activity::idle(at(0));
        activity.retry_done(at(10));
        assert_eq!(activity.kind, Kind::Idle);
        activity.step_started(at(20));
        activity.retry_done(at(30));
        assert_eq!(activity.kind, Kind::Thinking);
    }

    #[test]
    fn a_paused_turn_is_not_working_and_names_the_next_action() {
        let mut activity = Activity::idle(at(0));
        activity.paused(at(0), "step cap 50 reached");
        assert_eq!(activity.kind, Kind::Paused);
        assert!(!activity.kind.working(), "nothing animates while paused");
        let row = text(activity.line(Theme::plain(), 3, at(60_000), 100));
        assert!(row.contains("paused"), "{row}");
        assert!(row.contains("step cap 50 reached"), "{row}");
        assert!(row.contains("say continue"), "{row}");
    }

    #[test]
    fn an_error_keeps_its_reason_and_offers_the_retry() {
        let mut activity = Activity::idle(at(0));
        activity.failed(at(0), "provider 401");
        assert_eq!(activity.kind, Kind::Error);
        let row = text(activity.line(Theme::plain(), 0, at(0), 80));
        assert!(row.contains("error"), "{row}");
        assert!(row.contains("provider 401"), "{row}");
        assert!(row.contains("say to retry"), "{row}");
    }

    #[test]
    fn the_state_row_is_clipped_before_the_word_is() {
        let mut activity = Activity::idle(at(0));
        activity.step_started(at(0));
        for width in [80usize, 60, 40, 20, 10] {
            let row = text(activity.line(Theme::plain(), 1, at(12_000), width));
            assert!(row.contains("thinking"), "width {width}: {row:?}");
        }
        // At 80 the detail is there; at 20 it is gone rather than half a word.
        assert!(text(activity.line(Theme::plain(), 1, at(12_000), 80)).contains("esc stops"));
        let narrow = text(activity.line(Theme::plain(), 1, at(12_000), 20));
        assert!(!narrow.contains("waiting"), "{narrow:?}");
    }

    #[test]
    fn several_running_calls_read_as_one_count_and_the_newest_preview() {
        let mut activity = Activity::idle(at(0));
        activity.tool_started(at(0), "bash: sleep 1");
        activity.tool_started(at(0), "bash: sleep 2");
        activity.tool_started(at(0), "bash: sleep 3");
        let row = text(activity.line(Theme::plain(), 0, at(3_000), 100));
        assert!(row.contains("running 3 tools 3s"), "{row}");
        assert!(row.contains("bash: sleep 3"), "{row}");
    }

    #[test]
    fn a_writing_state_counts_an_estimated_token_total() {
        let mut activity = Activity::idle(at(0));
        activity.step_started(at(0));
        activity.text(at(10), 400);
        let row = text(activity.line(Theme::plain(), 0, at(1000), 100));
        assert!(row.contains("writing 0s"), "{row}");
        assert!(row.contains("~100 tok"), "{row}");
    }

    #[test]
    fn elapsed_is_short_and_fixed_shape() {
        assert_eq!(short_elapsed(Duration::from_secs(0)), "0s");
        assert_eq!(short_elapsed(Duration::from_secs(59)), "59s");
        assert_eq!(short_elapsed(Duration::from_secs(65)), "1m05s");
        assert_eq!(short_elapsed(Duration::from_secs(3725)), "1h02m");
    }

    #[test]
    fn the_outcome_line_is_one_dim_line_and_only_for_a_turn_that_needs_a_human() {
        let theme = Theme::plain();
        let stopped = text(summary_line(theme, "stopped", 1, 10));
        assert!(stopped.starts_with('⏹'), "{stopped}");
        assert!(stopped.contains("1 step · 10 tok"), "{stopped}");
        let failed = text(summary_line(theme, "error", 2, 3000));
        assert!(failed.starts_with('✗'), "{failed}");
        assert!(failed.contains("2 steps · 3.0k tok"), "{failed}");
    }

    #[test]
    fn the_ascii_glyph_set_keeps_the_state_words_and_loses_the_spinner_unicode() {
        let mut activity = Activity::idle(at(0));
        let ascii = Theme::plain().ascii();
        assert_eq!(activity.glyph(ascii, 0), "*");
        activity.step_started(at(0));
        assert_eq!(activity.glyph(ascii, 0), "|");
        assert_eq!(activity.glyph(ascii, 1), "/");
        activity.failed(at(0), "x");
        assert_eq!(activity.glyph(ascii, 0), "x");
    }
}
