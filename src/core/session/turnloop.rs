//! The turn half of the session loop.
// These private loop modules expose crate-visible helpers to the feature modules.
#![allow(clippy::redundant_pub_crate)]

use super::types::*;
use super::*;

impl Session {
    pub(crate) async fn drive_turn(&mut self, rx: &mut Inbox) -> Result<TurnEnd, String> {
        self.run.final_text.clear();
        self.run.retryable_failure = false;
        self.report(HerdrState::Working);
        self.run.turn_steps = 0;
        self.gates_do(|gates, _| gates.reset_turn());
        loop {
            // An explicit cap counts the steps of *this* turn: reaching it pauses the turn (the work so far is
            // kept, and the next message opens a new turn with a fresh count).
            if let Some(cap) = self.cfg.max_steps
                && self.run.turn_steps >= cap
            {
                return Ok(TurnEnd::Paused(PauseReport::steps(cap)));
            }
            self.run.turn_steps += 1;

            // ---------------------------------------------------- before_step A metadata fetch that ran beside
            // the session is applied here: this is the first boundary that reads the window, so the list has had
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
            features.before_step(&mut cx).await;
            let applied = self.apply(Boundary::BeforeStep).await?;
            if applied.end {
                return Ok(TurnEnd::End);
            }
            if let Some(reason) = applied.stopped {
                return Ok(TurnEnd::Stopped(reason));
            }

            // Compaction applies here and nowhere else: after the previous batch settled, before the next request,
            // never between a tool call and its result.
            if let Some(end) = self.maybe_compact(rx).await? {
                return Ok(end);
            }
            // Sources edited on disk while the turn ran: one `source` message
            // each, at the tail. Never a system rewrite.
            {
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
                features.check_sources(&mut cx)?;
            }
            // A key that is within two minutes of its expiry is re-read before the request: opencode refreshes its
            // token whenever it runs, so a copied-out one goes stale before its own `expires` does
            self.refresh_expiring_key();

            let folded = crate::fold::fold(&self.log.events, &self.log.dir)?;
            // Ask for several calls in one message when the provider says the model takes them, or (cold or slow
            // list) when the model's family is known to; the loop already runs a batch concurrently and appends
            let request = self.step_request(&folded.request_messages());
            let request_text = serde_json::to_string(&request.body).unwrap_or_default();
            self.run.step += 1;
            let step = self.run.step;
            self.record(EventBody::StepStarted {
                step,
                provider: self.provider.provider_name.clone(),
                model: self.provider.model.clone(),
                request_hash: crate::util::fnv1a64_hex(request_text.as_bytes()),
            })?;

            let assembler = match self.stream(rx, RequestFor::Step, false).await? {
                StreamEnd::Assembled(assembler) => assembler,
                StreamEnd::Stopped(reason) => return Ok(TurnEnd::Stopped(reason)),
                StreamEnd::End => return Ok(TurnEnd::End),
                StreamEnd::Paused(report) => return Ok(TurnEnd::Paused(report)),
            };
            let calls = assembler.tool_calls();
            let text = assembler.text.clone();

            self.record(EventBody::Assistant {
                step,
                text: text.clone().into(),
                reasoning: if assembler.reasoning.is_empty() {
                    None
                } else {
                    Some(assembler.reasoning.clone().into())
                },
                tool_calls: calls
                    .iter()
                    .map(|call| LoggedToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        args: Payload::value(call.args.clone()),
                    })
                    .collect(),
                finish_reason: assembler.finish_reason.clone(),
                usage: assembler.usage.as_ref().map(|usage| crate::event::Usage {
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                    cached_tokens: usage.cached_tokens,
                    cache_write_tokens: usage.cache_write_tokens,
                    allowance: usage.allowance.clone(),
                    kind: None,
                }),
            })?;
            if let Some(usage) = &assembler.usage {
                // The size of the context the provider just saw: what the auto
                // trigger compares to the window.
                self.compact.last_prompt_tokens = Some(usage.prompt_tokens);
                self.note_warnings();
            }
            // Even a turn that ends up stopped keeps the last thing the model
            // said in `turn_ended.final_text`.
            self.run.final_text = text.clone();
            self.sync();
            // Documented test hook: die where a crash would leave the log
            // between an assistant message and its tool calls.
            crash_at("usage");

            if !self.out.json && self.out.echo_assistant && !text.is_empty() {
                println!("{text}");
                let _ = std::io::stdout().flush();
            }

            if calls.is_empty() {
                self.run.final_text = text;
                // ------------------------------------------------------ turn_end A gate can send the model back once
                // per turn with a reason, or replace the final text.
                if self
                    .features
                    .gates
                    .as_ref()
                    .expect("gates feature")
                    .gated(crate::gates::Event::TurnEnd, None)
                {
                    let (tool_calls, blocked) = {
                        let gates = self.features.gates.as_ref().expect("gates feature");
                        (gates.turn_tool_calls(), gates.turn_blocked())
                    };
                    let payload = json!({
                        "final_text": self.run.final_text,
                        "tool_calls": tool_calls,
                        "steps": self.run.turn_steps,
                        "blocked": blocked,
                    });
                    let verdict = self
                        .gates_call(crate::gates::Event::TurnEnd, None, payload)
                        .await?;
                    if crate::features::gates::apply_turn_verdict(self, verdict, blocked)? {
                        continue;
                    }
                }
                // ---------------------------------------------------- after_step
                let applied = self.apply(Boundary::AfterStep).await?;
                if applied.end {
                    return Ok(TurnEnd::End);
                }
                if let Some(reason) = applied.stopped {
                    return Ok(TurnEnd::Stopped(reason));
                }
                // A `say` that arrived while the model was answering gets
                // another step in the same turn.
                if applied.messages > 0 {
                    continue;
                }
                // A job's declared `max_steps` caps the run itself: if this was the last allowed step, the job is
                // `paused` even when the model answered on it, so the scheduler's next invocation resumes it.
                if let Some(cap) = self.run.step_cap
                    && self.run.turn_steps >= cap
                {
                    return Ok(TurnEnd::Paused(PauseReport::steps(cap)));
                }
                return Ok(TurnEnd::Done);
            }

            // ------------------------------------------------------ tool batch
            self.tools.live_calls = calls.iter().map(|call| call.id.clone()).collect();
            let batch = self.run_tools(rx, step, &calls).await?;
            self.tools.live_calls.clear();
            let applied = self.apply(Boundary::AfterTools).await?;
            if batch.end || applied.end {
                return Ok(TurnEnd::End);
            }
            if let Some(reason) = batch.stopped {
                return Ok(TurnEnd::Stopped(reason));
            }
            if let Some(reason) = applied.stopped {
                return Ok(TurnEnd::Stopped(reason));
            }
        }
    }

    /// A 400 that names one of the capability fields kelir added: remove it from the request being retried
    /// and remember it, so no later request in this session pays the same 400.
    pub(crate) fn drop_refused_field(
        &mut self,
        request: &mut provider::BuiltRequest,
        error: &provider::ProviderError,
        dropped: &mut Option<String>,
    ) -> bool {
        // One drop per request: a second 400 means the field was not the
        // (only) reason, and looping through the body would hide it.
        if dropped.is_some() || !error.message.starts_with("http 400") {
            return false;
        }
        let field = ["reasoning_effort", "parallel_tool_calls"]
            .into_iter()
            .find(|field| {
                request.has_field(field)
                    && !self.provider.refused_fields.contains(*field)
                    && provider::names_field(&error.message, field)
            });
        let Some(field) = field else {
            return false;
        };
        *request = request.without_field(field);
        self.provider.refused_fields.insert(field.to_string());
        *dropped = Some(field.to_string());
        true
    }

    /// Re-read a key that is about to expire, before the next request uses it.
    pub(crate) fn refresh_expiring_key(&mut self) {
        let Some(expires) = self.provider.active_provider.key_expires_at_ms else {
            return;
        };
        let now = crate::models::now_ms();
        if expires.saturating_sub(now) >= credentials::EXPIRY_MARGIN.as_millis() as u64 {
            return;
        }
        let before = self.provider.active_provider.key.clone();
        match credentials::resolve(&mut self.provider.active_provider) {
            Ok(fresh) => {
                if self.provider.active_provider.key == before {
                    // Same value, so the same expiry: the provider will say so.
                    return;
                }
                if let Ok(client) = Client::new(
                    self.provider.active_provider.clone(),
                    &self.log.id,
                    self.cfg.cache_ttl.clone(),
                ) {
                    self.provider.client = client;
                }
                let _ = self.record(EventBody::Error {
                    location: "provider".into(),
                    message: format!(
                        "the key from {} was about to expire ({}); re-read it, new expiry {}",
                        fresh.describe(),
                        credentials::format_ms(expires),
                        fresh.expiry_text()
                    ),
                    retryable: false,
                    status: None,
                    attempt: None,
                    retry_in_ms: None,
                });
            }
            Err(message) => {
                // Not fatal: the request goes out with the key we have.
                if !self.out.json && !self.out.tui {
                    eprintln!("kelir: {message}");
                }
            }
        }
    }

    /// Re-read the key after a refusal that means "this credential is stale" (HTTP 401/403) and
    /// rebuild the request with it, once per request.
    pub(crate) fn reread_credential(
        &mut self,
        error: &provider::ProviderError,
        request: &mut provider::BuiltRequest,
        attempt: u32,
        reread: &mut bool,
    ) -> bool {
        if *reread || !matches!(error.status, Some(401 | 403)) {
            return false;
        }
        *reread = true;
        let before = self.provider.active_provider.key.clone();
        let fresh = match credentials::resolve(&mut self.provider.active_provider) {
            Ok(fresh) => fresh,
            Err(message) => {
                self.record_error(message, error.status, attempt);
                return false;
            }
        };
        if self.provider.active_provider.key == before {
            // The source had nothing newer: retrying the same key buys nothing, and the provider's own
            // message (a refusal, not a wait) is the answer.
            return false;
        }
        if let Ok(client) = Client::new(
            self.provider.active_provider.clone(),
            &self.log.id,
            self.cfg.cache_ttl.clone(),
        ) {
            self.provider.client = client;
        }
        let messages = crate::fold::fold(&self.log.events, &self.log.dir)
            .map(|folded| folded.request_messages())
            .unwrap_or_default();
        *request = self.step_request(&messages);
        self.record_error(
            format!(
                "{}: re-read the key from {} and retrying once",
                error.message,
                fresh.describe()
            ),
            error.status,
            attempt,
        );
        true
    }

    /// An `error` event and a sync, for the two credential paths.
    pub(crate) fn record_error(&mut self, message: String, status: Option<u16>, attempt: u32) {
        let _ = self.record(EventBody::Error {
            location: "provider".into(),
            message,
            retryable: false,
            status,
            attempt: Some(attempt),
            retry_in_ms: None,
        });
    }

    /// Stream one response, taking verbs while it is in flight. A `stop` ends the turn without
    /// committing the half-response; anything else waits for the next boundary.
    pub(crate) async fn stream(
        &mut self,
        rx: &mut Inbox,
        request: RequestFor<'_>,
        summary: bool,
    ) -> Result<StreamEnd, String> {
        let policy = self.cfg.retry;
        let budget_wait = if summary {
            Some(Duration::ZERO)
        } else {
            policy.max_wait
        };
        let mut budget = crate::retry::Budget::new(budget_wait, policy.max_attempts);
        let mut attempt = 1u32;
        let mut rng = crate::retry::Xorshift::from_clock();
        let mut inbox_open = true;
        // The first request is the one `drive_turn` built (and hashed into
        // `step_started`); a retry is a new request and is built again.
        let mut built = request.build(self);
        // A `model`/`say --now` that cut a wait short already applied its verb, so the next request
        // is built (new model, new fold) but the `before_step` boundary is not run again.
        let mut rebuild = false;
        // The body field a 400 asked kelir to drop, once per request : the second 400 is the answer,
        // not a queue of fields to keep removing.
        let mut dropped: Option<String> = None;
        // Whether this request already re-read the key once (see
        // [`Session::reread_credential`]).
        let mut reread = false;
        loop {
            // The governor (opt-in) can make this request wait for a slot that another local kelir
            // holds. That wait is visible and interruptible like any other.
            let mut slot: Option<crate::governor::Slot> = None;
            if let Some(governor) = self.provider.governor.clone() {
                match self.await_slot(rx, &governor, &mut inbox_open).await? {
                    SlotWait::Taken(taken) => slot = taken,
                    SlotWait::Interrupted(end) => return Ok(end),
                }
            }
            // A *retry* is a new request, so a wait is a `before_step` boundary: a `model` verb that arrived
            // during the wait changes the model, a queued `say` joins the fold, and `end`/`stop` take effect here.
            if !summary && attempt > 1 {
                let applied = self.apply(Boundary::BeforeStep).await?;
                if applied.end {
                    return Ok(StreamEnd::End);
                }
                if let Some(reason) = applied.stopped {
                    return Ok(StreamEnd::Stopped(reason));
                }
                rebuild = true;
            }
            if rebuild {
                built = request.build(self);
                rebuild = false;
            }
            budget.record_attempt();
            let outcome = self.attempt(rx, &built, summary, &mut inbox_open).await;
            // The slot is held for exactly one request: another local kelir can
            // start as soon as this one's answer is in.
            drop(slot);
            let error = match outcome {
                Attempt::End(end) => return Ok(end),
                Attempt::Ok(assembler) => return Ok(StreamEnd::Assembled(assembler)),
                Attempt::Failed(error) => error,
            };
            // A 400 that names a field kelir put in the body: drop it, remember it for the session, and try once
            // more with the same request without it.
            if self.drop_refused_field(&mut built, &error, &mut dropped) {
                self.record(EventBody::Error {
                    location: "provider".into(),
                    message: format!(
                        "{}: retrying without `{}` and leaving it out for the rest of the session",
                        error.message,
                        dropped.as_deref().unwrap_or("?")
                    ),
                    retryable: false,
                    status: error.status,
                    attempt: Some(attempt),
                    // Not a wait: the retry is sent at once, so no countdown.
                    retry_in_ms: None,
                })?;
                self.sync();
                continue;
            }
            // The credential went stale: opencode rotates its own token, so the store may hold a fresh
            // one already. One re-read per request, then the request is sent again.
            if self.reread_credential(&error, &mut built, attempt, &mut reread) {
                self.sync();
                continue;
            }
            // A summary is tried once; a step within its budget.
            let will_retry = error.retryable && !summary && budget.fits(Duration::ZERO);
            // The wait, decided before it is recorded so the event can say how
            // long it is (and the screen can count it down).
            let wait = will_retry
                .then(|| crate::retry::delay(&policy, attempt - 1, error.retry_after, &mut rng))
                .filter(|wait| budget.fits(*wait));
            self.record(EventBody::Error {
                location: "provider".into(),
                message: error.message.clone(),
                retryable: error.retryable,
                status: error.status,
                attempt: Some(attempt),
                retry_in_ms: wait.map(|wait| wait.as_millis() as u64),
            })?;
            if !will_retry {
                if !summary {
                    self.run.retryable_failure = error.retryable;
                }
                // A retryable failure whose budget is spent is an outage, not a bug: the turn pauses and a
                // later `say` starts the next attempt from the same fold.
                if error.retryable && !summary {
                    self.sync();
                    return Ok(StreamEnd::Paused(PauseReport {
                        reason: PauseReason::Provider,
                        attempts: budget.attempts(),
                        waited: budget.spent(),
                        detail: error.reason(),
                    }));
                }
                return Err(error.message);
            }
            self.sync();
            // A wait the budget cannot afford (or the count cap reached) means the provider asked for
            // longer than what is left: pausing now is honest, waiting a little and failing is not.
            let Some(wait) = wait else {
                self.sync();
                return Ok(StreamEnd::Paused(PauseReport {
                    reason: PauseReason::Provider,
                    attempts: budget.attempts(),
                    waited: budget.spent(),
                    detail: error.reason(),
                }));
            };
            budget.charge(wait);
            self.show_wait(error.status, wait, attempt);
            match self.wait_for(rx, wait, &mut inbox_open).await? {
                Waited::Timeout => {}
                // `model`/`say --now`: a *new* request, sent now, with a fresh slot and a fresh budget.
                Waited::Retry => {
                    self.emit(crate::tui::View::Live(crate::tui::Live::RetryingDone));
                    budget = crate::retry::Budget::new(budget_wait, policy.max_attempts);
                    attempt = 1;
                    rebuild = true;
                    continue;
                }
                // A verb ended the turn during the wait: the wait's note is cleared here too, so no
                // swap/`end` race can leave a stale countdown behind.
                Waited::End(end) => {
                    self.emit(crate::tui::View::Live(crate::tui::Live::RetryingDone));
                    return Ok(*end);
                }
            }
            self.emit(crate::tui::View::Live(crate::tui::Live::RetryingDone));
            attempt = attempt.saturating_add(1);
        }
    }

    /// One provider request, with verbs taken while it is in flight.
    pub(crate) async fn attempt(
        &mut self,
        rx: &mut Inbox,
        request: &provider::BuiltRequest,
        summary: bool,
        inbox_open: &mut bool,
    ) -> Attempt {
        let client = self.provider.client.clone();
        // Live text goes to the UI as it arrives; the callback is a send to an
        // unbounded channel, so it can never stall the stream.
        let ui = if summary { None } else { self.out.ui.clone() };
        // Documented test hook: die with half a response streamed and nothing
        // committed, which is where a crash in the stream lands.
        let crash_mid_stream = !summary && crash_armed("mid_stream");
        let mut on_delta = move |delta: crate::stream::Delta| {
            if let crate::stream::Delta::Text(text) = &delta {
                if crash_mid_stream {
                    crash_now("mid_stream");
                }
                if let Some(ui) = &ui {
                    let _ = ui.send(crate::tui::View::Live(crate::tui::Live::Delta(
                        text.clone(),
                    )));
                }
            }
        };
        let future = client.stream_with(request, &mut on_delta);
        tokio::pin!(future);
        let mut outcome: Option<Result<crate::stream::Assembler, provider::ProviderError>> = None;
        let mut interrupt: Option<StreamEnd> = None;
        while outcome.is_none() && interrupt.is_none() {
            tokio::select! {
                result = &mut future => outcome = Some(result),
                incoming = rx.recv(), if *inbox_open => {
                    match incoming {
                        Some(incoming) => self.take_interrupt(incoming, &mut interrupt),
                        None => *inbox_open = false,
                    }
                }
            }
        }
        if let Some(end) = interrupt {
            return Attempt::End(end);
        }
        match outcome.expect("the stream ended without a result") {
            Ok(assembler) => Attempt::Ok(assembler),
            Err(error) => Attempt::Failed(error),
        }
    }

    /// Wait `wait`, taking verbs.
    pub(crate) async fn wait_for(
        &mut self,
        rx: &mut Inbox,
        wait: Duration,
        inbox_open: &mut bool,
    ) -> Result<Waited, String> {
        if wait.is_zero() {
            return Ok(Waited::Timeout);
        }
        // An interrupt that landed before this wait began is still pending (the flag is level- triggered and
        // no one else consumes it), so a wait never has to be entered at exactly the right instant to see it —
        let deadline = tokio::time::sleep(wait);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => return Ok(Waited::Timeout),
                _ = crate::signals::interrupted() => {
                    // One interrupt ends one turn: clear the level so the next
                    // wait is not ended by the same Ctrl-C.
                    crate::signals::take();
                    // The turn ends `stopped`, exactly like `stop`: the log
                    // says so and a one-shot run exits 0.
                    return Ok(Waited::End(Box::new(StreamEnd::Stopped(
                        "interrupted (Ctrl-C)".into(),
                    ))));
                }
                incoming = rx.recv(), if *inbox_open => match incoming {
                    Some(incoming) => {
                        let mut end: Option<StreamEnd> = None;
                        let resume = self.take_wait_verb(incoming, &mut end).await?;
                        if let Some(end) = end {
                            return Ok(Waited::End(Box::new(end)));
                        }
                        if resume {
                            return Ok(Waited::Retry);
                        }
                    }
                    None => *inbox_open = false,
                }
            }
        }
    }

    /// Handle one verb that arrived *during* a retry or slot wait. Returns
    /// whether the wait should end with the request re-sent at once.
    pub(crate) async fn take_wait_verb(
        &mut self,
        incoming: Incoming,
        interrupt: &mut Option<StreamEnd>,
    ) -> Result<bool, String> {
        // `say --now` and `model` are applied here and now, so they are accepted and answered as
        // such: `applies: "now"`, with the state the next request will really see.
        if at_once_verb(&incoming.request) {
            return Ok(match self.accept_at_once(incoming).await {
                AtOnce::Applied(end) => {
                    if let Some(end) = end {
                        *interrupt = Some(*end);
                    }
                    true
                }
                // A refused verb changed nothing: the wait it arrived in goes on.
                AtOnce::Rejected => false,
            });
        }
        let Some(pending) = self.accept(incoming) else {
            return Ok(false);
        };
        match &pending.verb {
            Verb::Stop { call: None, reason } => {
                *interrupt = Some(StreamEnd::Stopped(
                    reason.clone().unwrap_or_else(|| "stopped by user".into()),
                ));
                Ok(false)
            }
            Verb::End => {
                *interrupt = Some(StreamEnd::End);
                Ok(false)
            }
            // `say --now` and `model` never reach here: the branch above applies them through
            // `accept_at_once`, which is what makes the ack truthful about a swap that already happened.
            Verb::Say { .. } | Verb::Model { .. } => {
                let mut applied = Option::<StreamEnd>::None;
                self.apply_one(pending, &mut applied).await?;
                if let Some(end) = applied {
                    *interrupt = Some(end);
                }
                Ok(true)
            }
            _ => {
                // Anything else keeps the boundary it asked for (`later` at the
                // turn end, a tool/skill change at `before_step`).
                self.scheduler.push(pending);
                Ok(false)
            }
        }
    }

    /// Apply one verb now (the body of [`Session::apply`]'s loop for one
    /// `Pending`).
    pub(crate) async fn apply_one(
        &mut self,
        pending: Pending,
        end: &mut Option<StreamEnd>,
    ) -> Result<(), String> {
        let mut applied = Applied::default();
        self.apply_pending(pending, &mut applied).await?;
        if applied.end {
            *end = Some(StreamEnd::End);
        }
        if let Some(reason) = applied.stopped {
            *end = Some(StreamEnd::Stopped(reason));
        }
        Ok(())
    }
}
