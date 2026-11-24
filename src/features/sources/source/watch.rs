//! Sources: the base prompt, instruction files and skills directories.
//! The system prompt is rendered **once** at `session_started` and then frozen: a source
//! edited on disk mid-session arrives as a `source` event (a message at the tail), never as
//! a rewrite of the prefix, because prompt caching is the point of rendering once.

use std::path::{Path, PathBuf};

use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct Tracked {
    pub kind: SourceKind,
    pub name: String,
    pub path: SourcePath,
    /// The hash of the text this context **currently** carries for the source: the frozen system's, or the
    /// newest `source` message's.
    pub hash: String,
    /// A compaction folded the source's message away: the next boundary says
    /// what the file holds now, even if it holds the same bytes.
    pub stale: bool,
    /// The file was already reported as gone, so a missing source is one note,
    /// not one per boundary .
    pub gone: bool,
}

/// A source whose file was deleted: the context still carries its last text, which is now
/// nothing on disk. Reported once, so a reader knows why the model still believes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Removal {
    pub kind: SourceKind,
    pub name: String,
    pub path: PathBuf,
}

/// A source edited on disk since it went into the context.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub kind: SourceKind,
    pub name: String,
    pub path: PathBuf,
    pub text: String,
    /// What the re-read had to say beyond the text (a truncation, invalid
    /// UTF-8): recorded next to the `source` event, never dropped silently.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Watch {
    tracked: Vec<Tracked>,
}

impl Watch {
    /// Track the sources the system prompt was rendered from.
    pub fn from_rendered(rendered: &Rendered) -> Watch {
        Watch::from_sources(&rendered.sources)
    }

    /// Track the sources `session_started` recorded.
    pub fn from_sources(sources: &[SourceInfo]) -> Watch {
        let mut watch = Watch::default();
        for source in sources {
            let kind = kind_of(source);
            watch.add(
                kind,
                &source.name,
                &SourcePath::from_log(kind, &source.path),
                &source.hash,
            );
        }
        watch
    }

    /// Rebuild the watcher from a log: the frozen `sources[]` first, then every `source` event in
    /// order, so the context's *current* word on each source is the last message it carries .
    pub fn rebuild(events: &[LogEvent], dir: &Path) -> Watch {
        let mut watch = Watch::from_sources(&sources_of(events));
        for event in events {
            let crate::log::event::EventBody::Source {
                kind,
                name,
                text,
                path,
            } = &event.body
            else {
                continue;
            };
            // A pushed hint and a media note are context, not files: nothing
            // on disk is watched for them.
            if matches!(kind.as_str(), "hint" | "attachment-note") {
                continue;
            }
            let kind = SourceKind::parse(kind);
            let Some(text) = text else {
                continue;
            };
            let text = text.resolve_text(dir).unwrap_or_default();
            let path = match path.as_deref() {
                Some(path) => SourcePath::from_log(kind, path),
                None => SourcePath::Embedded,
            };
            watch.add(kind, name, &path, &hash_of(&text));
        }
        // A `source` event a standing compaction folds into its summary is not in the context any more: the
        // resume says what the file holds at its first boundary, exactly as a live session would after the
        for event in events {
            if let crate::log::event::EventBody::Compacted {
                through_seq,
                pinned_seqs,
                ..
            } = &event.body
            {
                watch.forget_folded(events, *through_seq, pinned_seqs);
            }
        }
        watch
    }

    pub fn add(&mut self, kind: SourceKind, name: &str, path: &SourcePath, hash: &str) {
        if let Some(existing) = self
            .tracked
            .iter_mut()
            .find(|tracked| tracked.kind == kind && tracked.name == name)
        {
            existing.path = path.clone();
            existing.hash = hash.to_string();
            existing.stale = false;
            existing.gone = false;
            return;
        }
        self.tracked.push(Tracked {
            kind,
            name: name.to_string(),
            path: path.clone(),
            hash: hash.to_string(),
            stale: false,
            gone: false,
        });
    }

    /// The context no longer carries this source's text: announce the file as
    /// it is at the next boundary, even though its bytes did not change.
    pub fn forget(&mut self, name: &str) {
        for tracked in &mut self.tracked {
            if tracked.name == name {
                tracked.stale = true;
            }
        }
    }

    /// A compaction folded every message up to `through_seq` into a summary, so a `source` event at or
    /// below it (rendered from a pinned user message excepted) is no longer in the context.
    pub fn forget_folded(&mut self, events: &[LogEvent], through_seq: u64, pinned: &[u64]) {
        for event in events {
            if event.seq > through_seq || pinned.contains(&event.seq) {
                continue;
            }
            if let crate::log::event::EventBody::Source { name, .. } = &event.body {
                self.forget(name);
            }
        }
    }

    pub fn tracked(&self) -> &[Tracked] {
        &self.tracked
    }

    /// Every tracked source whose file has disappeared since the context last heard about it,
    /// once per disappearance .
    pub fn removals(&mut self) -> Vec<Removal> {
        let mut out = Vec::new();
        for tracked in &mut self.tracked {
            if tracked.gone {
                continue;
            }
            let Some(path) = tracked.path.file() else {
                continue;
            };
            if path.is_file() {
                continue;
            }
            tracked.gone = true;
            out.push(Removal {
                kind: tracked.kind,
                name: tracked.name.clone(),
                path: path.to_path_buf(),
            });
        }
        out
    }

    /// Every source whose current content this context does not carry yet.
    pub fn changes(&mut self) -> Vec<Change> {
        let mut out = Vec::new();
        for tracked in &mut self.tracked {
            let Some(path) = tracked.path.file().map(Path::to_path_buf) else {
                continue;
            };
            let Some((text, notes)) = read_source(&path, limit_for(tracked.kind)) else {
                continue;
            };
            let hash = hash_of(&text);
            tracked.gone = false;
            if !tracked.stale && tracked.hash == hash {
                continue;
            }
            tracked.hash = hash;
            tracked.stale = false;
            out.push(Change {
                kind: tracked.kind,
                name: tracked.name.clone(),
                path,
                text,
                notes,
            });
        }
        out
    }
}

/// The `sources[]` of the log's `session_started`.
pub fn sources_of(events: &[LogEvent]) -> Vec<SourceInfo> {
    events
        .iter()
        .find_map(|event| match &event.body {
            crate::log::event::EventBody::SessionStarted { sources, .. } => Some(sources.clone()),
            _ => None,
        })
        .unwrap_or_default()
}
