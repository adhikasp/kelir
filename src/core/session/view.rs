//! Message folding, attachment import and view checks.
// These private loop modules expose crate-visible helpers to the feature modules.
#![allow(clippy::redundant_pub_crate)]

use super::*;

impl Session {
    /// Log the verb (applied or not), validate it and answer the client. The
    /// verb is *applied* later, at its boundary.
    /// Messages that should start the next turn: `later` deliveries, and any
    /// `say` that was still waiting when the turn ended.
    pub(crate) fn take_turn_messages(&mut self) -> Option<TurnStart> {
        let mut messages = Vec::new();
        let mut end = false;
        let mut woke = false;
        for pending in self
            .scheduler
            .take_where(|verb| verb.applies_at(Boundary::TurnEnd) && !verb.is_action())
        {
            match pending.verb {
                Verb::Say { text, now } => messages.push(Message {
                    text,
                    delivery: if now { "now" } else { "next" },
                    from: pending.from,
                }),
                Verb::Later { text } => messages.push(Message {
                    text,
                    delivery: "later",
                    from: pending.from,
                }),
                // A job report that arrived while the turn was ending: the
                // wake message is written here, and the next turn reads it.
                Verb::Bg(status) => woke |= self.note_bg(&status).unwrap_or(false),
                Verb::Child(note) => woke |= self.note_child(&note).unwrap_or(false),
                Verb::End => end = true,
                Verb::Brief { .. }
                | Verb::Stop { .. }
                | Verb::Status
                | Verb::Cancel { .. }
                | Verb::Model { .. }
                | Verb::Tools { .. }
                | Verb::Compact { .. }
                | Verb::Skills { .. }
                | Verb::View { .. } => {}
            }
        }
        if end {
            Some(TurnStart::End)
        } else if !messages.is_empty() {
            Some(TurnStart::Messages(messages))
        } else if woke {
            // The message is already in the log; the turn reads it from the
            // fold like any other.
            Some(TurnStart::Messages(Vec::new()))
        } else {
            None
        }
    }

    /// the `user` message from `job` that wakes the agent.
    pub(crate) fn note_bg(
        &mut self,
        reported: &crate::features::bg::Status,
    ) -> Result<bool, String> {
        let id = reported.id.clone();
        // The id is this session's own or nothing: a status file nobody minted never reaches the log
        // as a job. Same-user children are trusted; ownership is the only check here.
        if !self.trust.owns(&id) {
            return Ok(false);
        }
        // The file is the truth as of now, the argument is a snapshot from when the report was made.
        let dir = crate::paths::calls_dir(&self.log.dir);
        let file = crate::features::bg::Status::read(&crate::features::bg::status_path(&dir, &id));
        // A final record is terminal: the shim keeps it beside the status file so a running record
        // put back later cannot un-settle a job.
        let final_status =
            crate::features::bg::Status::read(&crate::features::bg::final_path(&dir, &id))
                .filter(|last| last.id == id && last.is_job() && last.settled());
        let candidate = final_status.as_ref().or(file.as_ref()).unwrap_or(reported);
        if candidate.id != id
            || !candidate.out.trim().is_empty() && !self.trust.inside_bg(&candidate.out)
        {
            return Ok(false);
        }
        let status = crate::features::bg::observed_in(&dir, candidate);
        let started = self.log.events.iter().any(|event| {
            matches!(&event.body, EventBody::CallStarted(start)
                if start.kind == "bg" && start.id == status.id)
        });
        if !started {
            self.record(crate::features::bg::started_body(&status))?;
        }
        if !status.settled() {
            self.sync();
            return Ok(false);
        }
        // Settle once. The log is the record: a repeated report (a stale watcher pass, a child
        // second event, no second wake message.
        let settled = self.log.events.iter().any(|event| {
            matches!(&event.body, EventBody::CallSettled(settle)
                if settle.kind == "bg" && settle.id == status.id)
        });
        if settled {
            return Ok(false);
        }
        self.record(crate::features::bg::settled_body(&status))?;
        let text = crate::features::bg::wake_text(&status);
        self.notify("bg_settled", &text);
        self.record(EventBody::User {
            text: Payload::text(text.clone()),
            from: "job".into(),
            delivery: "next".into(),
            attachments: Vec::new(),
        })?;
        self.ui_note(format!("» {}", crate::util::one_line_visible(&text, 200)));
        self.sync();
        Ok(true)
    }

    /// A wake-up the session raised itself (its own `bg/` watcher) is not a request: there is no `Request`
    /// to answer and nobody is waiting.
    pub(crate) fn note_own(&self, action: Option<crate::ipc::Action>) -> Option<Pending> {
        action.map(|crate::ipc::Action::Note(status)| Pending {
            // The watcher wrote no `verb` event, so there is no seq of its own; the scheduler keeps
            // arrival order and `note_bg` writes the `bg_*` events in the order the notes apply.
            seq: 0,
            verb: Verb::Bg(Box::new(status)),
            from: "job".into(),
        })
    }

    /// One image queued by `kelir view`: resolve the path, record the `source` event (kind
    /// `attachment`), and return what the TUI shows.
    pub(crate) fn apply_view(&mut self, named: &str) -> Result<(String, u64), String> {
        let path = self.check_view(named)?;
        let bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        // Audio and video are stored with a note, never sent: a `source` event
        // of kind `attachment-note` whose text says where the file is.
        if let Some(kind) = crate::features::attachments::media_kind(&path) {
            let seq = crate::features::attachments::next_seq(&self.log.dir);
            let rel = crate::features::attachments::save_media(&self.log.dir, seq, &path)
                .map_err(|error| format!("view: {error}"))?;
            self.record(EventBody::Source {
                kind: crate::features::attachments::MEDIA_SOURCE_KIND.to_string(),
                name: rel.clone(),
                text: Some(Payload::text(crate::features::attachments::media_note(
                    kind, &rel, bytes,
                ))),
                path: Some(rel.clone()),
            })?;
            self.sync();
            return Ok((rel, bytes));
        }
        // The image is copied into the session's attachments dir, so the request does not depend on
        // a path outside the session (and a later resume still finds it).
        let rel = self.import_attachment(&path)?;
        self.record(EventBody::Source {
            kind: "attachment".to_string(),
            name: rel.clone(),
            text: None,
            path: Some(rel.clone()),
        })?;
        self.sync();
        Ok((rel, bytes))
    }

    /// Resolve and validate a `view` path without side effects: the same check
    /// the ack runs, so the CLI's answer is the truth.
    pub(crate) fn check_view(&self, named: &str) -> Result<std::path::PathBuf, String> {
        let path = crate::features::attachments::resolve(&self.log.dir, &self.ctx.cwd, named);
        if !path.is_file() {
            return Err(format!(
                "view: no such image: {}",
                crate::util::path_string(&path)
            ));
        }
        if crate::features::attachments::media_kind(&path).is_some() {
            let bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            if bytes > crate::features::attachments::MEDIA_MAX_BYTES {
                return Err(format!(
                    "view: {} is {} (the limit for audio and video is {})",
                    crate::util::path_string(&path),
                    crate::features::attachments::bytes_label(bytes),
                    crate::features::attachments::bytes_label(
                        crate::features::attachments::MEDIA_MAX_BYTES
                    )
                ));
            }
            return Ok(path);
        }
        let mime = crate::features::attachments::mime_of(&path);
        if mime == "application/octet-stream" {
            // A PDF is refused on purpose: kelir sends no document
            // formats, only images; the message says what to do instead.
            let pdf = path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));
            return Err(format!(
                "view: {} is not an image kelir knows (png, jpg, gif, webp){}",
                crate::util::path_string(&path),
                if pdf {
                    "; a PDF is not read: extract its text with a tool, or render pages to png and view those"
                } else {
                    ""
                }
            ));
        }
        let bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        if bytes > crate::features::attachments::MAX_BYTES {
            return Err(format!(
                "view: {} is {} (the limit is {})",
                crate::util::path_string(&path),
                crate::features::attachments::bytes_label(bytes),
                crate::features::attachments::bytes_label(crate::features::attachments::MAX_BYTES)
            ));
        }
        // An extension is a claim, not proof: the bytes have to start with a format's signature and carry a
        // header kelir can read a size from.
        match crate::features::attachments::sniff(&path) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(format!(
                    "view: {} does not look like an image (empty, or not a png, jpg, gif or webp)",
                    crate::util::path_string(&path)
                ));
            }
            Err(message) => return Err(format!("view: {message}")),
        }
        // `None` (the provider did not state the model's vision support) is a
        // "try it": refusing on a missing flag would block a model that works.
        if self.vision_support() == Some(false) {
            return Err(format!(
                "view: {} cannot take images: it is a text-only model, so the image would be refused by the provider (pick a vision model with `/model`, or `--model`). The conversation is unchanged.",
                self.provider.model
            ));
        }
        Ok(path)
    }

    /// Whether the provider says the current model takes images: `Some(true)` and `Some(false)` are
    /// answers, `None` is "not stated" (the fetched or cached model list has no entry, or the entry has no
    pub(crate) fn vision_support(&self) -> Option<bool> {
        self.features
            .models
            .models
            .as_ref()
            .and_then(|models| models.vision_for(&self.provider.model))
    }

    /// Copy one image into `<session>/attachments/` and return its relative
    /// path. Content that is already there is not copied twice.
    pub(crate) fn import_attachment(&self, path: &Path) -> Result<String, String> {
        let dir = crate::features::attachments::dir(&self.log.dir);
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("view: cannot create {}: {error}", dir.display()))?;
        // Keep the file where it is when it already lives in the session's
        // attachments (a prompt pointer names exactly that).
        if let Ok(rest) = path.strip_prefix(&self.log.dir) {
            return Ok(crate::util::path_string(rest));
        }
        // The sequence number counts the session's copies, so two different files get two numbers.
        let mut seen = std::collections::HashMap::new();
        let seq = crate::features::attachments::next_seq(&self.log.dir);
        let saved = crate::features::attachments::save(&self.log.dir, seq, path, &mut seen)
            .map_err(|error| format!("view: {error}"))?;
        Ok(saved.rel)
    }

    /// Record one `source` event: context that arrived after the start. It renders as a message
    /// at the tail, so the system prefix (and its cache) is untouched.
    /// The per-boundary mtime/hash check of the recorded sources: a file edited on disk since it went into
    /// the context becomes one `source` message.
    pub(crate) async fn record_messages(&mut self, messages: Vec<Message>) -> Result<(), String> {
        for mut message in messages {
            let from = message.from.clone();
            let Some(text) = self.gates_prompt(message.text, &from).await? else {
                continue;
            };
            message.text = text;
            if message.from != "tui" {
                self.ui_note(format!(
                    "› {}",
                    crate::util::one_line_visible(&message.text, 200)
                ));
            }
            // A message deferred to the end of a turn (`/later`) is the same message: the list of the images its
            // text points at goes with it , or a queued prompt would be the one shape whose `attachments[]`
            let attachments = crate::features::attachments::paths_in(&message.text)
                .into_iter()
                .map(serde_json::Value::String)
                .collect();
            self.record(EventBody::User {
                text: Payload::text(message.text),
                from: message.from,
                delivery: message.delivery.to_string(),
                attachments,
            })?;
            self.gates_do(|gates, session| gates.flush_context(session));
        }
        self.sync();
        Ok(())
    }
}
