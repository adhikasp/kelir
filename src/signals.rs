//! Ctrl-C (and SIGTERM) as the session's own `stop`, while a wait is running.
//!
//! A signal must not kill a session mid-wait while the log says nothing
//! (STEER-5 review B2). The waits in `session::wait_for` select on
//! [`interrupted`], so an interrupt during a retry or slot wait ends the turn
//! the way the `stop` verb does: `turn_ended` outcome `stopped`, one-shot exit
//! 0, and a `--keep` session stays up (`kelir resume` then picks the fold up).
//!
//! The window in which an interrupt is graceful is exactly the wait: the
//! [`interrupted`] future registers the handler while it is polled and
//! deregisters it when it is dropped, and both Tokio (Unix `signal`) and the
//! console (Windows `SetConsoleCtrlHandler`) fall through to the default
//! disposition when nothing listens. So Ctrl-C during a request in flight or
//! while a tool runs still kills the process, which is the pre-STEER-5
//! behaviour and the one `kelir resume` is written for.
//!
//! Unix watches SIGINT and SIGTERM (a `kill` reads the same way); Windows
//! watches Ctrl-C and Ctrl-Break, so a supervisor's `CTRL_BREAK_EVENT` — the
//! only console event a process group can be sent — is honoured too.

/// Resolves when an interrupt arrives. Polling it installs the handler;
/// dropping it takes the handler away again.
pub async fn interrupted() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = signal(SignalKind::interrupt()).ok();
        let mut terminate = signal(SignalKind::terminate()).ok();
        tokio::select! {
            _ = async {
                match &mut interrupt {
                    Some(stream) => {
                        stream.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            } => {}
            _ = async {
                match &mut terminate {
                    Some(stream) => {
                        stream.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let mut interrupt = Box::pin(tokio::signal::ctrl_c());
        // A Ctrl-Break a supervisor sent to the group; `Err` (no console) is no
        // listener at all, which the pending branch below stands for.
        let mut brk = Box::pin(async {
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

#[cfg(test)]
mod tests {
    // Never *send* this process an interrupt here: a console control event on
    // Windows reaches every process on the console (the runner included), and a
    // Unix `SIGINT` would end the suite. What a unit test can prove is the
    // registration itself — the handler installs, stays armed, and is taken away
    // again when the wait ends — and that is the Windows half of the path. The
    // wait that consumes the signal is driven end to end on Unix, by a real
    // `SIGINT` to a real session:
    // `tests/e2e_steer5.rs::an_interrupt_during_a_wait_ends_the_turn_stopped`.

    #[tokio::test]
    async fn the_handler_installs_and_is_dropped_with_the_wait() {
        // Polling the future is what registers the handler; the timeout is what
        // drops it, which must put the default disposition back rather than
        // leaving a handler behind for the rest of the process.
        for _ in 0..3 {
            let polled =
                tokio::time::timeout(std::time::Duration::from_millis(20), super::interrupted());
            assert!(polled.await.is_err(), "nothing signalled this test");
        }
    }

    #[tokio::test]
    async fn a_wait_that_is_dropped_before_the_signal_still_leaves_cleanly() {
        // A retry wait that ends on its own deadline is the common case: the
        // `select!` drops `interrupted()` mid-poll.
        let mut wait = Box::pin(super::interrupted());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut wait)
                .await
                .is_err()
        );
        drop(wait);
        // A second wait still installs cleanly, so nothing was left poisoned.
        let again =
            tokio::time::timeout(std::time::Duration::from_millis(10), super::interrupted());
        assert!(again.await.is_err());
    }
}
