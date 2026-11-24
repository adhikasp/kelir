//! Background jobs: the shim, the status files, and the `kelir bg` CLI.
//! A job is one `kelir bg-shim` process (the same binary) that spawns the command, hands it the
//! output file directly and writes `bg/<id>.status` whenever its state changes.
//! The shim owns the command's process tree: on Unix both are in the shim's process group
//! (`setsid` for a job), on Windows the command is assigned to a named Job Object the shim keeps

pub mod cli;
pub mod launch;
pub mod shim;
pub mod status;
pub mod text;

pub use cli::*;
pub use launch::*;
pub use shim::*;
pub use status::*;
pub use text::*;

#[cfg(test)]
mod tests {
    use super::cli::CancelMarker;
    use super::status::pid_identity;
    use super::*;
    use crate::util::path_string;
    use std::path::PathBuf;
    use std::time::Duration;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kelir-bg-{}-{name}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample() -> Status {
        Status {
            id: new_id(),
            state: STATE_RUNNING.into(),
            pid: Some(4242),
            shim: Some(4241),
            shim_started: None,
            pid_started: None,
            exit_code: None,
            started: "2026-11-19T10:00:00Z".into(),
            finished: None,
            cmd: "sleep 60".into(),
            cmd_path: None,
            out: "bg/bg_1.out".into(),
            kind: KIND_JOB.into(),
            promoted: false,
            call: None,
            job: Some("kelir-bg-job-bg_1".into()),
            error: None,
        }
    }

    /// A job object name must be a plain kernel-safe token even when its parts
    /// carry path separators.
    #[test]
    fn job_names_are_filesystem_and_kernel_safe() {
        assert_eq!(
            job_name("ses_01ABCD", "11-2-1"),
            "kelir-bg-ses_01ABCD-11-2-1"
        );
        assert_eq!(job_name("s/x", "a.b"), "kelir-bg-s_x-a_b");
        let name = job_name("s/x", "a.b");
        assert!(!name.contains('\\'));
        assert!(!name.contains('/'));
    }

    #[test]
    fn a_torn_or_missing_status_file_is_skipped_not_fatal() {
        let dir = temp_dir("torn");
        let path = status_path(&dir, "bg_torn");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"id\": \"bg_torn\", \"state\": \"run").unwrap();
        assert!(Status::read(&path).is_none());
        // The listing skips it, and skips a leftover temp file too.
        std::fs::write(dir.join("bg_other.status.123.tmp"), "{}").unwrap();
        assert!(list(&dir).is_empty());
        // A good neighbour is still listed.
        let mut good = sample();
        good.id = "bg_good".into();
        good.write(&status_path(&dir, "bg_good")).unwrap();
        let listed = list(&dir);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "bg_good");
        assert!(Status::read(&dir.join("nope.status")).is_none());
    }

    #[test]
    fn promotion_is_due_only_after_the_configured_time_and_never_at_zero() {
        let after = Duration::from_secs(600);
        assert!(!promotion_due(after, Duration::from_secs(599)));
        assert!(promotion_due(after, Duration::from_secs(600)));
        assert!(promotion_due(after, Duration::from_secs(601)));
        assert!(
            !promotion_due(Duration::ZERO, Duration::from_secs(10_000)),
            "zero disables promotion"
        );
        assert!(promotion_due(
            Duration::from_secs(1),
            Duration::from_secs(1)
        ));
    }

    /// A status file with no pid must not turn `cancel` into "signal my own
    /// process group" (`kill(0, …)` is every process in it, including the test).
    #[cfg(unix)]
    #[test]
    fn killing_group_zero_does_nothing() {
        kill_tree(0, &None);
    }

    #[test]
    fn a_promotion_marker_is_written_and_seen() {
        let dir = temp_dir("promote");
        let marker = promote_path(&dir, "11-2-1");
        assert!(!marker.exists());
        mark_promoted(&dir, "11-2-1").unwrap();
        assert!(marker.exists());
    }

    /// A `cancel` in flight is not a lost job: between the kill and the `cancelled` status the shim is dead
    /// and the file still says `running`, so every reader has to be told what is happening.
    #[test]
    fn a_cancel_in_flight_is_not_a_lost_job() {
        let dir = temp_dir("cancelling");
        let status = sample();
        // No shim at all, so `observed` alone would call it lost.
        assert_ne!(status.observed().state, STATE_RUNNING);
        assert_ne!(observed_in(&dir, &status).state, STATE_RUNNING);

        let marker = CancelMarker::write(&dir, &status.id);
        assert!(cancelling(&dir, &status.id));
        assert_eq!(observed_in(&dir, &status).state, STATE_RUNNING);
        // Every way out of `cancel_at` clears the marker, including a refusal.
        std::fs::remove_file(status_path(&dir, &status.id)).ok();
        drop(marker);
        assert!(!cancelling(&dir, &status.id));
        assert_ne!(observed_in(&dir, &status).state, STATE_RUNNING);

        // A settled job stays settled whatever a stale marker says.
        let mut settled = sample();
        settled.state = STATE_CANCELLED.into();
        let marker = CancelMarker::write(&dir, &settled.id);
        assert_eq!(observed_in(&dir, &settled).state, STATE_CANCELLED);
        drop(marker);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A marker left behind by a `cancel` that was itself killed must not keep a
    /// dead job `running` for ever: past its time to live it is ignored.
    #[test]
    fn a_stale_cancel_marker_is_ignored() {
        let dir = temp_dir("stale-cancel");
        let status = sample();
        let marker = CancelMarker::write(&dir, &status.id);
        assert!(cancelling(&dir, &status.id));
        // Backdate the marker past its life, the way a killed cancel would.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(cancel_path(&dir, &status.id))
            .unwrap();
        file.set_modified(std::time::SystemTime::now() - CANCEL_MARKER_TTL * 2)
            .unwrap();
        assert!(!cancelling(&dir, &status.id));
        assert_ne!(observed_in(&dir, &status).state, STATE_RUNNING);
        drop(marker);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A job whose shim is gone is never reported as running for ever: the
    /// status file is left alone and the reader says `lost`.
    #[test]
    fn a_running_status_with_no_shim_is_lost() {
        let mut status = sample();
        // A pid that cannot exist (0 is never a process) stands in for a shim
        // that is gone.
        status.shim = Some(0);
        let observed = status.observed();
        assert_eq!(observed.state, STATE_LOST, "{observed:?}");
        assert!(observed.settled());
        assert!(observed.error.unwrap_or_default().contains("shim"));
        // A settled status is never reinterpreted.
        status.state = STATE_EXITED.into();
        status.exit_code = Some(0);
        assert_eq!(status.observed().state, STATE_EXITED);
    }

    /// The identity check itself: a status is only the shim's when it records the shim's **own** start
    /// time.
    #[test]
    fn a_status_is_matched_with_its_own_process_not_just_its_pid() {
        let me = std::process::id();
        let mine = crate::sys::proc::start_time_text(me);
        // Where the platform cannot report a start time (macOS) identity falls
        // back to liveness: `NoPlatform`, never `Same`.
        let recorded = if crate::sys::proc::start_time_text(me).is_some() {
            ShimIdentity::Same
        } else {
            ShimIdentity::NoPlatform
        };
        let mut status = sample();
        status.shim = Some(me);
        status.started = crate::log::event::now_ts();
        status.shim_started = mine.clone();
        assert_eq!(shim_identity(&status), recorded);
        assert_eq!(status.observed().state, STATE_RUNNING);

        // The process is ours, but the status does not say when it started.
        status.shim_started = None;
        assert_eq!(
            shim_identity(&status),
            if mine.is_some() {
                ShimIdentity::NoRecord
            } else {
                ShimIdentity::NoPlatform
            }
        );
        assert_eq!(
            status.observed().state,
            if crate::sys::proc::start_time(me).is_some() {
                // The platform can tell, so "no record" is not proof of life.
                STATE_LOST
            } else {
                STATE_RUNNING
            }
        );

        if mine.is_some() {
            // A pid whose process started years after the recorded time is a
            // recycled pid, even though the job's own `started` is close.
            status.shim_started = Some("2020-01-01T00:00:00Z".into());
            status.started = crate::log::event::now_ts();
            assert_eq!(shim_identity(&status), ShimIdentity::Different);
            assert_eq!(status.observed().state, STATE_LOST);
            // Identity never depends on `started` once the shim is gone.
            status.started = (time::OffsetDateTime::now_utc() - time::Duration::seconds(45))
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap();
            assert_eq!(shim_identity(&status), ShimIdentity::Different);
        }

        // No shim at all is nobody's: the missing pid is caught before the
        // platform check, so this is `NoRecord` on every OS.
        status.shim = None;
        assert_eq!(shim_identity(&status), ShimIdentity::NoRecord);
        assert_ne!(status.observed().state, STATE_RUNNING);
    }

    /// The command's own record is what `cancel` proves a lost job's tree by:
    /// without it, nothing is signalled.
    #[test]
    fn a_command_pid_is_only_trusted_with_its_own_start_time() {
        let me = std::process::id();
        let mut status = sample();
        status.pid = Some(me);
        status.pid_started = crate::sys::proc::start_time_text(me);
        // macOS cannot report a process start time, so a live pid is only
        // `NoPlatform` there; Windows and Linux still have to prove `Same`.
        let recorded = if crate::sys::proc::start_time(me).is_some() {
            ShimIdentity::Same
        } else {
            ShimIdentity::NoPlatform
        };
        assert_eq!(pid_identity(&status), recorded);
        status.pid_started = Some("2020-01-01T00:00:00Z".into());
        // A recorded start time that does not match the live pid is a
        // recycled pid on every platform that can report start times.
        assert_eq!(
            pid_identity(&status),
            if crate::sys::proc::start_time(me).is_some() {
                ShimIdentity::Different
            } else {
                ShimIdentity::NoPlatform
            }
        );
        // Without a record there is nothing to compare on platforms that can report start times; on macOS
        // `NoPlatform` is decided first, even with no record.
        status.pid_started = None;
        assert_eq!(
            pid_identity(&status),
            if crate::sys::proc::start_time(me).is_some() {
                ShimIdentity::NoRecord
            } else {
                ShimIdentity::NoPlatform
            }
        );
        status.pid = Some(0);
        assert_eq!(pid_identity(&status), ShimIdentity::NoRecord);
    }

    #[test]
    fn ids_and_wake_text_are_stable() {
        let id = new_id();
        assert!(id.starts_with("bg_"), "{id}");
        let body = &id["bg_".len()..];
        assert_eq!(body.len(), 26);
        assert!(
            body.chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        );
        assert_ne!(new_id(), new_id());
        let dir = temp_dir("wake-pins");
        let out = out_path(&dir, "bg_1");
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        std::fs::write(&out, "hello from the job\n").unwrap();
        let mut status = sample();
        status.id = "bg_1".into();
        status.out = path_string(&out);
        status.state = STATE_CANCELLED.into();
        let text = wake_text(&status);
        assert!(text.starts_with("[bg job bg_1 was cancelled"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
