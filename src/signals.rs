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
use std::time::{Duration, Instant};

use tokio::sync::watch;

/// A second interrupt inside this window ends the process.
pub const DOUBLE: Duration = Duration::from_secs(2);

/// The process-wide interrupt counter. `install` sets it; every wait
/// subscribes to it.
static ARMED: OnceLock<watch::Sender<u64>> = OnceLock::new();

/// Start watching for interrupts, once per process. Safe to call from every
/// session (a child session in the same process is not a thing, but a test
/// binary runs many sessions) and idempotent.
pub fn install() {
    if ARMED.get().is_some() {
        return;
    }
    let (tx, _rx) = watch::channel(0u64);
    if ARMED.set(tx.clone()).is_err() {
        return;
    }
    tokio::spawn(async move {
        let mut last: Option<Instant> = None;
        let mut count = 0u64;
        loop {
            watch_for_one().await;
            count = count.saturating_add(1);
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
            let _ = tx.send(count);
        }
    });
}

/// Resolves when an interrupt arrives. Installs nothing, so an interrupt
/// outside a wait (during a request in flight, a tool run) reaches no one and
/// leaves the session alone — the second-interrupt rule above is the way out.
pub async fn interrupted() {
    let Some(tx) = ARMED.get() else {
        return std::future::pending().await;
    };
    let mut rx = tx.subscribe();
    let start = *rx.borrow_and_update();
    loop {
        if *rx.borrow_and_update() != start {
            return;
        }
        if rx.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
}

/// Wait for one interrupt, `pending` for ever when the platform refuses a
/// handler.
async fn watch_for_one() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt()).ok();
        let mut terminate = signal(SignalKind::terminate()).ok();
        tokio::select! {
            _ = recv(&mut interrupt) => {}
            _ = recv(&mut terminate) => {}
        }
    }
    #[cfg(not(unix))]
    {
        let mut interrupt = std::pin::pin!(tokio::signal::ctrl_c());
        let mut brk = std::pin::pin!(async {
            match tokio::signal::windows::ctrl_break() {
                Ok(mut stream) => {
                    stream.recv().await;
                }
                Err(_) => std::future::pending::<()>().await,
            }
        });
        tokio::select! {
            _ = &mut interrupt => {}
            _ = &mut brk => {}
        }
    }
}

#[cfg(unix)]
async fn recv(stream: &mut Option<tokio::signal::unix::Signal>) {
    match stream {
        Some(stream) => {
            stream.recv().await;
        }
        None => std::future::pending::<()>().await,
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
