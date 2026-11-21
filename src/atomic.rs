//! Small file primitives shared by the names store, the job lock and STATUS.
//! Two operations, both all-or-nothing as far as a reader can tell:

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp name beside `path`: dot-prefixed (listings skip it), unique to this
/// process and call.
pub fn temp_beside(path: &Path) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let stem = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    path.with_file_name(format!(".{stem}.{}.{n}.tmp", std::process::id()))
}

fn write_temp(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let tmp = temp_beside(path);
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    match result {
        Ok(()) => Ok(tmp),
        Err(error) => {
            let _ = std::fs::remove_file(&tmp);
            Err(error)
        }
    }
}

/// Replace `path` with `bytes` atomically.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = write_temp(path, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// What [`publish_new`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Published {
    /// The file did not exist; it does now, complete.
    Created,
    /// Somebody else's file is there; nothing was changed.
    Exists,
}

/// Create `path` with `bytes` unless it exists.
pub fn publish_new(path: &Path, bytes: &[u8]) -> io::Result<Published> {
    let tmp = write_temp(path, bytes)?;
    let linked = std::fs::hard_link(&tmp, path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(Published::Created),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(Published::Exists),
        Err(_) => {
            // No hard links here (a share, a FAT volume): create-new is still
            // exclusive, only the content arrives a moment later.
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut file) => {
                    file.write_all(bytes)?;
                    file.sync_all()?;
                    Ok(Published::Created)
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(Published::Exists),
                Err(error) => Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn publish_new_creates_once_and_never_replaces() {
        let temp = dir();
        let path = temp.path().join("f");
        assert_eq!(publish_new(&path, b"one").unwrap(), Published::Created);
        assert_eq!(publish_new(&path, b"two").unwrap(), Published::Exists);
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        // No temp file is left behind by either call.
        let names: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp() {
        let temp = dir();
        let path = temp.path().join("f");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn many_threads_publishing_one_name_produce_exactly_one_winner() {
        let temp = dir();
        let path = std::sync::Arc::new(temp.path().join("lock"));
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || {
                    publish_new(&path, format!("owner {n}").as_bytes()).unwrap()
                })
            })
            .collect();
        let won = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|outcome| *outcome == Published::Created)
            .count();
        assert_eq!(won, 1);
        let text = std::fs::read_to_string(&*path).unwrap();
        assert!(text.starts_with("owner "), "{text}");
    }
}
