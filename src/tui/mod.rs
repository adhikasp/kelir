//! The full-screen TUI. The alternate screen is ours: the transcript is a
//! viewport over rows built from the session's events, so every prompt and
//! answer stays reachable and a resize reflows them from their source.

pub mod activity;
mod app;
pub mod commands;
pub mod complete;
pub mod editor;
mod editor_impl;
mod keys;
pub mod markdown;
pub mod scrollback;
mod terminal;
pub mod theme;
pub mod tools_view;
mod view;

use std::io::Write;
use std::time::Instant;

use ratatui::crossterm::event::Event;
use tokio::sync::mpsc;

use crate::features::herdr::State as HerdrState;
use crate::ipc::Response;
use crate::tui::activity::Kind;
use crate::tui::theme::Theme;

pub use app::{App, Entry};
pub use keys::{Press, WHEEL_ROWS, translate, translate_mouse};

/// An out-of-log note shown exactly like a log note. The session records the
/// real event when it has a sequence number; this is for the few UI-only
/// messages whose log copy is written elsewhere.
pub fn note_event(text: impl Into<String>) -> crate::log::event::LogEvent {
    crate::log::event::LogEvent::new(
        0,
        crate::log::event::EventBody::Note(crate::log::event::Note {
            kind: "core".into(),
            text: text.into(),
        }),
    )
}

/// The error counterpart of [`note_event`]: a view-only diagnostic, never a
/// record in `events.jsonl`.
pub fn error_event(message: impl Into<String>) -> crate::log::event::LogEvent {
    crate::log::event::LogEvent::new(
        0,
        crate::log::event::EventBody::Error {
            location: "ui".into(),
            message: message.into(),
            retryable: false,
            status: None,
            attempt: None,
            retry_in_ms: None,
        },
    )
}

pub use view::{
    COMPLETION_MARK, POPUP_ROWS, PROMPT_WIDTH, call_head, call_label, facts_for_width, fit,
    last_line, read_tail, render, status_text, tokens, tui_wait_line,
};

/// The one channel from the session to the view. Log events are the source of
/// truth; live events are the handful of facts that never enter the log.
// LogEvent is already cloned at every append; boxing it would add an
// allocation to the hottest path without making the channel cheaper.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum View {
    Log(crate::log::event::LogEvent),
    Live(Live),
}

/// What only the running TUI knows: keystrokes in flight, transient wait
/// countdowns, verb answers and terminal status. Everything else is a log
/// event.
#[derive(Debug, Clone)]
pub enum Live {
    /// Provider text as it arrives, before the assistant event is committed.
    Delta(String),
    /// A rate-limit or slot wait, whose deadline is not a log fact.
    Retrying {
        line: String,
        until: Instant,
        status: Option<u16>,
        attempt: u32,
    },
    RetryingDone,
    /// An answer to a verb the TUI sent itself.
    Reply(Response),
    /// The Herdr pane's status word.
    Herdr {
        state: HerdrState,
    },
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Working,
    Done,
    Paused,
    Error,
}

impl Phase {
    pub fn word(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Paused => "paused",
            Self::Error => "error",
        }
    }

    pub fn activity_kind(self) -> Kind {
        match self {
            Self::Idle | Self::Done => Kind::Idle,
            Self::Working => Kind::Thinking,
            Self::Paused => Kind::Paused,
            Self::Error => Kind::Error,
        }
    }
}

/// The facts row's data: model and tokens; steps and cache are tracked for
/// the log, not shown in the ambient row.
#[derive(Debug, Clone)]
pub struct Status {
    pub provider: String,
    pub model: String,
    pub step: u32,
    pub prompt_tokens: u64,
    pub last_prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub running: usize,
    pub turn_steps: u32,
    pub limit: Option<u32>,
    pub phase: Phase,
    pub title: String,
    pub retrying: Option<(String, Instant)>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            provider: String::new(),
            model: String::new(),
            step: 0,
            prompt_tokens: 0,
            last_prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            running: 0,
            turn_steps: 0,
            limit: None,
            phase: Phase::Idle,
            title: String::new(),
            retrying: None,
        }
    }
}

/// `kelir`'s own words on first paint, in the order they arrive.
pub struct BannerLine {
    pub text: String,
    pub dim: bool,
}

impl BannerLine {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            dim: false,
        }
    }

    pub fn dim(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            dim: true,
        }
    }
}

/// The event loop: draw, wait for a key, session event or tick, then repeat.
/// The [`terminal::RestoreGuard`] field restores the terminal on any exit,
/// including an error returned from `run`.
pub struct Tui {
    pub app: App,
    terminal: ratatui::DefaultTerminal,
    events: mpsc::UnboundedReceiver<View>,
    keys: mpsc::UnboundedReceiver<Event>,
    tick: Option<Instant>,
    last_paint: Option<Instant>,
    #[expect(dead_code, reason = "held for its Drop, not read")]
    guard: terminal::RestoreGuard,
}

#[allow(clippy::large_enum_variant)]
enum Step {
    Ui(View),
    Key(Event),
    Tick,
}

/// Take the terminal and the session's event stream. The caller has already
/// checked that stdout is a terminal.
pub fn start(
    events: mpsc::UnboundedReceiver<View>,
    to_tui: mpsc::UnboundedSender<View>,
    verbs: mpsc::UnboundedSender<crate::ipc::Incoming>,
    banner: &[BannerLine],
    cap: Option<u32>,
    token: String,
    theme: Theme,
    skills: Vec<complete::SkillInfo>,
) -> std::io::Result<Tui> {
    terminal::start(events, to_tui, verbs, banner, cap, token, theme, skills)
}

impl Tui {
    /// Drive the UI until it quits. `Ok(true)` means the person asked for a new
    /// session (`/new`): the terminal is already restored, and the process is
    /// meant to start a fresh one rather than return to the shell.
    pub async fn run(self) -> std::io::Result<bool> {
        self.run_inner().await
    }

    async fn run_inner(mut self) -> std::io::Result<bool> {
        loop {
            self.flush_status()?;
            self.draw()?;
            // A documented, test-only panic with the terminal fully owned: it
            // exercises the restore path a real crash takes.
            if crate::envs::var_os("KELIR_TEST_PANIC").is_some() {
                panic!("KELIR_TEST_PANIC: injected panic to prove the terminal is restored");
            }
            if self.app.quit {
                break;
            }
            if !self.app.activity.kind.working() {
                self.tick = None;
            } else if self.tick.is_none() {
                self.tick = Some(Instant::now() + view::SPINNER_TICK);
            }
            let step: Option<Step> = tokio::select! {
                event = self.events.recv() => event.map(Step::Ui),
                event = self.keys.recv() => event.map(Step::Key),
                _ = wait_until(self.tick), if self.tick.is_some() => Some(Step::Tick),
            };
            match step {
                Some(Step::Ui(event)) => self.drain(event),
                Some(Step::Key(event)) => {
                    self.on_terminal(event);
                    while let Ok(event) = self.keys.try_recv() {
                        self.on_terminal(event);
                    }
                }
                Some(Step::Tick) => {
                    let now = Instant::now();
                    if self.app.tick(now) {
                        self.tick = Some(now + view::SPINNER_TICK);
                    }
                }
                None => self.app.quit = true,
            }
        }
        self.finish();
        Ok(self.app.restart)
    }

    /// Everything already queued is applied before the next paint: a burst of
    /// streamed deltas costs one frame, not one frame each.
    fn drain(&mut self, first: View) {
        self.app.apply(first);
        let mut applied = 1usize;
        while let Ok(event) = self.events.try_recv() {
            self.app.apply(event);
            applied += 1;
        }
        while let Ok(event) = self.keys.try_recv() {
            self.on_terminal(event);
        }
        if applied > 1
            && let Some(last) = self.last_paint
        {
            let elapsed = last.elapsed();
            if elapsed < view::FRAME_MIN {
                std::thread::sleep(view::FRAME_MIN - elapsed);
            }
        }
    }

    fn on_terminal(&mut self, event: Event) {
        match event {
            Event::Key(key) => {
                if let Some(press) = translate(key) {
                    self.app.press(press);
                }
            }
            Event::Paste(text) => self.app.paste(&text),
            Event::Mouse(mouse) => {
                if let Some(press) = keys::translate_mouse(mouse) {
                    self.app.press(press);
                }
            }
            // ratatui's backend resizes on the next draw; `set_size` in
            // `render` is the single resize path (it preserves scroll).
            _ => {}
        }
    }

    fn flush_status(&mut self) -> std::io::Result<()> {
        let records = self.app.take_status_records();
        if records.is_empty() {
            return Ok(());
        }
        let mut stdout = std::io::stdout();
        for record in records {
            write!(stdout, "{record}")?;
        }
        stdout.flush()
    }

    fn draw(&mut self) -> std::io::Result<()> {
        let mut stdout = std::io::stdout();
        let _ = ratatui::crossterm::execute!(
            stdout,
            ratatui::crossterm::terminal::BeginSynchronizedUpdate
        );
        let app = &mut self.app;
        let result = self.terminal.draw(|frame| view::render(app, frame));
        let _ = ratatui::crossterm::execute!(
            stdout,
            ratatui::crossterm::terminal::EndSynchronizedUpdate
        );
        self.last_paint = Some(Instant::now());
        result.map(|_| ())
    }

    /// On the way out: back to the shell's own screen, flushed, with one line
    /// saying how to come back to this session.
    ///
    /// The transcript the TUI painted is app state, not terminal scrollback
    /// (the alternate buffer saw it), so it goes away entirely instead of
    /// being dumped over whatever the person had on screen before.
    fn finish(&mut self) {
        self.flush_status().ok();
        terminal::restore();
        terminal::clear_and_hint(self.app.session_id.as_deref());
    }
}

async fn wait_until(at: Option<Instant>) {
    match at {
        Some(at) => {
            let now = Instant::now();
            if at > now {
                tokio::time::sleep(at - now).await;
            }
        }
        None => std::future::pending().await,
    }
}
