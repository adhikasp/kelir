//! Memory: a configured directory of notes, loaded as sources (M5-B).
//!
//! `memory_dir` in the config names a directory of `.md` / `.txt` files. At
//! `session_started` they are rendered into the frozen system (a `# Memory`
//! section) and recorded in `sources[]` with kind `memory`. The model writes
//! memory with `edit` like any other file; the per-boundary check that already
//! watches instruction files and skills sees the change and appends it as a
//! `source` message (kind `memory`) at the tail, never as a system rewrite. A
//! file *created* mid-session — the common case, a new note — is found by
//! [`unseen`] and announced the same way.
//!
//! Bounded on purpose: memory is context every request pays for. A file is cut
//! at [`MAX_FILE_BYTES`] with a visible notice, the directory at
//! [`MAX_TOTAL_BYTES`] and [`MAX_FILES`] files; whatever is left out is named
//! in a note, never silently dropped.

use std::path::{Path, PathBuf};

use crate::sources::{Found, Layer, SourceKind, Tracked};

/// The most one memory file contributes.
pub const MAX_FILE_BYTES: usize = 16 * 1024;
/// The most the whole directory contributes to the frozen system.
pub const MAX_TOTAL_BYTES: usize = 64 * 1024;
/// How many files are read from the directory.
pub const MAX_FILES: usize = 64;

/// Resolve the configured directory: `~/` was expanded by the config; a
/// relative path is the session cwd's.
pub fn resolve(configured: &Path, cwd: &Path) -> PathBuf {
    if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        cwd.join(configured)
    }
}

fn wanted(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.starts_with('.') {
        return false;
    }
    matches!(
        path.extension()
            .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
            .as_deref(),
        Some("md") | Some("txt")
    )
}

/// The memory files of `dir`, sorted by name, as discovered sources. A missing
/// or unreadable directory is no memory (and a note when it exists but cannot
/// be read); nothing here ever fails a session.
pub fn scan(dir: &Path, notes: &mut Vec<String>) -> Vec<Found> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            if dir.exists() {
                notes.push(format!(
                    "memory: cannot read {}: {error}",
                    crate::util::path_string(dir)
                ));
            }
            return Vec::new();
        }
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && wanted(path))
        .collect();
    files.sort();
    if files.len() > MAX_FILES {
        notes.push(format!(
            "memory: {} has {} files; only the first {MAX_FILES} (by name) are read",
            crate::util::path_string(dir),
            files.len()
        ));
        files.truncate(MAX_FILES);
    }
    let mut budget = MAX_TOTAL_BYTES;
    let mut out = Vec::new();
    for path in files {
        let Some(text) = crate::sources::read_file(&path, MAX_FILE_BYTES, &mut budget, notes)
        else {
            continue;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push(Found {
            kind: SourceKind::Memory,
            name,
            path,
            text,
            layer: Layer::Project,
            builtin: false,
        });
    }
    out
}

/// Memory files in `dir` that the context has not heard of yet: the notes the
/// model (or a person) created since the session started. The caller announces
/// each as a `source` message and starts tracking it.
pub fn unseen(dir: &Path, tracked: &[Tracked]) -> Vec<Found> {
    let mut notes = Vec::new();
    scan(dir, &mut notes)
        .into_iter()
        .filter(|found| {
            !tracked
                .iter()
                .any(|known| known.kind == SourceKind::Memory && known.name == found.name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kelir-memory-{}-{name}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn only_visible_md_and_txt_files_are_memory_and_they_are_sorted() {
        let dir = temp("scan");
        for (name, text) in [
            ("b.md", "bee"),
            ("a.txt", "ay"),
            (".hidden.md", "no"),
            ("c.png", "no"),
            ("noext", "no"),
        ] {
            std::fs::write(dir.join(name), text).unwrap();
        }
        std::fs::create_dir_all(dir.join("sub.md")).unwrap();
        let mut notes = Vec::new();
        let found = scan(&dir, &mut notes);
        let names: Vec<&str> = found.iter().map(|found| found.name.as_str()).collect();
        assert_eq!(names, ["a.txt", "b.md"]);
        assert!(found.iter().all(|found| found.kind == SourceKind::Memory));
        assert!(notes.is_empty(), "{notes:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_directory_is_no_memory_and_no_note() {
        let dir = temp("missing").join("nope");
        let mut notes = Vec::new();
        assert!(scan(&dir, &mut notes).is_empty());
        assert!(notes.is_empty());
    }

    #[test]
    fn a_big_file_is_cut_with_a_notice_and_the_directory_has_a_budget() {
        let dir = temp("big");
        std::fs::write(dir.join("big.md"), "x".repeat(MAX_FILE_BYTES * 2)).unwrap();
        let mut notes = Vec::new();
        let found = scan(&dir, &mut notes);
        assert_eq!(found.len(), 1);
        assert!(
            found[0].text.contains("[kelir: truncated;"),
            "{}",
            found[0].text.len()
        );
        assert!(!notes.is_empty());
        // Many files: the total budget leaves later ones out, with a note.
        let dir2 = temp("many");
        for index in 0..8 {
            std::fs::write(
                dir2.join(format!("n{index}.md")),
                "y".repeat(MAX_FILE_BYTES),
            )
            .unwrap();
        }
        let mut notes = Vec::new();
        let found = scan(&dir2, &mut notes);
        assert!(found.len() < 8, "{}", found.len());
        assert!(
            notes.iter().any(|note| note.contains("budget")),
            "{notes:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn a_new_file_is_unseen_until_it_is_tracked() {
        let dir = temp("unseen");
        std::fs::write(dir.join("a.md"), "one").unwrap();
        let found = scan(&dir, &mut Vec::new());
        let mut watch = crate::sources::Watch::default();
        for item in &found {
            watch.add(
                item.kind,
                &item.name,
                &crate::sources::SourcePath::File(item.path.clone()),
                &crate::sources::hash_of(&item.text),
            );
        }
        assert!(unseen(&dir, watch.tracked()).is_empty());
        std::fs::write(dir.join("b.md"), "two").unwrap();
        let fresh = unseen(&dir, watch.tracked());
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].name, "b.md");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_relative_directory_is_the_cwds() {
        let cwd = Path::new("work");
        assert_eq!(resolve(Path::new("mem"), cwd), cwd.join("mem"));
    }
}
