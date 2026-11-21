//! Single-flight: one lock per job (`jobs/<name>/lock`).
//! The design is `scripts/gate-lock.sh`'s, in Rust, with the lessons of
//! `docs/reviews/buildinfra-review.md` built in:

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::atomic::{Published, publish_new};

/// A lock younger than this is never taken over, whatever its pid says.
pub const FRESH: Duration = Duration::from_secs(1);
/// An unreadable lock file is somebody's until it is this old.
pub const GARBAGE_AFTER: Duration = Duration::from_secs(30);
/// The gap between the two looks that must agree before a takeover.
const SECOND_LOOK: Duration = Duration::from_millis(150);
const POLL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Unique per acquisition: what "release only by the holder" compares.
    pub token: String,
    pub pid: u32,
    /// The holder's start time (ms since the epoch); `None` where the platform
    /// cannot say, in which case only the pid's existence is checked.
    pub start: Option<u64>,
    pub cmd: String,
    pub started: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// The recorded process is running.
    Alive,
    /// It looks gone but the lock is too young to believe it.
    Fresh,
    /// Gone (or never written properly and old): may be taken over.
    Stale(String),
}

/// Judge a lock. Pure: the caller supplies what it read and whether the recorded process is
/// alive, so the whole state machine is unit-tested without a process.
pub fn judge(record: Option<&Record>, age: Duration, alive: bool) -> Verdict {
    match record {
        Some(_) if alive => Verdict::Alive,
        Some(record) => {
            if age < FRESH {
                Verdict::Fresh
            } else {
                Verdict::Stale(format!(
                    "pid {} is gone or is a different process now",
                    record.pid
                ))
            }
        }
        None => {
            if age < GARBAGE_AFTER {
                Verdict::Fresh
            } else {
                Verdict::Stale("the lock file holds no readable record".into())
            }
        }
    }
}

/// Who holds a lock that this run could not take.
#[derive(Debug, Clone, PartialEq)]
pub struct Holder {
    pub pid: Option<u32>,
    pub cmd: String,
    pub started: String,
    /// Why the lock could not be taken when there is no readable record.
    pub reason: String,
}

#[derive(Debug)]
pub enum Outcome {
    Held(Guard),
    Busy(Holder),
}

/// The lock while it is held. Dropping it releases it (if it is still ours).
#[derive(Debug)]
pub struct Guard {
    path: PathBuf,
    token: String,
    released: bool,
}

impl Guard {
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Remove the lock, but only if it still carries this acquisition's token.
    /// Returns whether it was removed.
    pub fn release(&mut self) -> bool {
        if self.released {
            return false;
        }
        self.released = true;
        match read(&self.path) {
            Some(record) if record.token == self.token => std::fs::remove_file(&self.path).is_ok(),
            _ => false,
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.release();
    }
}

/// The record of the lock at `path` if its holder is running right now.
pub fn live_holder(path: &Path) -> Option<Record> {
    let record = read(path)?;
    crate::child::process_alive(record.pid, record.start).then_some(record)
}

fn read(path: &Path) -> Option<Record> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn age_of(path: &Path) -> Duration {
    let Ok(modified) = std::fs::metadata(path).and_then(|meta| meta.modified()) else {
        // Unreadable metadata: treat as young, the safe side for a steal.
        return Duration::ZERO;
    };
    match SystemTime::now().duration_since(modified) {
        Ok(age) => age,
        // A modification time in the future is clock skew; a skew of minutes
        // must not freeze a dead lock forever.
        Err(skew) if skew.duration() > Duration::from_secs(60) => Duration::from_secs(3600),
        Err(_) => Duration::ZERO,
    }
}

fn token() -> String {
    format!(
        "{}.{}.{}",
        std::process::id(),
        ulid::Ulid::generate().to_string().to_lowercase(),
        std::time::SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    )
}

/// One attempt to take the lock at `path`; never waits (beyond the 150 ms
/// second look that a takeover needs).
pub fn try_acquire(path: &Path, cmd: &str) -> io::Result<Outcome> {
    try_acquire_with(path, cmd, &crate::child::process_alive)
}

/// [`try_acquire`] with the liveness probe injected, for the tests.
pub fn try_acquire_with(
    path: &Path,
    cmd: &str,
    alive: &dyn Fn(u32, Option<u64>) -> bool,
) -> io::Result<Outcome> {
    if let Some(parent) = path.parent() {
        crate::paths::create_private_dir(parent)?;
    }
    let pid = std::process::id();
    let mine = Record {
        token: token(),
        pid,
        start: crate::child::start_ms(pid),
        cmd: cmd.chars().take(300).collect(),
        started: crate::event::now_ts(),
    };
    let bytes = serde_json::to_vec(&mine).map_err(io::Error::other)?;
    // Taking over costs at most a few rounds; a lock that changes hands every
    // round is somebody else's busy lock.
    for _ in 0..5 {
        match publish_new(path, &bytes)? {
            Published::Created => {
                return Ok(Outcome::Held(Guard {
                    path: path.to_path_buf(),
                    token: mine.token,
                    released: false,
                }));
            }
            Published::Exists => {}
        }
        let first = read(path);
        let verdict = judge(
            first.as_ref(),
            age_of(path),
            first
                .as_ref()
                .is_some_and(|record| alive(record.pid, record.start)),
        );
        let reason = match verdict {
            Verdict::Alive | Verdict::Fresh => return Ok(Outcome::Busy(holder_of(first))),
            Verdict::Stale(reason) => reason,
        };
        // The same verdict on a second look, a moment later.
        std::thread::sleep(SECOND_LOOK);
        let second = read(path);
        if second.as_ref().map(|r| &r.token) != first.as_ref().map(|r| &r.token)
            || !matches!(
                judge(
                    second.as_ref(),
                    age_of(path),
                    second
                        .as_ref()
                        .is_some_and(|record| alive(record.pid, record.start)),
                ),
                Verdict::Stale(_)
            )
        {
            // It changed under us: somebody else is already on it.
            return Ok(Outcome::Busy(holder_of(second)));
        }
        if !take_over(path, first.as_ref().map(|r| r.token.as_str()), &mine.token)? {
            return Ok(Outcome::Busy(holder_of(read(path))));
        }
        let _ = reason;
    }
    Ok(Outcome::Busy(holder_of(read(path))))
}

fn holder_of(record: Option<Record>) -> Holder {
    match record {
        Some(record) => Holder {
            pid: Some(record.pid),
            cmd: record.cmd,
            started: record.started,
            reason: String::new(),
        },
        None => Holder {
            pid: None,
            cmd: String::new(),
            started: String::new(),
            reason: "the lock file is unreadable or holds no record; it is young \
                     and respected (it cannot be proven safe to take over)"
                .into(),
        },
    }
}

/// Move the stale lock aside and make sure it was the one judged. `Ok(true)`: it was, it is
/// gone, the caller may publish.
fn take_over(path: &Path, judged: Option<&str>, my_token: &str) -> io::Result<bool> {
    let tomb = path.with_extension(format!("stale.{my_token}"));
    if std::fs::rename(path, &tomb).is_err() {
        // Gone already, or not ours to move: let the caller look again.
        return Ok(true);
    }
    let moved = read(&tomb);
    let same = match (moved.as_ref(), judged) {
        (Some(record), Some(token)) => record.token == token,
        // An unreadable file was judged unreadable; the moved one must be too.
        (None, None) => true,
        _ => false,
    };
    if same {
        let _ = std::fs::remove_file(&tomb);
        return Ok(true);
    }
    // Not the lock that was examined: hand it back, whole.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match std::fs::hard_link(&tomb, path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&tomb);
                return Ok(false);
            }
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(io::Error::other(format!(
                    "cannot hand the job lock back to its owner; it is at {}: {error}",
                    tomb.display()
                )));
            }
        }
    }
}

/// Take the lock, waiting up to `wait` for a holder to finish. `wait` of zero
/// is one attempt.
pub fn acquire(path: &Path, cmd: &str, wait: Duration) -> io::Result<Outcome> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        match try_acquire(path, cmd)? {
            Outcome::Busy(holder) => {
                if std::time::Instant::now() >= deadline {
                    return Ok(Outcome::Busy(holder));
                }
                std::thread::sleep(POLL);
            }
            held => return Ok(held),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(pid: u32) -> Record {
        Record {
            token: "t".into(),
            pid,
            start: Some(1),
            cmd: "c".into(),
            started: "s".into(),
        }
    }

    #[test]
    fn a_live_holder_is_never_taken_from_however_old_the_lock() {
        let r = record(7);
        for age in [0, 1, 3600, 86400 * 30] {
            assert_eq!(
                judge(Some(&r), Duration::from_secs(age), true),
                Verdict::Alive
            );
        }
    }

    #[test]
    fn a_dead_holder_of_a_fresh_lock_is_not_stolen_from() {
        let r = record(7);
        assert_eq!(
            judge(Some(&r), Duration::from_millis(100), false),
            Verdict::Fresh
        );
        assert!(matches!(
            judge(Some(&r), Duration::from_secs(5), false),
            Verdict::Stale(_)
        ));
    }

    #[test]
    fn an_unreadable_young_lock_names_no_pid_but_says_why() {
        let holder = holder_of(None);
        assert_eq!(holder.pid, None);
        assert!(holder.cmd.is_empty() && holder.started.is_empty());
        assert!(
            holder.reason.contains("unreadable"),
            "a skip must explain the empty holder: {holder:?}"
        );
        // A readable holder keeps its identity and needs no reason .
        let named = holder_of(Some(record(42)));
        assert_eq!(named.pid, Some(42));
        assert!(named.reason.is_empty());
    }

    #[test]
    fn an_unreadable_lock_is_somebodys_until_it_is_old() {
        assert_eq!(judge(None, Duration::from_secs(5), false), Verdict::Fresh);
        assert!(matches!(
            judge(None, GARBAGE_AFTER, false),
            Verdict::Stale(_)
        ));
    }

    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("jobs").join("j").join("lock");
        (temp, path)
    }

    fn held(outcome: io::Result<Outcome>) -> Guard {
        match outcome.unwrap() {
            Outcome::Held(guard) => guard,
            Outcome::Busy(holder) => panic!("expected the lock, busy: {holder:?}"),
        }
    }

    #[test]
    fn the_second_taker_is_refused_with_the_holders_pid() {
        let (_temp, path) = scratch();
        let first = held(try_acquire(&path, "kelir job run j"));
        match try_acquire(&path, "kelir job run j").unwrap() {
            Outcome::Busy(holder) => {
                assert_eq!(holder.pid, Some(std::process::id()));
                assert!(holder.cmd.contains("job run"));
            }
            Outcome::Held(_) => panic!("two holders"),
        }
        drop(first);
        // Released: the next taker gets it.
        let _again = held(try_acquire(&path, "x"));
    }

    #[test]
    fn a_lock_published_by_a_dead_process_is_taken_over() {
        let (_temp, path) = scratch();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let dead = Record {
            token: "dead".into(),
            pid: 4_000_000,
            start: Some(1),
            cmd: "old".into(),
            started: "then".into(),
        };
        std::fs::write(&path, serde_json::to_vec(&dead).unwrap()).unwrap();
        // Young: refused, not stolen.
        assert!(matches!(
            try_acquire_with(&path, "x", &|_, _| false).unwrap(),
            Outcome::Busy(_)
        ));
        // Old enough: taken over.
        let old = SystemTime::now() - Duration::from_secs(10);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let guard = held(try_acquire_with(&path, "x", &|_, _| false));
        assert_ne!(read(&path).unwrap().token, "dead");
        drop(guard);
        assert!(!path.exists());
    }

    #[test]
    fn a_reused_pid_is_not_a_live_holder() {
        let (_temp, path) = scratch();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // This very process's pid, with a start time of a different process.
        let other = Record {
            token: "old".into(),
            pid: std::process::id(),
            start: crate::child::start_ms(std::process::id())
                .map(|ms| ms.saturating_sub(86_400_000)),
            cmd: "previous run".into(),
            started: "yesterday".into(),
        };
        std::fs::write(&path, serde_json::to_vec(&other).unwrap()).unwrap();
        let old = SystemTime::now() - Duration::from_secs(10);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        if crate::child::start_ms(std::process::id()).is_none() {
            return; // the platform cannot tell start times apart
        }
        let _guard = held(try_acquire(&path, "x"));
    }

    #[test]
    fn a_holder_whose_pid_is_alive_keeps_an_old_lock() {
        let (_temp, path) = scratch();
        let _guard = held(try_acquire(&path, "x"));
        let old = SystemTime::now() - Duration::from_secs(86_400);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(matches!(try_acquire(&path, "y").unwrap(), Outcome::Busy(_)));
    }

    #[test]
    fn release_leaves_a_lock_that_is_no_longer_ours() {
        let (_temp, path) = scratch();
        let mut mine = held(try_acquire(&path, "x"));
        // Somebody (a wrong takeover, a human) replaces it with their own.
        let theirs = Record {
            token: "theirs".into(),
            pid: 1,
            start: None,
            cmd: "other".into(),
            started: "now".into(),
        };
        std::fs::write(&path, serde_json::to_vec(&theirs).unwrap()).unwrap();
        assert!(!mine.release(), "must not remove a lock with another token");
        assert_eq!(read(&path).unwrap().token, "theirs");
    }

    #[test]
    fn a_takeover_hands_back_a_lock_that_changed_after_the_verdict() {
        let (_temp, path) = scratch();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let newer = Record {
            token: "newer".into(),
            pid: 9,
            start: None,
            cmd: "newer".into(),
            started: "now".into(),
        };
        std::fs::write(&path, serde_json::to_vec(&newer).unwrap()).unwrap();
        // The caller judged some other token stale; what is there is `newer`.
        let proceed = take_over(&path, Some("older"), "me").unwrap();
        assert!(!proceed);
        assert_eq!(read(&path).unwrap().token, "newer", "handed back whole");
    }

    #[test]
    fn many_threads_racing_for_the_lock_produce_one_holder() {
        let (_temp, path) = scratch();
        let path = std::sync::Arc::new(path);
        let live = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let peak = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let handles: Vec<_> = (0..6)
            .map(|_| {
                let (path, live, peak) = (path.clone(), live.clone(), peak.clone());
                std::thread::spawn(move || {
                    let mut won = 0;
                    for _ in 0..20 {
                        if let Outcome::Held(guard) = try_acquire(&path, "race").unwrap() {
                            let now = live.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                            peak.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(2));
                            live.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                            drop(guard);
                            won += 1;
                        }
                    }
                    won
                })
            })
            .collect();
        let total: u32 = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert!(total > 0);
        assert_eq!(
            peak.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "two holders at once"
        );
    }
}
