//! The outer loop: run, idle and the boundary helpers.
// These private loop modules expose crate-visible helpers to the feature modules.
#![allow(clippy::redundant_pub_crate)]

use super::*;

impl Session {
    // --------------------------------------------------------------- turns

    /// Run the session: one turn, then more when `--keep` or when messages are
    /// queued. Ends with `session_ended`.
    pub(crate) async fn run(&mut self, rx: &mut Inbox) -> u8 {
        let mut code = 0u8;
        // Every way out of the loop below sets it (the loop always runs at
        // least one turn).
        let mut reason: String;
        // A resumed session with nothing in flight and nothing new has no turn to run: `kelir run --resume <id>` on
        // a finished session reports and leaves.
        if self.run.resumed && !self.run.in_flight && !self.run.has_task && !self.out.keep {
            let _ = self.record(EventBody::SessionEnded {
                reason: "resumed".into(),
            });
            self.sync();
            return 0;
        }
        // `session_start` gates, then the command-line task through `prompt_submit`. A task a gate
        // dropped ends a one-shot run (exit 1) and leaves a TUI waiting for the next message.
        let task_kept = self.gates_start().await;
        if !task_kept && !self.out.keep {
            self.gates_end("denied").await;
            let _ = self.record(EventBody::SessionEnded {
                reason: "denied".into(),
            });
            self.sync();
            self.trust.retire();
            return 1;
        }
        // A TUI session starts with no task: wait for the first message.
        if (!self.run.has_task || !task_kept) && self.out.keep {
            match self.idle(rx).await {
                Idle::Turn => {}
                Idle::End => {
                    self.gates_end("end").await;
                    let _ = self.record(EventBody::SessionEnded {
                        reason: "end".into(),
                    });
                    self.sync();
                    return 0;
                }
            }
        }
        let mut turn_retries = 0u32;
        loop {
            self.run.turn += 1;
            let end = self.drive_turn(rx).await;
            let failed = match &end {
                Ok(TurnEnd::Done) => {
                    let _ = self.record(EventBody::TurnEnded {
                        outcome: "done".into(),
                        reason: None,
                        final_text: self.run.final_text.clone().into(),
                    });
                    self.finish_turn("done");
                    code = 0;
                    reason = "done".into();
                    false
                }
                Ok(TurnEnd::Stopped(why)) => {
                    if !self.out.json && !self.out.tui && !why.is_empty() {
                        eprintln!("→ turn stopped: {why}");
                    }
                    let _ = self.record(EventBody::TurnEnded {
                        outcome: "stopped".into(),
                        reason: None,
                        final_text: self.run.final_text.clone().into(),
                    });
                    self.finish_turn("stopped");
                    reason = "stopped".into();
                    false
                }
                Ok(TurnEnd::End) => {
                    let _ = self.record(EventBody::TurnEnded {
                        outcome: "stopped".into(),
                        reason: None,
                        final_text: self.run.final_text.clone().into(),
                    });
                    self.finish_turn("stopped");
                    reason = "end".into();
                    false
                }
                // A pause is never a failure: nothing is broken and the fold is intact.
                Ok(TurnEnd::Paused(report)) => {
                    let message = report.message();
                    if report.reason == PauseReason::Steps {
                        if !self.out.tui {
                            eprintln!("→ {message}");
                        } else {
                            self.ui_note(message.clone());
                        }
                    } else {
                        if !self.out.tui {
                            eprintln!("kelir: {message}");
                        }
                        self.ui_note(format!("» {message}"));
                        self.run.final_text = message.clone();
                    }
                    let text = match report.reason {
                        PauseReason::Steps => self.run.final_text.clone(),
                        PauseReason::Provider => message.clone(),
                    };
                    let _ = self.record(EventBody::TurnEnded {
                        outcome: "paused".into(),
                        reason: Some(report.reason.as_str().into()),
                        final_text: text.into(),
                    });
                    self.finish_turn("paused");
                    code = 0;
                    reason = "paused".into();
                    false
                }
                Err(message) => {
                    if !self.out.tui {
                        eprintln!("kelir: {message}");
                    }
                    self.ui_error(message.clone());
                    let _ = self.record(EventBody::TurnEnded {
                        outcome: "error".into(),
                        reason: None,
                        final_text: String::new().into(),
                    });
                    self.finish_turn("error");
                    // A long-lived session survives a failed turn: the error is in the log and the next `say`
                    // gets another chance ( ). A one-shot run still exits 1.
                    if !self.out.keep {
                        code = 1;
                    }
                    reason = "error".into();
                    true
                }
            };
            self.sync();
            // A resumed turn that failed on an error the provider called retryable — an outage — gets the turn
            // tried again: surviving one is what `resume` is for, and a provider often recovers within seconds.
            let retry_the_turn = failed
                && !self.out.keep
                && self.run.resumed
                && self.run.retryable_failure
                && turn_retries + 1 < provider::TURN_ATTEMPTS;
            if matches!(end, Ok(TurnEnd::End)) || (failed && !self.out.keep && !retry_the_turn) {
                break;
            }
            if retry_the_turn {
                turn_retries += 1;
                self.run.retryable_failure = false;
                code = 0;
                continue;
            }
            match self.take_turn_messages() {
                Some(TurnStart::End) => {
                    reason = "end".into();
                    break;
                }
                Some(TurnStart::Messages(messages)) => {
                    if let Err(message) = self.record_messages(messages).await {
                        eprintln!("kelir: {message}");
                        code = 1;
                        reason = "error".into();
                        break;
                    }
                    continue;
                }
                None => {
                    if !self.out.keep {
                        break;
                    }
                    match self.idle(rx).await {
                        Idle::Turn => continue,
                        Idle::End => {
                            reason = "end".into();
                            break;
                        }
                    }
                }
            }
        }
        self.gates_end(&reason).await;
        // Give detached notify hooks a short grace period before a one-shot process exits: otherwise
        // the runtime is dropped with the hook task still queued and `turn_done` never fires.
        crate::features::notify::wait_outstanding(Duration::from_secs(5)).await;
        let _ = self.record(EventBody::SessionEnded { reason });
        self.sync();
        // The session is ending: the token it minted is no longer served by anything, so it must not stay on
        // disk for whoever comes next ( ).
        self.trust.retire();
        code
    }

    /// `--keep` and no turn running: take verbs until one starts a turn.
    pub(crate) async fn idle(&mut self, rx: &mut Inbox) -> Idle {
        loop {
            // A metadata fetch that started with the session lands here as well as at `before_step`: an idle
            // session answers `state` too, and a list that arrived while nobody was asking should still be applied
            let landed = match self.features.models.fetch.as_mut() {
                Some(fetch) if fetch.is_running() => fetch.wait(IDLE_MODELS_WAIT).await,
                _ => None,
            };
            if let Some(list) = landed {
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
                    out: Out { ui, json, .. },
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
                };
                features.models.apply_list(list, &mut cx);
                continue;
            }
            // A fetch still running must not strand its answer: blocking on `rx.recv` here would keep the idle
            // session from ever applying a list that lands a second later (the offline reason the banner and
            let incoming = match self.features.models.fetch.as_ref().map(Fetch::is_running) {
                Some(true) => {
                    tokio::select! {
                        incoming = rx.recv() => incoming,
                        _ = tokio::time::sleep(IDLE_MODELS_WAIT) => continue,
                    }
                }
                _ => rx.recv().await,
            };
            let Some(mut incoming) = incoming else {
                return Idle::End;
            };
            if let Some(note) = self.note_own(incoming.action.take()) {
                // Its own watcher: a job started or settled while idle. It is queued like the `bg` report it
                // stands in for, and the turn it may start begins below, through `take_turn_messages`.
                self.scheduler.push(note);
            } else {
                // Nothing is running in idle, so a `stop` has nothing to stop: queueing it would stop the *next* turn
                // at its first boundary, and answering "now" would be a lie.
                let bare_stop =
                    incoming.request.verb == "stop" && incoming.request.args.get("call").is_none();
                let pending = self.accept_as(incoming, bare_stop.then_some("dropped"));
                if let Some(pending) = pending {
                    if matches!(pending.verb, Verb::Stop { call: None, .. }) {
                        self.ui_note("» stop: nothing is running");
                    } else if let Verb::Compact { focus } = &pending.verb {
                        // A human asked to compact while idle: nothing is running, so this is safe to do *now*.
                        self.ui_note(match focus {
                            Some(focus) => format!("» compact: {focus}"),
                            None => "» compact".to_string(),
                        });
                        self.compact.request = Some(CompactRequest {
                            by: "user",
                            focus: focus.clone(),
                            immediate: true,
                        });
                    } else {
                        self.scheduler.push(pending);
                    }
                }
            }
            // An immediate `/compact` runs here, at a point with no tool call in flight.
            if self.compact.request.as_ref().is_some_and(|r| r.immediate) {
                let request = self.compact.request.take().expect("checked above");
                if let Err(message) = self.run_compact(rx, &request).await {
                    self.ui_note(format!("» compact failed: {message}"));
                }
            }
            match self.take_turn_messages() {
                Some(TurnStart::End) => return Idle::End,
                Some(TurnStart::Messages(messages)) => {
                    if self.record_messages(messages).await.is_err() {
                        return Idle::End;
                    }
                    // A `cancel` queued in the same breath must still be
                    // applied (it is not a message and no boundary is coming).
                    if self.apply_actions().await.is_err() {
                        return Idle::End;
                    }
                    return Idle::Turn;
                }
                None => {}
            }
            if self.apply_actions().await.is_err() {
                return Idle::End;
            }
        }
    }

    /// Handle one verb that arrived while a request was in flight. `stop` and
    /// `end` interrupt now; anything else waits for its boundary.
    pub(crate) fn take_interrupt(&mut self, incoming: Incoming, interrupt: &mut Option<StreamEnd>) {
        if let Some(pending) = self.accept(incoming) {
            match pending.verb {
                Verb::Stop { call: None, reason } => {
                    *interrupt = Some(StreamEnd::Stopped(
                        reason.unwrap_or_else(|| "stopped by user".into()),
                    ));
                }
                Verb::End => *interrupt = Some(StreamEnd::End),
                _ => self.scheduler.push(pending),
            }
        }
    }
}
