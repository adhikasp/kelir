//! Tool-child process grouping, so a stop can kill a whole tree.
//! Every tool child is `kelir bg-shim`, which owns the command it runs: the shim puts the command
//! in its own process group on Unix and in a Job Object on Windows, so the shim can take the tree
//! down by itself, and its owner can signal intent by killing the shim's group / job.

pub mod core;
#[cfg(windows)]
pub mod win;

pub use core::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn killing_a_group_twice_reports_once() {
        let group = ProcessGroup::new();
        assert!(!group.is_killed());
        assert!(group.kill());
        assert!(group.is_killed());
        assert!(!group.kill());
    }

    #[test]
    fn a_detached_group_is_never_killed() {
        let group = ProcessGroup::new();
        group.detach();
        assert!(group.is_detached());
        assert!(!group.kill(), "a promoted job is not the session's to kill");
        assert!(!group.is_killed());
    }

    #[test]
    fn the_running_registry_tracks_and_kills_keys() {
        let running = Running::new();
        assert!(running.keys().is_empty());
        running.insert("7-1-1", ProcessGroup::new());
        assert_eq!(running.keys(), vec!["7-1-1".to_string()]);
        assert!(running.kill("7-1-1"));
        assert!(!running.kill("nope"));
        assert_eq!(running.kill_all(), 0, "the group was already killed");
        running.remove("7-1-1");
        assert!(running.keys().is_empty());
    }

    #[test]
    fn kept_groups_are_killed_when_the_session_handle_goes_away() {
        let group = ProcessGroup::new();
        let observed = group.clone();
        {
            let groups = SessionGroups::new();
            groups.keep(group);
            assert!(!observed.is_killed());
        }
        assert!(observed.is_killed());
    }

    #[test]
    fn a_kept_group_that_was_promoted_survives_the_session() {
        let group = ProcessGroup::new();
        let observed = group.clone();
        {
            let groups = SessionGroups::new();
            groups.keep(group);
            // Promotion detaches it: the session no longer owns the job.
            observed.detach();
        }
        assert!(
            !observed.is_killed(),
            "a promoted job died with the session"
        );
    }
}
