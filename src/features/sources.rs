//! Sources: machine/project files, skills, memory and their on-disk watcher.
//! The watcher and every `source` event belong to this feature; the core only
//! calls the boundary at `before_step`.

use crate::core::boundary::{Cx, Feature};
use crate::event::{EventBody, Payload};
use crate::tui::{View, note_event};

pub struct Sources {
    watch: crate::sources::Watch,
}

impl Sources {
    pub fn new(watch: crate::sources::Watch) -> Self {
        Self { watch }
    }

    /// Called by compaction when a range of events has been folded away.
    pub fn forget_folded(
        &mut self,
        events: &[crate::event::LogEvent],
        through: u64,
        pinned: &[u64],
    ) {
        self.watch.forget_folded(events, through, pinned);
    }

    /// `skills`: read one skill or instruction source (from a file, a skill directory, or the
    /// verb's own text) and add it to the context as a `source` message.
    pub fn apply_skills(
        &mut self,
        kind: crate::sources::SourceKind,
        name: &str,
        path: Option<&str>,
        text: Option<&str>,
        cx: &mut Cx<'_>,
    ) -> Result<(), String> {
        let (body, resolved, notes) = match (text, path) {
            (Some(text), _) => (text.to_string(), None, Vec::new()),
            (None, Some(path)) => {
                let path = std::path::PathBuf::from(path);
                let file = if path.is_file() {
                    path.clone()
                } else {
                    path.join("SKILL.md")
                };
                let (body, notes) =
                    crate::sources::read_source(&file, crate::sources::limit_for(kind))
                        .ok_or_else(|| {
                            format!("skills: cannot read {}", crate::util::path_string(&file))
                        })?;
                // A skill's front matter must parse: a broken `SKILL.md` is refused here rather than handed
                // to the model as if it were one.
                if kind == crate::sources::SourceKind::Skill {
                    let skill = crate::sources::parse_skill(&body)?;
                    if skill.name != name {
                        return Err(format!(
                            "skills: {} names itself `{}`, not `{name}`",
                            crate::util::path_string(&file),
                            skill.name
                        ));
                    }
                }
                (body, Some(file), notes)
            }
            (None, None) => return Err("skills: nothing to add".into()),
        };
        let hash = crate::sources::hash_of(&body);
        if self.latest_source_is(cx, kind, name, &hash) {
            return Err(format!(
                "skills: `{name}` already carries exactly this text"
            ));
        }
        for note in notes {
            cx.record(EventBody::Error {
                location: "source".into(),
                message: note,
                retryable: false,
                status: None,
                attempt: None,
                retry_in_ms: None,
            })?;
        }
        let logged = match &resolved {
            Some(path) => crate::sources::SourcePath::File(path.clone()),
            None => crate::sources::SourcePath::Embedded,
        };
        self.record_source(kind, name, &body, &logged, cx)?;
        cx.emit(View::Log(note_event(format!("» skills: {name}"))));
        cx.sync();
        Ok(())
    }

    /// Does the *last* thing this context says about a source carry `hash`?
    pub fn latest_source_is(
        &self,
        cx: &Cx<'_>,
        kind: crate::sources::SourceKind,
        name: &str,
        hash: &str,
    ) -> bool {
        let last = cx.events.iter().rev().find_map(|event| match &event.body {
            EventBody::Source {
                kind: event_kind,
                name: event_name,
                text,
                ..
            } if event_name == name && crate::sources::SourceKind::parse(event_kind) == kind => {
                // A big body is spilled to a blob: read it back, or a
                // repeat of the current text would slip through.
                Some(
                    text.as_ref()
                        .and_then(|text| text.resolve_text(cx.dir).ok())
                        .map(|text| crate::sources::hash_of(&text)),
                )
            }
            _ => None,
        });
        matches!(last, Some(Some(seen)) if seen == hash)
    }

    pub fn check_sources(&mut self, cx: &mut Cx<'_>) -> Result<usize, String> {
        // A source whose file is gone is one note per disappearance, never a
        // silent hole .
        for removed in self.watch.removals() {
            cx.record(EventBody::Error {
                location: "source".into(),
                message: format!(
                    "source: {} ({}) was deleted; the context still carries its last text",
                    crate::util::path_string(&removed.path),
                    removed.name
                ),
                retryable: false,
                status: None,
                attempt: None,
                retry_in_ms: None,
            })?;
            cx.emit(View::Log(note_event(format!(
                "» source deleted on disk: {}",
                crate::util::path_string(&removed.path)
            ))));
            // The model curates its memory, so a note it can no longer find must not stay a live fact in
            // its context: a one-line tail message, the system prefix untouched.
            if removed.kind == crate::sources::SourceKind::Memory {
                // Recorded directly, not through `record_source`: that would
                // re-arm the watcher and announce the same absence every time.
                cx.record(EventBody::Source {
                    kind: removed.kind.event_kind().to_string(),
                    name: removed.name.clone(),
                    text: Some(Payload::text(format!(
                        "memory note {} was deleted; disregard its earlier text",
                        removed.name
                    ))),
                    path: Some(crate::util::path_string(&removed.path)),
                })?;
            }
        }
        let changes = self.watch.changes();
        let count = changes.len();
        for change in changes {
            for note in change.notes {
                cx.record(EventBody::Error {
                    location: "source".into(),
                    message: note,
                    retryable: false,
                    status: None,
                    attempt: None,
                    retry_in_ms: None,
                })?;
            }
            let path = change.path.clone();
            self.record_source(
                change.kind,
                &change.name,
                &change.text,
                &crate::sources::SourcePath::File(path.clone()),
                cx,
            )?;
            cx.emit(View::Log(note_event(format!(
                "» source changed on disk: {}",
                crate::util::path_string(&path)
            ))));
        }
        // A memory note created since the start (the common case: the model
        // wrote a new file) is announced like a changed one.
        let mut count = count;
        if let Some(dir) = cx.cfg.memory_dir.clone() {
            let dir = crate::memory::resolve(&dir, &cx.ctx.cwd);
            for found in crate::memory::unseen(&dir, self.watch.tracked()) {
                self.record_source(
                    found.kind,
                    &found.name,
                    &found.text,
                    &crate::sources::SourcePath::File(found.path.clone()),
                    cx,
                )?;
                cx.emit(View::Log(note_event(format!(
                    "» new memory note: {}",
                    crate::util::path_string(&found.path)
                ))));
                count += 1;
            }
        }
        if count > 0 {
            cx.sync();
        }
        Ok(count)
    }

    pub fn record_source(
        &mut self,
        kind: crate::sources::SourceKind,
        name: &str,
        text: &str,
        path: &crate::sources::SourcePath,
        cx: &mut Cx<'_>,
    ) -> Result<(), String> {
        cx.record(EventBody::Source {
            kind: kind.event_kind().to_string(),
            name: name.to_string(),
            text: Some(Payload::text(text)),
            path: path.file().map(crate::util::path_string),
        })?;
        // The context's current word on the source is now this message: the watcher compares against this one
        // hash, so the file on disk and the watcher agree at once (no second announcement) and an edit back to
        self.watch
            .add(kind, name, path, &crate::sources::hash_of(text));
        Ok(())
    }
}

impl Feature for Sources {
    fn name(&self) -> &'static str {
        "sources"
    }

    fn check_sources(&mut self, cx: &mut Cx<'_>) -> Result<(), String> {
        Sources::check_sources(self, cx).map(|_| ())
    }
}
