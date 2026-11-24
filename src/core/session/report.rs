//! Turn reporting and final state answers.
// These private loop modules expose crate-visible helpers to the feature modules.
#![allow(clippy::redundant_pub_crate)]

use super::*;

impl Session {
    /// a kill can leave a log whose last message is the assistant's final answer, and a request may not
    /// end on one — providers read a trailing assistant message as prefill or refuse it.
    pub(crate) fn continue_a_turn_that_ends_on_an_answer(&mut self) -> Result<(), String> {
        if !self.run.pending_task.trim().is_empty() {
            return Ok(());
        }
        let closed = self
            .log
            .events
            .iter()
            .any(|event| matches!(event.body, EventBody::SessionEnded { .. }));
        let continuing = !closed
            || matches!(
                resume::last_turn_outcome(&self.log.events).as_deref(),
                Some("stopped") | Some("paused") | Some("error")
            );
        if !continuing {
            return Ok(());
        }
        let folded = crate::log::fold::fold(&self.log.events, &self.log.dir)?;
        if folded.messages.last().map(|message| message.role.as_str()) != Some("assistant") {
            return Ok(());
        }
        self.record(EventBody::User {
            text: Payload::text(CONTINUE_NOTE),
            from: "cli".into(),
            delivery: "now".into(),
            attachments: Vec::new(),
        })?;
        self.run.has_task = true;
        Ok(())
    }

    /// The partial output of a call the previous process started, when its `calls/<seq>-*` directory
    /// still has one.
    pub(crate) fn interrupted_output(&self, call: &UnsettledCall) -> Option<(String, u64, u64)> {
        if !call.started {
            return None;
        }
        // The last start for this id, in case a provider reuses call ids.
        let seq = self
            .log
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.body {
                EventBody::CallStarted(start)
                    if start.kind == "tool" && start.id == call.call_id =>
                {
                    Some(event.seq)
                }
                _ => None,
            })?;
        let prefix = format!("{seq}-");
        let entry = std::fs::read_dir(crate::paths::calls_dir(&self.log.dir))
            .ok()?
            .flatten()
            .find(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))?;
        let out = entry.path().join("out");
        let partial = crate::log::spill::partial(&out).ok()?;
        if partial.bytes == 0 {
            return None;
        }
        Some((crate::util::path_string(&out), partial.bytes, partial.lines))
    }

    /// The gate's read-only view of this session, borrowed from the core.
    pub(crate) fn gate_ctx(&self) -> crate::features::gates::GateCtx<'_> {
        crate::features::gates::GateCtx {
            client: &self.provider.client,
            provider: &self.provider.active_provider,
            config: &self.cfg,
            provider_name: &self.provider.provider_name,
            note: &|_message| {},
            events: &self.log.events,
            dir: &self.log.dir,
            skills_dirs: &self.cfg.skills_dirs,
            config_dir: Some(&self.log.config_dir),
            cwd: &self.ctx.cwd,
        }
    }

    /// Append one event. Oversized payloads go to `blobs/` first, named by the
    /// seq the event is about to get.
    pub(crate) fn record(&mut self, body: EventBody) -> Result<LogEvent, String> {
        let seq = self.log.writer.next_seq();
        let title = if !self.log.titled
            && let EventBody::User { text, from, .. } = &body
            && from != "cli"
        {
            Some(crate::log::event::title_from_prompt(
                text.inline_str().unwrap_or(""),
            ))
        } else {
            None
        };
        let body = spill_payloads(body, &self.log.blobs, seq)?;
        let event = self
            .log
            .writer
            .append(body)
            .map_err(|error| format!("cannot append to the event log: {error}"))?;
        // One place to append and to publish: the live TUI and `attach` now
        // consume the very same event, with no translation layer between.
        self.emit(crate::tui::View::Log(event.clone()));
        if self.out.json {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let _ = writeln!(
                lock,
                "{}",
                serde_json::to_string(&event).unwrap_or_default()
            );
            let _ = lock.flush();
        }
        self.tally.observe(event.seq, &event.body);
        self.log.events.push(event.clone());
        // Name the session from its first prompt: deterministic, no model call.
        if let Some(title) = title {
            self.log.titled = true;
            self.record(EventBody::SessionTitle {
                title: title.clone(),
            })?;
            self.report_title(&title);
        }
        Ok(event)
    }

    pub(crate) fn warn(&self) -> crate::provider::usage::Warn {
        crate::provider::usage::Warn {
            prompt_tokens: self.cfg.warn_prompt_tokens,
        }
    }

    pub(crate) fn note_warnings(&mut self) {
        let warn = self.warn();
        for warning in self.tally.warnings(warn) {
            let _ = self.record(EventBody::Note(crate::log::event::Note {
                kind: warning.kind.to_string(),
                text: warning.message.clone(),
            }));
            self.ui_note(format!("» warning: {}", warning.message));
            if !self.out.json && !self.out.tui {
                eprintln!("kelir: warning: {}", warning.message);
            }
        }
    }

    pub(crate) fn status_answer(&self) -> Response {
        let retry = self.retry_wait.get().map(|(until, status, attempt, slot)| {
            crate::provider::usage::RetryNote {
                status,
                attempt,
                remaining_ms: until.saturating_duration_since(Instant::now()).as_millis() as u64,
                slot,
            }
        });
        let live = crate::provider::usage::Live {
            id: &self.log.id,
            provider: &self.provider.provider_name,
            model: &self.provider.model,
            turn: self.run.turn,
            turn_steps: self.run.turn_steps,
            max_steps: self.cfg.max_steps,
            session_steps: self.run.step,
            // Until the model list answers (or a window is configured) the
            // number is a guess, and a guess is `null`, not 128000.
            window: (self.features.models.window_source != "default")
                .then_some(self.features.models.window),
            window_source: self.features.models.window_source,
            running: &self.tools.live_calls,
            retry,
            warn: self.warn(),
        };
        let mut state = crate::provider::usage::status_json(&self.tally, &live);
        let mut feature_fields = serde_json::Map::new();
        self.features.status(&mut feature_fields);
        if let Some(object) = state.as_object_mut() {
            for (key, value) in feature_fields {
                object.insert(key, value);
            }
        }
        if let Some(object) = state.as_object_mut() {
            self.features.status_extended(self, object);
        }
        Response::ok("status", 0, "now", state)
    }

    pub(crate) fn sync(&mut self) {
        let _ = self.log.writer.sync();
    }

    /// Tell the UI what happened. A closed UI is not an error: the session is
    /// the source of truth and keeps working headless.
    pub(crate) fn emit(&self, event: crate::tui::View) {
        match &event {
            crate::tui::View::Live(crate::tui::Live::Retrying {
                until,
                status,
                attempt,
                ..
            }) => self.retry_wait.set(Some((
                *until,
                *status,
                *attempt,
                *attempt == 0 && status.is_none(),
            ))),
            crate::tui::View::Live(crate::tui::Live::RetryingDone) => self.retry_wait.set(None),
            _ => {}
        }
        if let Some(ui) = &self.out.ui {
            let _ = ui.send(event);
        }
    }

    /// Report Herdr status (no-op unless a writer was opened).
    pub(crate) fn report(&mut self, state: HerdrState) {
        let Some(herdr) = &self.out.herdr else {
            return;
        };
        let state = crate::features::herdr::state_for(state, herdr.telegram());
        if self.out.tui {
            self.emit(crate::tui::View::Live(crate::tui::Live::Herdr { state }));
        } else if let Some(herdr) = &mut self.out.herdr {
            herdr.report(state);
        }
    }

    /// Set the Herdr pane title from the session title. In the TUI the record travels with the
    /// other status records and is written between paints; non-TUI paths write it here.
    /// A human-only line: it goes to the view, never to the durable log. The
    /// log keeps facts, not every UI announcement (the existing log-count tests depend on it), so this is a synthetic event with no sequence number.
    pub(crate) fn ui_note(&mut self, text: impl Into<String>) {
        self.emit(crate::tui::View::Log(crate::tui::note_event(text)));
    }

    /// An error the UI must show, also without adding a log record.
    pub(crate) fn ui_error(&mut self, message: impl Into<String>) {
        self.emit(crate::tui::View::Log(crate::tui::error_event(message)));
    }

    pub(crate) fn report_title(&mut self, title: &str) {
        if self.out.herdr.is_none() {
            return;
        }
        let record = crate::features::herdr::title_record(title);
        if !self.out.tui
            && let Some(herdr) = &mut self.out.herdr
        {
            herdr.write_record(&record);
        }
    }

    pub(crate) fn notify(&mut self, kind: &str, detail: &str) {
        let dispatch = crate::features::notify::fire(
            &self.cfg.notify,
            &std::collections::BTreeMap::new(),
            &self.log.dir,
            &self.ctx.shell,
            kind,
            detail,
        );
        if let Some(dispatch) = dispatch {
            let detail = dispatch
                .error
                .unwrap_or_else(|| format!("{} hook(s) fired", dispatch.hooks));
            let _ = self.record(EventBody::Note(crate::log::event::Note {
                kind: "notify".into(),
                text: format!("{}: {detail}", dispatch.kind),
            }));
        }
    }

    /// One turn is over: the UI gets its outcome, Herdr gets its word.
    pub(crate) fn finish_turn(&mut self, outcome: &str) {
        let state = match outcome {
            "done" => HerdrState::Done,
            "error" => HerdrState::Error,
            // A paused turn waits for a human to say `continue`; that is the
            // one Herdr word a supervisor can see as waiting.
            "paused" => HerdrState::Blocked,
            _ => HerdrState::Idle,
        };
        self.report(state);
        // Notifications are last; clone the detail before the event write borrows `self`.
        let detail = self.run.final_text.clone();
        match outcome {
            "done" => self.notify("turn_done", &detail),
            "error" => self.notify("error", &detail),
            "paused" => self.notify("paused", &detail),
            _ => {}
        }
    }

    pub(crate) fn print_result(&self, exit_code: u8) {
        if !self.out.json {
            return;
        }
        let steps = self
            .log
            .events
            .iter()
            .filter(|event| matches!(event.body, EventBody::StepStarted { .. }))
            .count();
        // The last turn's outcome, so a stopped turn is reported as such.
        let outcome = self
            .log
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.body {
                EventBody::TurnEnded { outcome, .. } => Some(outcome.clone()),
                _ => None,
            })
            .unwrap_or_else(|| {
                if exit_code == 0 {
                    "done".to_string()
                } else {
                    "error".to_string()
                }
            });
        print_result_event(
            &outcome,
            exit_code,
            &self.run.final_text,
            steps,
            Some((&self.log.id, &self.log.dir)),
            None,
        );
    }

    pub(crate) fn state(&self) -> Value {
        json!({
            "turn": self.run.turn,
            "step": self.run.step,
            "turn_steps": self.run.turn_steps,
            "max_steps": self.cfg.max_steps,
            "running": self.tools.live_calls,
            "provider": self.provider.provider_name,
            "model": self.provider.model,
            "window": self.features.models.window,
            "window_source": self.features.models.window_source,
            // Why this start does not have the provider's window, when it does not: `http 401`,
            // `offline`, `timeout`, `malformed`, `fetching`. Absent when nothing needs saying.
            "window_note": self.features.models.window_note,
            // Whether the list is still being fetched beside the session.
            "models_fetching": self
                .features
                .models
                .fetch
                .as_ref()
                .is_some_and(Fetch::is_running),
            "disabled": self.tools.disabled.iter().cloned().collect::<Vec<_>>(),
        })
    }

    // -------------------------------------------------------------- verbs
}
