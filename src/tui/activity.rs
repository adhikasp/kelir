//! What the agent is doing, as one value the state row renders and the tests can assert on.
//! The design note (TUI-v2 §3) asks for exactly this split: the coarse phase tells a supervisor
//! "working or not", while [`Activity`] carries the detail a human needs ("thinking for 12 s",
//! "running `cargo test`").
//! It is a pure state machine: transitions take an [`Instant`] and touch nothing else, so every

use std::time::{Duration, Instant};

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::tui::theme::Theme;

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
            let used = crate::tui::scrollback::display_width(&format!(" {head} "));
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
