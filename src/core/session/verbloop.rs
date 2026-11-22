//! The verb half of the session loop: accept, parse, log and apply verbs.

use super::types::*;
use super::*;

impl Session {
    pub(super) fn accept(&mut self, incoming: Incoming) -> Option<Pending> {
        self.accept_as(incoming, None)
    }

    /// `accept`, with the answer's `applies` word overridden. The idle loop uses it for a `stop`
    /// that has nothing to stop: the verb is dropped, so the answer says `dropped`, not `now`.
    pub(super) fn accept_as(
        &mut self,
        incoming: Incoming,
        applies: Option<&'static str>,
    ) -> Option<Pending> {
        let Incoming {
            request,
            reply,
            action,
        } = incoming;
        if let Some(pending) = self.note_own(action) {
            // The session's own watcher, not a client: nothing to log and nobody to answer, but the note still
            // waits for a boundary like the `bg` report it stands in for, so `bg_started`/`bg_settled` land in the
            self.scheduler.push(pending);
            return None;
        }
        let from = request.from.clone();
        let name = request.verb.clone();
        if name == "status" {
            let _ = reply.send(self.status_answer());
            return None;
        }
        if crate::mcp::broker::is_mcp_verb(&name) {
            self.mcp_do(|mcp, session| mcp.verb(session, request, reply));
            return None;
        }
        if name == crate::child::VERB || name == "brief" {
            self.child_or_brief(request, reply);
            return None;
        }
        // `bg-spawn` is the one thing a job cannot do itself: on Windows a `kelir bg run` inside a call sits
        // in the call's Job Object, which forbids breakaway (it has to — an escapable job cannot be
        if name == "bg-spawn" {
            let answer = self.bg_spawn_answer(&from, &request);
            let _ = reply.send(answer);
            return None;
        }
        if name == "bg-claim" {
            let answer = self.bg_spawn_answer(&from, &request);
            let _ = reply.send(answer);
            return None;
        }
        let parsed = self.parse_verb(&request, &from);
        let scope = match &parsed {
            Ok(verb) => applies.unwrap_or_else(|| verb.scope()),
            Err(_) => "rejected",
        };
        let error = parsed.as_ref().err().cloned();
        // A refusal is recorded only while the refusal budget lasts: a token holder that names
        // foreign jobs one by one cannot grow the log without bound. The answer still goes out.
        let recorded = if error.is_some() && !self.trust.may_record_refusal() {
            Err("the refusal budget is spent".to_string())
        } else {
            self.log_verb(&name, &request.args, &from, scope, error)
        };
        // The ack must not outrun the log: a crash right after `{"ok": true}`
        // may not lose a verb the client believes was accepted.
        self.sync();
        match (parsed, recorded) {
            (Ok(verb), Ok(event)) => {
                let pending = Pending {
                    seq: event.seq,
                    verb: verb.clone(),
                    from,
                };
                let _ = reply.send(Response::ok(&name, event.seq, scope, self.state()));
                Some(pending)
            }
            (Ok(_), Err(message)) => {
                let _ = reply.send(Response::error(message));
                None
            }
            (Err(message), _) => {
                let _ = reply.send(Response::error(message));
                None
            }
        }
    }

    /// `accept` for a verb a wait is about to apply at once (`say --now`, `model`): the answer goes out
    /// *after* the verb landed, so it says `applies: "now"` and `state.model` (or the queued text) is what
    pub(super) async fn accept_at_once(&mut self, incoming: Incoming) -> AtOnce {
        let Incoming {
            request,
            reply,
            action,
        } = incoming;
        if let Some(pending) = self.note_own(action) {
            self.scheduler.push(pending);
            return AtOnce::Rejected;
        }
        let from = request.from.clone();
        let name = request.verb.clone();
        if name == "status" {
            let _ = reply.send(self.status_answer());
            return AtOnce::Rejected;
        }
        if crate::mcp::broker::is_mcp_verb(&name) {
            self.mcp_do(|mcp, session| mcp.verb(session, request, reply));
            return AtOnce::Rejected;
        }
        if name == crate::child::VERB || name == "brief" {
            self.child_or_brief(request, reply);
            return AtOnce::Rejected;
        }
        if name == "bg-spawn" {
            let answer = self.bg_spawn_answer(&from, &request);
            let _ = reply.send(answer);
            return AtOnce::Rejected;
        }
        if name == "bg-claim" {
            let answer = self.bg_spawn_answer(&from, &request);
            let _ = reply.send(answer);
            return AtOnce::Rejected;
        }
        let parsed = self.parse_verb(&request, &from);
        let boundary = if parsed.is_ok() { "now" } else { "rejected" };
        let error = parsed.as_ref().err().cloned();
        // The refusal budget applies here too: a token holder naming foreign jobs one by one gets
        // its answer either way, and the log grows by no more than the budget.
        let recorded = if error.is_some() && !self.trust.may_record_refusal() {
            Err("the refusal budget is spent".to_string())
        } else {
            self.log_verb(&name, &request.args, &from, boundary, error)
        };
        self.sync();
        let (verb, event) = match (parsed, recorded) {
            (Ok(verb), Ok(event)) => (verb, event),
            (Ok(_), Err(message)) | (Err(message), _) => {
                let _ = reply.send(Response::error(message));
                // A refusal changes nothing, so the wait it arrived in goes on.
                return AtOnce::Rejected;
            }
        };
        let pending = Pending {
            seq: event.seq,
            verb,
            from,
        };
        let mut end: Option<StreamEnd> = None;
        let applied = self.apply_one(pending, &mut end).await;
        let answer = match applied {
            Ok(()) => Response::ok(&name, event.seq, "now", self.state()),
            Err(message) => Response::error(message),
        };
        // The answer goes out either way: the verb has landed (or the log said
        // why it could not), and a client must not wait on a reply nobody kept.
        let _ = reply.send(answer);
        AtOnce::Applied(end.map(Box::new))
    }

    /// The `bg-spawn` answer: the shim only ever answers a job, and a job is the only sender allowed to
    /// ask.
    pub(super) fn bg_spawn_answer(&mut self, from: &str, request: &Request) -> Response {
        if from != "job" {
            return Response::error("bg-spawn: an internal verb; jobs report themselves");
        }
        if request.verb == "bg-claim" {
            return self.bg_claim_answer();
        }
        if let Err(error) = self.trust.check_spawn(request) {
            return match self.refuse_verb("bg-spawn", &request.args, from, &error) {
                Ok(()) => Response::error(error),
                Err(message) => Response::error(message),
            };
        }
        // Only `cmd`, `shell`, `cwd` and a filtered `env` are the caller's; the kind, the socket,
        // the token, the job name and the call are the session's own.
        let call_is_live = |call: &str| self.tools.live_calls.iter().any(|live| live == call);
        match crate::trust::spawn_request(request).and_then(|spawn| {
            let spawn = spawn.pinned(&self.ctx.sock, &self.ctx.token, call_is_live);
            crate::bg::spawn_shim_for_client(&self.trust, &spawn)
        }) {
            Ok(spawned) => Response::ok("bg-spawn", 0, "now", json!({"id": spawned.id})),
            Err(error) => Response::error(error),
        }
    }

    /// `bg-claim`, retired: nothing sends it, so it answers an id and nothing else, from a small
    /// budget of its own (it must not use the one `bg-spawn` mints from).
    pub(super) fn bg_claim_answer(&mut self) -> Response {
        match self.trust.claim_ack() {
            Ok(id) => Response::ok("bg-claim", 0, "now", json!({"id": id})),
            Err(error) => Response::error(error),
        }
    }

    /// Record a refused verb, unless the refusal budget is spent: a token holder can name foreign
    /// jobs one by one and the log must not grow by one event per try.
    pub(super) fn refuse_verb(
        &mut self,
        name: &str,
        args: &Value,
        from: &str,
        error: &str,
    ) -> Result<(), String> {
        if !self.trust.may_record_refusal() {
            return Ok(());
        }
        let event = self.log_verb(name, args, from, "rejected", Some(error.to_string()))?;
        self.sync();
        let _ = event;
        Ok(())
    }

    pub(super) fn parse_verb(&self, request: &Request, from: &str) -> Result<Verb, String> {
        let name = request.verb.as_str();
        if name == "bg" && from != "job" {
            return Err("bg: an internal verb; jobs report themselves".to_string());
        }
        // Only for the session's own jobs: a foreign id is refused here, where
        // it is recorded as a `verb` event marked refused.
        self.trust.check_job(request)?;
        let parsed = verbs::parse(name, &request.args)?;
        // `view` is validated when it arrives, not when it applies: the CLI prints "image queued" from the
        // ack, so a bad path must be refused in the ack and not a boundary later (where nobody is listening).
        if let Verb::View { path } = &parsed
            && let Err(message) = self.check_view(path)
        {
            return Err(message);
        }
        if let Verb::Stop {
            call: Some(call), ..
        } = &parsed
            && !self.tools.live_calls.iter().any(|id| id == call)
        {
            return Err(format!("no running call with id `{call}`"));
        }
        Ok(parsed)
    }

    /// Log the arrival, before it is answered or applied.
    pub(super) fn log_verb(
        &mut self,
        name: &str,
        args: &Value,
        from: &str,
        boundary: &str,
        error: Option<String>,
    ) -> Result<crate::event::LogEvent, String> {
        self.record(EventBody::Verb {
            name: name.to_string(),
            // Never the request's args wholesale: a whitelist per verb
            args: Payload::value(verbs::loggable_args(name, args)),
            from: from.to_string(),
            boundary: boundary.to_string(),
            error,
        })
    }

    /// Apply the verbs due at `boundary`.
    pub(super) async fn apply(&mut self, boundary: Boundary) -> Result<Applied, String> {
        self.mcp_do(|mcp, session| mcp.flush(session));
        let mut applied = Applied::default();
        for pending in self.scheduler.due(boundary) {
            self.apply_pending(pending, &mut applied).await?;
        }
        self.sync();
        Ok(applied)
    }

    /// Apply the pending verbs that are *actions* rather than messages.
    pub(super) async fn apply_actions(&mut self) -> Result<(), String> {
        let actions = self.scheduler.take_where(Verb::is_action);
        if actions.is_empty() {
            return Ok(());
        }
        let mut applied = Applied::default();
        for pending in actions {
            self.apply_pending(pending, &mut applied).await?;
        }
        self.sync();
        Ok(())
    }

    /// Apply one verb. The caller decides when: a boundary, or a wait the verb
    /// is allowed to cut short.
    pub(super) async fn apply_pending(
        &mut self,
        pending: Pending,
        applied: &mut Applied,
    ) -> Result<(), String> {
        {
            match pending.verb {
                Verb::Say { text, now } => {
                    let from = pending.from.to_string();
                    let Some(text) = self.gates_prompt(text, &from).await? else {
                        return Ok(());
                    };
                    // A message from anywhere but this UI gets echoed, so a
                    // steering `ctl say` is visible in the pane it steers.
                    if pending.from != "tui" {
                        self.emit(UiEvent::Info(format!(
                            "› {}",
                            crate::util::one_line_visible(&text, 200)
                        )));
                    }
                    // The text carries the pointers; the list carries the paths they name, so a reader (and a
                    // later tool) can see which images a message was about without parsing prose .
                    let attachments = crate::attachments::paths_in(&text)
                        .into_iter()
                        .map(serde_json::Value::String)
                        .collect();
                    self.record(EventBody::User {
                        text: Payload::text(text),
                        from: pending.from,
                        delivery: if now { "now" } else { "next" }.to_string(),
                        attachments,
                    })?;
                    self.gates_do(|gates, session| gates.flush_context(session));
                    applied.messages += 1;
                }
                // Due only at a turn end, where `take_turn_messages` gets it.
                Verb::Later { .. } => {}
                // The call already settled; there is nothing left to kill.
                Verb::Stop { call: Some(_), .. } => {}
                Verb::Stop { call: None, reason } => {
                    applied.stopped = Some(reason.unwrap_or_else(|| "stopped by user".into()));
                }
                Verb::Model { target } => {
                    // A bad model must not kill the session: it is recorded and
                    // the old one keeps working.
                    if let Err(message) = self.swap_model(&target) {
                        self.record(EventBody::Error {
                            location: "verb".into(),
                            message,
                            retryable: false,
                            status: None,
                            attempt: None,
                            retry_in_ms: None,
                        })?;
                    }
                }
                Verb::Tools { disable, enable } => {
                    for name in disable {
                        self.tools.disabled.insert(name);
                    }
                    for name in enable {
                        self.tools.disabled.remove(&name);
                    }
                }
                Verb::View { path } => {
                    // An image queued by `kelir view` from inside a call: it becomes a `source` event at the tail, so the
                    // next request carries an image part and every earlier message (and the cached prefix) stays byte-
                    match self.apply_view(&path) {
                        Ok((rel, bytes)) => {
                            applied.messages += 1;
                            self.emit(UiEvent::Attachment { rel, bytes });
                            self.sync();
                        }
                        Err(message) => {
                            self.emit(UiEvent::Error(message.clone()));
                            self.record(EventBody::Error {
                                location: "view".into(),
                                message,
                                retryable: false,
                                status: None,
                                attempt: None,
                                retry_in_ms: None,
                            })?;
                            self.sync();
                        }
                    }
                }
                Verb::End => applied.end = true,
                Verb::Compact { focus } => {
                    // Applies at `before_step`: the compaction needs a provider call and must never land between
                    // a tool call and its result.
                    self.emit(UiEvent::Info(match &focus {
                        Some(focus) => format!("» compact: {focus}"),
                        None => "» compact".to_string(),
                    }));
                    if self.compact.request.is_none() {
                        self.compact.request = Some(CompactRequest {
                            by: "user",
                            focus: focus.clone(),
                            immediate: true,
                        });
                    }
                }
                Verb::Bg(status) => {
                    if self.note_bg(&status)? {
                        applied.messages += 1;
                    }
                }
                Verb::Child(note) => {
                    if self.note_child(&note)? {
                        applied.messages += 1;
                    }
                }
                // Answered where it arrives (`child_or_brief`).
                Verb::Brief { .. } => {}
                // Answered where it arrives; it never gets this far.
                Verb::Status => {}
                // A job of this session's own, killed on request.
                Verb::Cancel { id } => {
                    match crate::bg::cancel_in(&self.trust.bg_dir(), &id).await {
                        Ok(text) => self.emit(UiEvent::Info(format!("· {text}"))),
                        Err(text) => {
                            self.record(EventBody::Error {
                                location: "verb".into(),
                                message: format!("cancel {id}: {text}"),
                                retryable: false,
                                status: None,
                                attempt: None,
                                retry_in_ms: None,
                            })?;
                        }
                    }
                }
                // Adds or replaces one source, as a message at the tail: the
                // system prompt (and the provider's cache) is untouched.
                Verb::Skills {
                    kind,
                    name,
                    path,
                    text,
                } => {
                    let Self {
                        log:
                            Log {
                                writer,
                                blobs,
                                events,
                                dir,
                                id,
                                ..
                            },
                        provider:
                            Provider {
                                model,
                                provider_name,
                                client,
                                active_provider,
                                governor,
                                ..
                            },
                        out: Out { ui, json, tui, .. },
                        cfg,
                        ctx,
                        tally,
                        features,
                        ..
                    } = self;
                    let mut cx = Cx {
                        writer,
                        blobs,
                        events,
                        ui,
                        cfg,
                        dir,
                        id,
                        model,
                        provider_name,
                        client,
                        active_provider,
                        governor,
                        ctx,
                        tally,
                        json: *json,
                        tui: *tui,
                    };
                    match features.sources.apply_skills(
                        kind,
                        &name,
                        path.as_deref(),
                        text.as_deref(),
                        &mut cx,
                    ) {
                        Ok(()) => applied.messages += 1,
                        // A bad source must not kill the session: it is
                        // recorded and the turn continues.
                        Err(message) => {
                            self.record(EventBody::Error {
                                location: "verb".into(),
                                message,
                                retryable: false,
                                status: None,
                                attempt: None,
                                retry_in_ms: None,
                            })?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
