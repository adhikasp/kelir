//! Ctrl-C (and SIGTERM) as the session's own `stop`, while a wait is running.
//!
//! A signal must not kill a session mid-wait while the log says nothing
//! (STEER-5 review B2). [`interrupted`] resolves when an interrupt arrives;
//! `session::wait_for` selects on it, so a Ctrl-C during a retry or slot wait
//! ends the turn the way the `stop` verb does — `turn_ended` outcome `stopped`,
//! one-shot exit 0 — and a `--keep` session stays up for the next message
//! (`kelir resume` picks a killed one up, which is the other half of the
//! story).
//!
//! **One listener per process, installed once.** Tokio never restores a signal's
//! default disposition after a `Signal` has existed ("for the duration of the
//! entire process", its own docs), so a design that registered the handler only
//! for the length of a wait would leave Ctrl-C captured — and silently ignored —
//! for the rest of the session. [`install`] therefore installs the handler once,
//! at session start, and every wait subscribes to the same broadcast.
//!
//! **The flag is level-triggered, not edge-triggered.** The first design handed
//! every wait a `watch` channel and compared its subscription baseline with the
//! current value, which loses an interrupt that lands while no wait is polling:
//! `watch::Sender::send` *drops the value* when there are no receivers at that
//! instant, and a wait that subscribes afterwards takes the already-bumped value
//! as its baseline. A wait entered after the signal was consumed by an earlier
//! wait therefore never saw it, and the Linux gate caught exactly that (about
//! two runs in six: `an_interrupt_during_a_wait_ends_the_turn_stopped` never
//! ended its turn). [`requested`] is now a plain atomic the handler sets and
//! [`interrupted`] checks **on entry** and **while waiting** (a `Notify` only
//! wakes a poll that is already parked, so the entry check is the half that
//! makes it level-triggered). [`take`] clears it when a wait acts on it; a wait
//! that is not running leaves it set, which is what "the interrupt is pending
//! for the next wait" means.
//!
//! **The escape hatch is still there.** A single interrupt is the session's
//! `stop`; a second one within [`DOUBLE`] ends the process (`exit 130`), the
//! same rule the TUI applies to two Ctrl-Cs. That keeps a person's way out of a
//! wedged kelir without a second mechanism.
//!
//! Unix watches SIGINT and SIGTERM (a `kill` reads the same way); Windows
//! watches Ctrl-C and Ctrl-Break, so a supervisor's `CTRL_BREAK_EVENT` is
//! honoured too. Note that a Windows console control event addresses a whole
//! console, not one process: `GenerateConsoleCtrlEvent` from a test would also
//! hit the test runner, which is why nothing in the suite sends one.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

/// A second interrupt inside this window ends the process.
pub const DOUBLE: Duration = Duration::from_secs(2);

/// The process-wide listener, set inside [`install`].
static ARMED: OnceLock<&'static Listener> = OnceLock::new();
/// Guards the one-time setup itself, so two sessions starting at once do not
/// leak a second listener.
static INSTALL: std::sync::Once = std::sync::Once::new();

/// The interrupt state: a level that stays set until a wait acts on it, plus a
/// wakeup for a wait that is already parked in `select!`.
struct Listener {
    /// An interrupt is pending. Set by the handler, checked on entry to every
    /// wait, cleared by [`take`].
    requested: AtomicBool,
    /// How many interrupts have arrived, ever. Only for tests and diagnostics.
    count: AtomicU64,
    /// Wakes the waits that are parked right now.
    wake: Notify,
}

impl Listener {
    fn pending(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
}

/// Start watching for interrupts, once per process. Safe to call from every
/// session (a child session in the same process is not a thing, but a test
/// binary runs many sessions) and idempotent.
pub fn install() {
    // `call_once`, not a `get`-then-`set`: two sessions starting together (a
    // test binary does) must not leak two listeners and spawn two tasks, and
    // the one that matters — "the streams exist before this returns" — has to
    // hold for the losing caller too.
    INSTALL.call_once(|| {
        let listener: &'static Listener = Box::leak(Box::new(Listener {
            requested: AtomicBool::new(false),
            count: AtomicU64::new(0),
            wake: Notify::new(),
        }));
        let _ = ARMED.set(listener);
        // The OS-level handler is installed by creating the streams, not by
        // polling them: do that here, on the caller's task, so the protection
        // exists before `install` returns. A signal that arrives between this
        // call and the listener task's first poll would otherwise still have
        // the default disposition — the process dies — which is the narrow end
        // of the same race.
        let mut streams = Streams::new();
        tokio::spawn(async move {
            let mut last: Option<Instant> = None;
            loop {
                streams.one().await;
                listener.count.fetch_add(1, Ordering::SeqCst);
                let now = Instant::now();
                let twice = last
                    .map(|at| now.duration_since(at) <= DOUBLE)
                    .unwrap_or(false);
                last = Some(now);
                // The second interrupt is the way out: the same rule the TUI's
                // status line states ("Ctrl-C again within a second to quit").
                if twice {
                    let _ = std::io::Write::flush(&mut std::io::stderr());
                    std::process::exit(130);
                }
                // Set the level *before* the wakeup: a wait that checks the
                // level and then parks must see either `true` or the
                // notification, never neither.
                listener.requested.store(true, Ordering::SeqCst);
                listener.wake.notify_waiters();
            }
        });
    });
}

/// Resolves when an interrupt is pending — **immediately** if one arrived
/// before this call, which is the whole point: the handler runs on its own
/// task, so the signal can land between the `error` event and the first poll of
/// the wait that follows it, and an edge-triggered wakeup lost exactly that
/// (STEER-5 fixes round 2).
///
/// It never clears the flag; [`take`] does, so a wait that is cancelled for
/// another reason leaves the interrupt pending for the next one.
pub async fn interrupted() {
    let Some(listener) = ARMED.get() else {
        return std::future::pending().await;
    };
    loop {
        let notified = listener.wake.notified();
        tokio::pin!(notified);
        // Register before the check: a `Notified` future joins the waiter list
        // when it is enabled (or polled), not when it is created, and
        // `notify_waiters` stores no permit. Enabling first means an interrupt
        // that lands between the check and the await still completes the await
        // rather than parking for ever.
        notified.as_mut().enable();
        if listener.pending() {
            return;
        }
        notified.await;
    }
}

/// Clear the pending interrupt, and say whether there was one. A wait calls
/// this when it acts on [`interrupted`], so one Ctrl-C ends one turn.
pub fn take() -> bool {
    match ARMED.get() {
        Some(listener) => listener.requested.swap(false, Ordering::SeqCst),
        None => false,
    }
}

/// How many interrupts this process has seen (tests and diagnostics).
pub fn count() -> u64 {
    ARMED
        .get()
        .map(|listener| listener.count.load(Ordering::SeqCst))
        .unwrap_or(0)
}

/// The platform's interrupt streams, created once by [`install`] so the OS
/// handler is in place before the listener task is even spawned.
struct Streams {
    #[cfg(unix)]
    interrupt: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    terminate: Option<tokio::signal::unix::Signal>,
    #[cfg(not(unix))]
    interrupt: Option<tokio::signal::windows::CtrlC>,
    #[cfg(not(unix))]
    brk: Option<tokio::signal::windows::CtrlBreak>,
}

impl Streams {
    fn new() -> Streams {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Streams {
                interrupt: signal(SignalKind::interrupt()).ok(),
                terminate: signal(SignalKind::terminate()).ok(),
            }
        }
        #[cfg(not(unix))]
        {
            Streams {
                interrupt: tokio::signal::windows::ctrl_c().ok(),
                brk: tokio::signal::windows::ctrl_break().ok(),
            }
        }
    }

    /// Wait for one interrupt, `pending` for ever when the platform refused a
    /// handler (the process then keeps its default disposition).
    async fn one(&mut self) {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = recv_unix(&mut self.interrupt) => {}
                _ = recv_unix(&mut self.terminate) => {}
            }
        }
        #[cfg(not(unix))]
        {
            tokio::select! {
                _ = recv_windows(&mut self.interrupt) => {}
                _ = recv_windows(&mut self.brk) => {}
            }
        }
    }
}

#[cfg(unix)]
async fn recv_unix(stream: &mut Option<tokio::signal::unix::Signal>) {
    match stream {
        Some(stream) => {
            stream.recv().await;
        }
        None => std::future::pending().await,
    }
}

#[cfg(not(unix))]
async fn recv_windows<S: WindowsSignal>(stream: &mut Option<S>) {
    match stream {
        Some(stream) => {
            stream.recv().await;
        }
        None => std::future::pending().await,
    }
}

/// The two Windows control events have the same shape but no shared trait in
/// `tokio`, so the generic receiver above needs this.
#[cfg(not(unix))]
trait WindowsSignal {
    async fn recv(&mut self);
}

#[cfg(not(unix))]
impl WindowsSignal for tokio::signal::windows::CtrlC {
    async fn recv(&mut self) {
        let _ = tokio::signal::windows::CtrlC::recv(self).await;
    }
}

#[cfg(not(unix))]
impl WindowsSignal for tokio::signal::windows::CtrlBreak {
    async fn recv(&mut self) {
        let _ = tokio::signal::windows::CtrlBreak::recv(self).await;
    }
}

#[cfg(test)]
mod tests {
    // Never *send* this process an interrupt here: a console control event on
    // Windows reaches every process on the console (the runner included), and a
    // Unix `SIGINT` would end the suite. What a unit test can prove is the
    // plumbing — the listener installs once, stays installed, and a wait that
    // subscribes is simply pending while nothing happens. The waiting path is
    // driven end to end on Unix by a real `SIGINT` to a real session:
    // `tests/e2e_steer5.rs::an_interrupt_during_a_wait_ends_the_turn_stopped`.

    use super::{install, interrupted};
    use std::time::Duration;

    #[tokio::test]
    async fn the_listener_installs_once_and_waits_are_pending_until_a_signal() {
        install();
        install(); // idempotent: the second call keeps the first sender
        for _ in 0..3 {
            let polled = tokio::time::timeout(Duration::from_millis(20), interrupted());
            assert!(polled.await.is_err(), "nothing signalled this test");
        }
    }

    #[tokio::test]
    async fn a_wait_dropped_mid_poll_leaves_the_listener_alone() {
        install();
        let mut wait = Box::pin(interrupted());
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut wait)
                .await
                .is_err()
        );
        drop(wait);
        let again = tokio::time::timeout(Duration::from_millis(10), interrupted());
        assert!(again.await.is_err(), "a dropped wait poisoned the listener");
    }
}
