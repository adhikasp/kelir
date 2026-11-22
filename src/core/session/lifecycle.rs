//! Session construction and log reopen.
// These private loop modules expose crate-visible helpers to the feature modules.
#![allow(clippy::redundant_pub_crate)]

use super::*;

impl Session {
    pub(crate) fn open(
        prepared: Prepared,
        trust: std::sync::Arc<crate::trust::Trust>,
    ) -> Result<Self, String> {
        let mut prepared = prepared;
        let events_path = prepared.dir.join("events.jsonl");
        let writer = EventLog::open(&events_path)
            .map_err(|error| format!("cannot open {}: {error}", events_path.display()))?;
        let ctx = {
            let mut ctx = Ctx::new(
                prepared.dir.clone(),
                prepared.cwd.clone(),
                prepared.cfg.shell.clone(),
                prepared.cfg.spill_limit,
                prepared.sock.clone(),
            );
            ctx.session_id = prepared.id.clone();
            ctx.promote_after = prepared.cfg.promote_after;
            // Every tool child exports the session's token as `KELIR_TOKEN`
            // , exactly as it already gets `KELIR_SOCK`.
            ctx.token = trust.token().to_string();
            ctx
        };
        let reopened = prepared.resume.is_some();
        let mut session = Session {
            log: Log {
                writer,
                blobs: Blobs::new(&prepared.dir),
                events: prepared
                    .resume
                    .as_ref()
                    .map(|read| read.events.clone())
                    .unwrap_or_default(),
                id: prepared.id.clone(),
                dir: prepared.dir.clone(),
                config_dir: crate::paths::config_dir(),
                titled: prepared.resume.as_ref().is_some_and(|read| {
                    read.events
                        .iter()
                        .any(|event| matches!(event.body, EventBody::SessionTitle { .. }))
                }),
            },
            provider: Provider {
                client: prepared.client.clone(),
                active_provider: prepared.provider_cfg.clone(),
                provider_name: prepared.provider_name.clone(),
                model: prepared.model.clone(),
                refused_fields: BTreeSet::new(),
                governor: prepared.governor,
            },
            out: Out {
                json: prepared.json,
                echo_assistant: !prepared.ask && !prepared.tui,
                keep: prepared.keep,
                tui: prepared.tui,
                ui: None,
                herdr: prepared.herdr,
            },
            compact: CompactState {
                request: None,
                last_prompt_tokens: None,
                floor: None,
            },
            tools: ToolState {
                names: prepared
                    .resume
                    .as_ref()
                    .map(|read| read.tools.clone())
                    .unwrap_or_else(tools::names),
                disabled: BTreeSet::new(),
                live_calls: Vec::new(),
            },
            run: Run {
                step: prepared.resume.as_ref().map(|read| read.step).unwrap_or(0),
                turn: prepared.resume.as_ref().map(|read| read.turns).unwrap_or(0),
                turn_steps: 0,
                has_task: !prepared.task.trim().is_empty(),
                final_text: String::new(),
                run_sources: prepared.run_sources.clone(),
                task_from: prepared.task_from,
                step_cap: prepared.step_cap,
                resumed: reopened,
                in_flight: prepared
                    .resume
                    .as_ref()
                    .map(|read| read.in_flight)
                    .unwrap_or(false),
                retryable_failure: false,
                pending_task: prepared.task.clone(),
            },
            cfg: prepared.cfg.clone(),
            ctx,
            inbox: None,
            scheduler: Scheduler::default(),
            features: crate::features::Features {
                models: models::Models::new(
                    prepared.window,
                    prepared.window_source,
                    prepared.window_note.take(),
                    prepared.models.take(),
                    prepared.models_fetch.take(),
                    &prepared.model,
                ),
                mcp: Some(crate::features::mcp::Mcp::new(
                    crate::mcp::broker::Broker::new(
                        prepared.cwd.clone(),
                        prepared.dir.clone(),
                        prepared.id.clone(),
                    ),
                )),
                gates: Some(crate::features::gates::Gates::new()),
                sources: crate::features::sources::Sources::new(crate::sources::Watch::default()),
            },
            trust,
            tally: crate::usage::Tally::default(),
            kids: crate::features::children::Kids::new(
                &prepared.id,
                prepared.cfg.children,
                prepared.child.as_ref(),
            ),
            retry_wait: std::cell::Cell::new(None),
        };
        if session.kids.root.is_some() {
            crate::gates::limit_for_child();
        }
        session.gates_do(|gates, session| gates.build(session));
        crate::child::set_lineage(session.kids.lineage.clone());
        session.tally = crate::usage::Tally::from_events(&session.log.events, session.warn());
        // The watcher re-checks the sources the frozen system was rendered from; on a resume it is rebuilt
        // from the log's `sources[]` *and* the `source` events it already carries, so a source the verb added
        if let Some(read) = prepared.resume {
            session.features.sources = crate::features::sources::Sources::new(
                crate::sources::Watch::rebuild(&read.events, &session.log.dir),
            );
            // A turn that was in flight continues without a new task.
            session.run.has_task |= read.in_flight;
            session.reopen(read)?;
            session.settle_lost_children();
            return Ok(session);
        }
        session.features.sources = crate::features::sources::Sources::new(
            crate::sources::Watch::from_rendered(&prepared.system),
        );
        session.record(EventBody::SessionStarted {
            id: prepared.id.clone(),
            format: 1,
            parent: prepared.child.as_ref().map(|child| child.parent.clone()),
            depth: prepared.child.as_ref().map_or(0, |child| child.depth),
            cwd: crate::util::path_string(&prepared.cwd),
            os: std::env::consts::OS.to_string(),
            kelir_version: VERSION.to_string(),
            provider: prepared.provider_name.clone(),
            model: prepared.model.clone(),
            system: SystemInfo {
                text: prepared.system.text.clone().into(),
                hash: prepared.system.hash.clone(),
            },
            sources: prepared.system.sources.clone(),
            tools: session.tools.names.clone(),
            // What the loop will ask this model for: the configured effort, unless the provider's list says the
            // model does not take the parameter.
            reasoning_effort: session.first_effort(),
            // Where the key came from, never the key.
            key_origin: prepared.key_origin.clone(),
        })?;
        // What the render had to say about its sources (a non-UTF-8 file read lossily, a truncation, a
        // malformed skill skipped): recorded once, so a file never vanishes without a trace.
        for warning in &prepared.system.warnings {
            session.record(EventBody::Error {
                location: "source".into(),
                message: warning.clone(),
                retryable: false,
                status: None,
                attempt: None,
                retry_in_ms: None,
            })?;
        }
        if session.run.has_task {
            session.record_plan_sources(false)?;
            session.gates_do(|gates, session| {
                gates.submit_task(session, prepared.task.clone(), "now")
            })?;
        }
        session.sync();
        Ok(session)
    }

    /// Pick the log up where it stopped: report the damage, mark the resume, settle the calls nobody
    /// settled (as *unknown*, never by running them again), and add a new task if one was given.
    pub(crate) fn reopen(&mut self, read: Reopened) -> Result<(), String> {
        // Gaps in the log first: a torn last line and every line that never parsed are reported, never dropped
        // silently — but a gap that a previous resume already reported is not reported again.
        let reported: Vec<String> = self
            .log
            .events
            .iter()
            .filter_map(|event| match &event.body {
                EventBody::Error {
                    location, message, ..
                } if location == "log" => Some(message.clone()),
                _ => None,
            })
            .collect();
        for damage in &read.damage {
            if reported.contains(damage) {
                continue;
            }
            self.record(EventBody::Error {
                location: "log".into(),
                message: damage.clone(),
                retryable: false,
                status: None,
                attempt: None,
                retry_in_ms: None,
            })?;
        }
        // Calls the log shows as requested or started but never settled get exactly one
        // synthetic result. Re-running them is what a naive replay does; it is how `rm -rf`
        // runs twice.
        let mut unknown = 0usize;
        for call in &read.unsettled {
            let partial = self.interrupted_output(call);
            let result = resume::unknown_result(
                partial
                    .as_ref()
                    .map(|(path, bytes, lines)| (path.as_str(), *bytes, *lines)),
            );
            let (out_path, bytes, lines) = partial.unwrap_or_default();
            self.record(crate::event::call_settle(
                &call.call_id,
                "tool",
                "unknown",
                crate::event::CallSettle::default()
                    .bytes(bytes)
                    .lines(lines)
                    .out_path(out_path)
                    .result(Payload::text(result)),
            ))?;
            unknown += 1;
        }
        // The same for `kelir mcp` calls: an mcp call start with no settlement means the
        // session died while the server was running the tool.
        let settled: std::collections::HashSet<String> = self
            .log
            .events
            .iter()
            .filter_map(|event| match &event.body {
                EventBody::CallSettled(settle) if settle.kind == "mcp" => Some(settle.id.clone()),
                _ => None,
            })
            .collect();
        let orphans: Vec<String> = self
            .log
            .events
            .iter()
            .filter_map(|event| match &event.body {
                EventBody::CallStarted(start)
                    if start.kind == "mcp" && !settled.contains(&start.id) =>
                {
                    Some(start.id.clone())
                }
                _ => None,
            })
            .collect();
        for mcp_id in orphans {
            self.record(crate::event::call_settle(
                mcp_id,
                "mcp",
                "unknown",
                crate::event::CallSettle::default()
                    .exit_code(Some(crate::mcp::render::EXIT_TIMEOUT)),
            ))?;
            unknown += 1;
        }
        if unknown > 0 {
            self.emit(UiEvent::Info(format!(
                "kelir: resumed · {unknown} call(s) had an unknown outcome and were not re-run"
            )));
        }
        // An agent-called compaction that was settled but never applied — a kill between the tool's
        // `compaction queued` result and the boundary that summarises — is queued again here, so the model's
        if self.compact.request.is_none()
            && let Some(focus) = compact::queued_compaction(&self.log.events, &self.log.dir)
        {
            self.compact.request = Some(CompactRequest {
                by: "agent",
                focus,
                immediate: false,
            });
            self.emit(UiEvent::Info(
                "kelir: resumed · a queued compaction is applied at the next step".to_string(),
            ));
        }
        // Background jobs that started or settled while no session was running: the shim's status file is the
        for event in &self.log.events {
            match &event.body {
                EventBody::CallStarted(start) if start.kind == "bg" => self.trust.adopt(&start.id),
                EventBody::CallStarted(start) if start.kind == "tool" => {
                    if let Some(dir) = &start.info.dir {
                        self.trust.adopt(dir);
                    }
                }
                _ => {}
            }
        }
        let mut woke = false;
        for status in crate::bg::list(&crate::paths::calls_dir(&self.log.dir)) {
            if !status.is_job() {
                continue;
            }
            woke |= self.note_bg(&status)?;
        }
        if woke {
            // A job settling while the session was down *is* something to do:
            // the resumed turn reads the message like any other.
            self.run.has_task = true;
            self.emit(UiEvent::Info(
                "kelir: resumed · a background job settled while this session was not running"
                    .to_string(),
            ));
        }
        if !self.run.pending_task.trim().is_empty() || read.in_flight {
            self.record_plan_sources(true)?;
        }
        if !self.run.pending_task.trim().is_empty() {
            // `kelir run --resume <id> <task>`: a new message on the existing log.
            let delivery = if read.in_flight { "next" } else { "now" };
            let task = self.run.pending_task.clone();
            self.gates_do(|gates, session| gates.submit_task(session, task, delivery))?;
        } else {
            self.continue_a_turn_that_ends_on_an_answer()?;
        }
        self.sync();
        Ok(())
    }
}
