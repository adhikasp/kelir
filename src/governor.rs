//! Client-side concurrency governor: at most N requests per provider across every local kelir
//! process.
//! Several local workers sharing one account hit the provider's own limit together (the 429 wave
//! of 2026-11-19).
//! The governor is opt-in (`[providers.<name>] max_concurrent_requests = N`) and advisory: a slot

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::paths;

/// One held slot. Dropping it (or the OS reaping the process) frees the slot.
#[derive(Debug)]
pub struct Slot {
    /// Kept open for the life of the guard: closing the handle is the unlock.
    _file: File,
    pub index: u32,
    path: PathBuf,
}

impl Slot {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// `max_concurrent_requests` slots for one provider, under the state root.
#[derive(Debug, Clone)]
pub struct Governor {
    dir: PathBuf,
    provider: String,
    slots: u32,
}

impl Governor {
    /// A governor for `provider` with `slots` slots.
    pub fn new(provider: &str, slots: u32) -> Self {
        Self {
            dir: paths::state_dir().join("locks"),
            provider: safe_name(provider),
            slots: slots.max(1),
        }
    }

    /// The same, with the lock directory fixed: the tests point it at a
    /// scratch dir instead of the machine's state root.
    pub fn with_dir(dir: PathBuf, provider: &str, slots: u32) -> Self {
        Self {
            dir,
            provider: safe_name(provider),
            slots: slots.max(1),
        }
    }

    pub fn slots(&self) -> u32 {
        self.slots
    }

    pub fn path_for(&self, index: u32) -> PathBuf {
        self.dir.join(format!("{}-{index}", self.provider))
    }

    /// Take any free slot, or `None` when every slot is held (by this process or another one).
    pub fn try_acquire(&self) -> Option<Slot> {
        for index in 0..self.slots {
            let path = self.path_for(index);
            if let Some(parent) = path.parent()
                && std::fs::create_dir_all(parent).is_err()
            {
                return None;
            }
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&path)
                .ok()?;
            if std::fs::File::try_lock(&file).is_ok() {
                return Some(Slot {
                    _file: file,
                    index,
                    path,
                });
            }
        }
        None
    }
}

/// A provider name as a file name: letters, digits, `-`, `_`, `.`; anything
/// else becomes `-`.
fn safe_name(provider: &str) -> String {
    let mut out = String::with_capacity(provider.len());
    for c in provider.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    if out.is_empty() {
        out.push_str("provider");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn two_holders_never_share_a_slot() {
        let dir = temp();
        let governor = Governor::with_dir(dir.path().to_path_buf(), "opencode-go", 1);
        let first = governor.try_acquire().expect("the only slot");
        assert_eq!(first.index, 0);
        assert_eq!(first.path(), governor.path_for(0));
        assert!(
            governor.try_acquire().is_none(),
            "a second holder must not get the same slot"
        );
        drop(first);
        // macOS can report the lock as held for a moment after the guard
        // closes its descriptor; retry briefly before failing.
        let mut again = None;
        for _ in 0..50 {
            again = governor.try_acquire();
            if again.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let again = again.expect("the slot is free again");
        assert_eq!(again.index, 0);
    }

    #[test]
    fn n_slots_allow_exactly_n_holders() {
        let dir = temp();
        let governor = Governor::with_dir(dir.path().to_path_buf(), "go", 3);
        let held: Vec<Slot> = (0..3).filter_map(|_| governor.try_acquire()).collect();
        assert_eq!(held.len(), 3);
        assert_eq!(
            held.iter().map(|slot| slot.index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(governor.try_acquire().is_none(), "the fourth must wait");
        drop(held);
        assert_eq!(governor.slots(), 3);
        // macOS can still report the just-released flocks as held briefly.
        let mut again = None;
        for _ in 0..50 {
            again = governor.try_acquire();
            if again.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(again.is_some(), "a slot must be free after drop");
    }

    #[test]
    fn a_provider_name_cannot_name_a_directory() {
        let dir = temp();
        let governor = Governor::with_dir(dir.path().to_path_buf(), "../evil name", 1);
        let path = governor.path_for(0);
        assert_eq!(path.parent(), Some(dir.path()));
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "..-evil-name-0"
        );
    }

    #[test]
    fn a_zero_slot_governor_still_has_one_slot() {
        let dir = temp();
        let governor = Governor::with_dir(dir.path().to_path_buf(), "go", 0);
        assert_eq!(governor.slots(), 1);
        assert!(governor.try_acquire().is_some());
    }

    #[test]
    fn the_state_root_holds_the_slots() {
        let governor = Governor::new("opencode-go", 2);
        assert!(
            governor
                .path_for(1)
                .starts_with(paths::state_dir().join("locks"))
        );
        assert_eq!(
            governor.path_for(1).file_name().unwrap().to_string_lossy(),
            "opencode-go-1"
        );
    }
}
