//! Turn reporting and final state answers.

use super::*;

impl Session {
    /// a kill can leave a log whose last message is the assistant's final answer, and a request may not
    /// end on one — providers read a trailing assistant message as prefill or refuse it.
    pub(super) fn continue_a_turn_that_ends_on_an_answer(&mut self) -> Result<(), String> {
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
        let folded = crate::fold::fold(&self.log.events, &self.log.dir)?;
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

    /// The partial output of a call the previous process started, when its `commands/<seq>-*` directory
    /// still has one.
    pub(super) fn interrupted_output(&self, call: &UnsettledCall) -> Option<(String, u64, u64)> {
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
                EventBody::ToolStarted { call_id, .. } if *call_id == call.call_id => {
                    Some(event.seq)
                }
                _ => None,
            })?;
        let prefix = format!("{seq}-");
        let entry = std::fs::read_dir(self.log.dir.join("commands"))
            .ok()?
            .flatten()
            .find(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))?;
        let out = entry.path().join("out");
        let partial = crate::spill::partial(&out).ok()?;
        if partial.bytes == 0 {
            return None;
        }
        Some((crate::util::path_string(&out), partial.bytes, partial.lines))
    }

    /// The read-only view a built-in gate gets of this session. Kept in the core
    /// because it borrows the provider client, config and fold.
    pub(super) fn gate_ctx(&self) -> crate::gates::GateCtx<'_> {
        // The built-in hint must see machine-level skills, just as the old
        // core hint did (`places(&ctx.cwd, &cfg.skills_dirs, Some(config_dir()))`).
        crate::gates::GateCtx {
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
    pub(super) fn record(&mut self, body: EventBody) -> Result<LogEvent, String> {
        let seq = self.log.writer.next_seq();
        let title = if !self.log.titled
            && let EventBody::User { text, from, .. } = &body
            && from != "cli"
        {
            Some(crate::event::title_from_prompt(
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
        // Name the session from its first human prompt. Deterministic, no model call (design note
        // §9).
        if let Some(title) = title {
            self.log.titled = true;
            self.record(EventBody::SessionTitle {
                title: title.clone(),
            })?;
            self.emit(UiEvent::Title(title.clone()));
            self.report_title(&title);
        }
        Ok(event)
    }

    pub(super) fn warn(&self) -> crate::usage::Warn {
        crate::usage::Warn {
            prompt_tokens: self.cfg.warn_prompt_tokens,
        }
    }

    /// After a `usage` event: say so, once, when a threshold was crossed.
    pub(super) fn note_warnings(&mut self) {
        let warn = self.warn();
        for warning in self.tally.warnings(warn) {
            let _ = self.record(EventBody::Warning {
                kind: warning.kind.to_string(),
                message: warning.message.clone(),
                value: warning.value,
                threshold: warning.threshold,
            });
            self.emit(UiEvent::Info(format!("» warning: {}", warning.message)));
            if !self.out.json && !self.out.tui {
                eprintln!("kelir: warning: {}", warning.message);
            }
        }
    }

    pub(super) fn status_answer(&self) -> Response {
        let retry =
            self.retry_wait
                .get()
                .map(|(until, status, attempt, slot)| crate::usage::RetryNote {
                    status,
                    attempt,
                    remaining_ms: until.saturating_duration_since(Instant::now()).as_millis()
                        as u64,
                    slot,
                });
        let live = crate::usage::Live {
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
        let mut state = crate::usage::status_json(&self.tally, &live);
        let mut feature_fields = serde_json::Map::new();
        self.features.status(&mut feature_fields);
        if let Some(object) = state.as_object_mut() {
            for (key, value) in feature_fields {
                object.insert(key, value);
            }
        }
        // MCP servers: name, state, tool count and channel flag only. Never
        // config, env, descriptions or server stderr (those can hold secrets).
        let servers = self.features.mcp.as_ref().expect("mcp feature").servers();
        let mcp: Vec<Value> = servers["servers"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                let state = s["state"].as_str().unwrap_or("");
                // A `failed: ...` state carries server text; keep the word only.
                let state = if state.starts_with("failed") { "failed" } else { state };
                json!({"name": s["name"], "state": state, "tools": s["tools"], "channel": s["channel"]})
            })
            .collect();
        state["mcp"] = Value::Array(mcp);
        state["gates"] = self
            .features
            .gates
            .as_ref()
            .expect("gates feature")
            .status(self);
        state["children"] = json!(self.live_children());
        state["child_list"] = self.child_list();
        Response::ok("status", 0, "now", state)
    }

    pub(super) fn sync(&mut self) {
        let _ = self.log.writer.sync();
    }

    /// Tell the UI what happened. A closed UI is not an error: the session is
    /// the source of truth and keeps working headless.
    pub(super) fn emit(&self, event: UiEvent) {
        match &event {
            UiEvent::Retrying {
                until,
                status,
                attempt,
                ..
            } => self.retry_wait.set(Some((
                *until,
                *status,
                *attempt,
                *attempt == 0 && status.is_none(),
            ))),
            UiEvent::RetryingDone => self.retry_wait.set(None),
            _ => {}
        }
        if let Some(ui) = &self.out.ui {
            let _ = ui.send(event);
        }
    }

    /// Report Herdr status (no-op unless a writer was opened).
    pub(super) fn report(&mut self, state: HerdrState) {
        let Some(herdr) = &self.out.herdr else {
            return;
        };
        let state = crate::herdr::state_for(state, herdr.telegram());
        if self.out.tui {
            self.emit(UiEvent::Herdr { state });
        } else if let Some(herdr) = &mut self.out.herdr {
            herdr.report(state);
        }
    }

    /// Set the Herdr pane title from the session title. In the TUI the record travels with the
    /// other status records and is written between paints; non-TUI paths write it here.
    pub(super) fn report_title(&mut self, title: &str) {
        if self.out.herdr.is_none() {
            return;
        }
        let record = crate::herdr::title_record(title);
        if self.out.tui {
            self.emit(UiEvent::TitleRecord(record));
        } else if let Some(herdr) = &mut self.out.herdr {
            herdr.write_record(&record);
        }
    }

    pub(super) fn notify(&mut self, kind: &str, detail: &str) {
        let dispatch = crate::notify::fire(
            &self.cfg.notify,
            &std::collections::BTreeMap::new(),
            &self.log.dir,
            &self.ctx.shell,
            kind,
            detail,
        );
        if let Some(dispatch) = dispatch {
            let outcome = dispatch.outcome().to_string();
            let _ = self.record(EventBody::Notify {
                event: dispatch.kind,
                outcome,
                error: dispatch.error,
                hooks: dispatch.hooks as u32,
            });
        }
    }

    /// One turn is over: the UI gets its outcome, Herdr gets its word.
    pub(super) fn finish_turn(&mut self, outcome: &str) {
        self.emit(UiEvent::TurnEnded {
            outcome: outcome.to_string(),
            final_text: self.run.final_text.clone(),
        });
        let state = match outcome {
            "done" => HerdrState::Done,
            "error" => HerdrState::Error,
            // A paused turn waits for a human to say `continue`; that is the
            // one Herdr word a supervisor can see as waiting.
            "paused" => HerdrState::Blocked,
            _ => HerdrState::Idle,
        };
        self.report(state);
        // Notifications are last: the log already has the outcome, so a hook that reads it sees a
        // complete turn. Clone the detail first: `notify` records an event, which needs `&mut self`.
        let detail = self.run.final_text.clone();
        match outcome {
            "done" => self.notify("turn_done", &detail),
            "error" => self.notify("error", &detail),
            "paused" => self.notify("paused", &detail),
            _ => {}
        }
    }

    pub(super) fn print_result(&self, exit_code: u8) {
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

    pub(super) fn state(&self) -> Value {
        json!({
            "turn": self.run.turn,
            "step": self.run.step,
            // The turn's own count and its cap, so a steering human sees a pause coming (FINDINGS: "a status line
            // with steps used/limit would have shown it coming").
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
