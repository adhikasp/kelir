//! The tool half of the session loop: run a batch, settle a call, kill a tree.
// These private loop modules expose crate-visible helpers to the feature modules.
#![allow(clippy::redundant_pub_crate)]

use super::types::*;
use super::*;

impl Session {
    pub(crate) async fn run_tools(
        &mut self,
        rx: &mut Inbox,
        step: u32,
        calls: &[ToolCall],
    ) -> Result<Batch, String> {
        // `before_tool` gates: a deny settles the call as `denied` without running it; a rewrite replaces the
        let mut calls: Vec<ToolCall> = calls.to_vec();
        let mut vetoed: Vec<Option<(String, String)>> = vec![None; calls.len()];
        self.gates_do(|gates, _| gates.add_turn_tool_calls(calls.len() as u32));
        for index in 0..calls.len() {
            let name = calls[index].name.clone();
            if !self
                .features
                .gates
                .as_ref()
                .expect("gates feature")
                .gated(crate::gates::Event::BeforeTool, Some(&name))
            {
                continue;
            }
            let (args, veto) = crate::features::gates::apply_tool_verdict(
                self,
                &name,
                &calls[index].id,
                calls[index].args.clone(),
            )
            .await?;
            calls[index].args = args;
            if let Some(veto) = veto {
                vetoed[index] = Some(veto);
            }
        }
        let calls = &calls[..];
        let mut plan: Vec<(String, ToolCall)> = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            let dir_name = format!("{}-{}-{}", self.log.writer.next_seq(), step, index + 1);
            self.record(crate::event::call_start(
                &call.id,
                "tool",
                crate::event::CallInfo {
                    name: call.name.clone(),
                    input: Some(Payload::value(call.args.clone())),
                    dir: Some(dir_name.clone()),
                    ..crate::event::CallInfo::default()
                },
            ))?;
            self.emit(UiEvent::ToolStarted {
                name: call.name.clone(),
                input: call.args.clone(),
            });
            let dir = dir_name;
            // The job registry: a call's output dir name is the id its `bg` reports carry, so the
            // session registers it before the shim starts.
            let _ = self.trust.issue_call(&dir, &call.id);
            plan.push((dir, call.clone()));
        }
        self.sync();
        // Documented test hook: die after the intent is durable, before any process started, which
        // is where a crash right after the log write lands.
        crash_at("tool_started");

        let count = plan.len();
        let (done_tx, mut done_rx) =
            mpsc::channel::<(usize, ToolOutcome, Duration)>(count.max(1) * 2);
        // A call whose gate closed before its task was polled never starts: that
        // is the `skipped` case.
        let gates: Vec<Arc<AtomicBool>> = (0..count)
            .map(|_| Arc::new(AtomicBool::new(false)))
            .collect();
        let mut handles = Vec::with_capacity(count);
        for (index, (dir, call)) in plan.iter().enumerate() {
            let ctx = self.ctx.clone();
            let dir = dir.clone();
            let name = call.name.clone();
            let args = call.args.clone();
            let disabled = self.tools.disabled.contains(&call.name);
            // `compact` is a native tool whose work is the session's: a request
            // for the next boundary, answered here like any other call.
            let preset = if let Some((gate, reason)) = &vetoed[index] {
                Some(ToolOutcome::denied(gate, reason))
            } else if disabled {
                Some(ToolOutcome::disabled(&name))
            } else if self.kids.brief && (name == "compact" || name == "edit") {
                // The tool list is the parent's, byte for byte (that is what lets the prefix hit its cache);
                // a brief answers, it does not change anything, so these two are refused by result.
                Some(ToolOutcome::error(format!(
                    "refused: this is a brief, a read-only side question on a snapshot; `{name}` is not available here. Answer from what you can read with bash"
                )))
            } else if name == "compact" {
                let focus = call
                    .args
                    .get("focus")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
                    .filter(|focus| !focus.trim().is_empty());
                self.compact.request = Some(CompactRequest {
                    by: "agent",
                    focus: focus.clone(),
                    immediate: false,
                });
                Some(ToolOutcome::ok(compact_queued(&focus)))
            } else {
                None
            };
            let gate = gates[index].clone();
            let tx = done_tx.clone();
            let call_id = call.id.clone();
            handles.push(tokio::spawn(async move {
                let started = Instant::now();
                let outcome = if gate.load(Ordering::SeqCst) {
                    ToolOutcome::skipped("stopped before it started")
                } else if let Some(preset) = preset {
                    preset
                } else {
                    tools::dispatch(&name, &args, &ctx, &dir, &call_id).await
                };
                let _ = tx.send((index, outcome, started.elapsed())).await;
            }));
        }
        drop(done_tx);
        // Documented test hook: die with the calls running and their side
        // effects already made, which is where a crash mid-tool lands.
        if crash_armed("mid_tool") {
            tokio::time::sleep(Duration::from_millis(150)).await;
            crash_now("mid_tool");
        }

        let mut slots: Vec<Option<(ToolOutcome, Duration)>> = (0..count).map(|_| None).collect();
        // Killed calls: their result becomes `stopped by user: <reason>`.
        let mut forced: Vec<Option<String>> = (0..count).map(|_| None).collect();
        // A grace window per killed call, never one for the batch: a sibling
        // that legitimately runs longer must not be cut off.
        let mut deadlines: Vec<Option<Instant>> = (0..count).map(|_| None).collect();
        let mut batch = Batch::default();

        while slots.iter().any(Option::is_none) {
            // A call whose own window ran out is settled here; the rest of the
            // batch is untouched.
            let now = Instant::now();
            for index in 0..count {
                let Some(end) = deadlines[index] else {
                    continue;
                };
                if now < end || slots[index].is_some() {
                    continue;
                }
                deadlines[index] = None;
                let reason = forced[index].clone().unwrap_or_default();
                slots[index] = Some((ToolOutcome::stopped(&reason), Duration::ZERO));
                handles[index].abort();
                self.settle_call(&plan[index].1.id);
            }
            tokio::select! {
                item = done_rx.recv() => match item {
                    Some((index, outcome, elapsed)) => {
                        if let Some(deadline) = deadlines.get_mut(index) {
                            *deadline = None;
                        }
                        if let Some(slot) = slots.get_mut(index) {
                            *slot = Some((outcome, elapsed));
                        }
                        if let Some((_, call)) = plan.get(index) {
                            self.settle_call(&call.id);
                        }
                    }
                    None => break,
                },
                incoming = rx.recv() => {
                    let Some(incoming) = incoming else { break };
                    let Some(pending) = self.accept(incoming) else { continue };
                    match pending.verb.clone() {
                        Verb::Stop { call: Some(id), reason } => {
                            let reason = reason.unwrap_or_default();
                            if let Some(index) = plan.iter().position(|(_, call)| call.id == id) {
                                gates[index].store(true, Ordering::SeqCst);
                                self.ctx.running.kill(&plan[index].0);
                                if slots[index].is_none() {
                                    forced[index].get_or_insert(reason);
                                    deadlines[index] = Some(Instant::now() + STOP_GRACE);
                                }
                            }
                        }
                        Verb::Stop { call: None, reason } => {
                            let reason = reason.unwrap_or_default();
                            self.kill_batch(
                                &plan,
                                &slots,
                                &gates,
                                &mut forced,
                                &mut deadlines,
                                &reason,
                            );
                            batch.stopped = Some(reason);
                        }
                        Verb::Say { now: true, .. } => {
                            // Stop the running calls, settle, and let the
                            // message in at the after_tools boundary.
                            self.kill_batch(&plan, &slots, &gates, &mut forced, &mut deadlines, "");
                            self.scheduler.push(pending);
                        }
                        Verb::End => {
                            self.kill_batch(&plan, &slots, &gates, &mut forced, &mut deadlines, "");
                            batch.end = true;
                        }
                        _ => self.scheduler.push(pending),
                    }
                }
                _ = tokio::time::sleep(STOP_POLL), if deadlines.iter().any(Option::is_some) => {
                    // A call that started just after the kill is taken down too,
                    // so no orphan keeps working after the turn moved on.
                    let reason = batch.stopped.as_deref().unwrap_or("");
                    self.retake(&plan, &slots, &deadlines, &mut forced, reason);
                }
            }
        }

        // Anything still unsettled is settled by the session: aborted, and
        // reported as stopped (it ran) or skipped (it never did).
        for handle in &handles {
            handle.abort();
        }
        for (index, slot) in slots.iter_mut().enumerate() {
            if slot.is_some() {
                continue;
            }
            let outcome = match (&forced[index], &batch.stopped) {
                (Some(reason), _) => ToolOutcome::stopped(reason),
                (None, Some(reason)) => ToolOutcome::skipped(reason),
                (None, None) => ToolOutcome::error(format!(
                    "tool task for call {} did not settle (panicked or cancelled)",
                    index + 1
                )),
            };
            *slot = Some((outcome, Duration::ZERO));
        }
        // Every call of the batch has settled: none is a `stop --call` target
        // any more, and the next batch repopulates the list.
        self.tools.live_calls.clear();

        // Calls that became background jobs, recorded after the whole batch.
        let mut promoted: Vec<String> = Vec::new();
        for (index, ((dir, call), slot)) in plan.iter().zip(slots).enumerate() {
            let (mut outcome, elapsed) = slot.unwrap_or_else(|| {
                (
                    ToolOutcome::error("the call never reported a result"),
                    Duration::ZERO,
                )
            });
            if let Some(reason) = &forced[index] {
                // The call was stopped: keep the partial output the model can
                // still use, and say where the whole of it is.
                outcome = self.stopped_outcome(dir, reason);
            }
            // `after_tool` gates may replace the result the model reads,
            // or append a note to it. Only a call that ran and finished.
            if forced[index].is_none()
                && matches!(outcome.outcome.as_str(), "ok" | "error")
                && self
                    .features
                    .gates
                    .as_ref()
                    .expect("gates feature")
                    .gated(crate::gates::Event::AfterTool, Some(&call.name))
            {
                let payload = json!({
                    "tool": call.name,
                    "call_id": call.id,
                    "input": call.args,
                    "result": outcome.result,
                    "outcome": outcome.outcome,
                    "exit_code": outcome.exit_code,
                    "out_path": outcome.out_path,
                });
                if let crate::gates::Verdict::Rewrite(value) = self
                    .gates_call(crate::gates::Event::AfterTool, Some(&call.name), payload)
                    .await?
                    && let Some(result) = value.get("result").and_then(Value::as_str)
                {
                    outcome.result =
                        crate::util::truncate_bytes(result, self.cfg.spill_limit.max(1024));
                }
            }
            self.record(crate::event::call_settle(
                &call.id,
                "tool",
                &outcome.outcome,
                crate::event::CallSettle::default()
                    .exit_code(outcome.exit_code)
                    .bytes(outcome.bytes)
                    .lines(outcome.lines)
                    .out_path(&outcome.out_path)
                    .spilled(outcome.spilled)
                    .result(outcome.result.clone().into()),
            ))?;
            // A promoted call became a background job at this instant: a reader of the log should see the job, not
            // have to infer it from the `promoted` outcome.
            if outcome.outcome == "promoted" {
                promoted.push(dir.clone());
            }
            // The TUI renders its own line from `UiEvent::ToolSettled`; only the
            // headless path prints here, so the painter is the only writer.
            if !self.out.json && !self.out.tui {
                let line = format!(
                    "→ {} {} · {} · {} B · {:.1}s",
                    call.name,
                    compact_input(&call.args),
                    outcome.outcome,
                    outcome.bytes,
                    elapsed.as_secs_f64()
                );
                // A child's stdout is its answer, which its parent reads: the progress lines go to stderr,
                // where the parent keeps them in the child's `host.err`.
                if self.kids.root.is_some() {
                    eprintln!("{line}");
                } else {
                    println!("{line}");
                }
            }
            // A failed call's tail row: the last line it printed, so the
            // transcript says *why* without unfolding anything.
            let tail = if outcome.outcome == "ok" && outcome.exit_code.unwrap_or(0) == 0 {
                String::new()
            } else {
                call_tail(&outcome)
                    .or_else(|| last_result_line(&outcome.result))
                    .unwrap_or_default()
            };
            self.emit(UiEvent::ToolSettled {
                name: call.name.clone(),
                input: call.args.clone(),
                outcome: outcome.outcome.clone(),
                exit_code: outcome.exit_code,
                elapsed_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
                bytes: outcome.bytes,
                lines: outcome.lines,
                spilled: outcome.spilled,
                out_path: outcome.out_path.clone(),
                tail,
            });
        }
        self.sync();
        // Now that the batch's results are all in the log, a promoted call's job can be recorded
        for dir in promoted {
            if let Some(status) = crate::bg::Status::read(&crate::bg::status_path(
                &crate::paths::calls_dir(&self.log.dir),
                &dir,
            )) {
                let _ = self.note_bg(&status);
            }
        }
        // Documented test hook: die with the batch committed, which is where a
        crash_at("tool_settled");
        Ok(batch)
    }

    /// One call is done: it is no longer a target for `stop --call`.
    pub(crate) fn settle_call(&mut self, call_id: &str) {
        self.tools.live_calls.retain(|id| id != call_id);
    }

    /// The `stopped` result of a call that had already written output: the byte and line counts,
    /// the file, and the last 1 KB, so the model can pick up the partial work.
    pub(crate) fn stopped_outcome(&self, dir: &str, reason: &str) -> ToolOutcome {
        let out_path = self.ctx.call_dir(dir).join("out");
        match crate::spill::partial(&out_path) {
            Ok(partial) if partial.bytes > 0 => ToolOutcome::stopped_with_output(
                reason,
                &crate::util::path_string(&out_path),
                partial.bytes,
                partial.lines,
                &partial.tail,
            ),
            _ => ToolOutcome::stopped(reason),
        }
    }

    /// Kill every call that has a process, and gate the ones that do not.
    /// Calls already settled keep their real result.
    pub(crate) fn kill_batch(
        &self,
        plan: &[(String, ToolCall)],
        slots: &[Option<(ToolOutcome, Duration)>],
        gates: &[Arc<AtomicBool>],
        forced: &mut [Option<String>],
        deadlines: &mut [Option<Instant>],
        reason: &str,
    ) {
        let running = self.ctx.running.keys();
        for (index, (dir, _)) in plan.iter().enumerate() {
            gates[index].store(true, Ordering::SeqCst);
            if slots[index].is_some() || forced[index].is_some() {
                continue;
            }
            if running.iter().any(|key| key == dir) {
                self.ctx.running.kill(dir);
                forced[index] = Some(reason.to_string());
            }
            // Every unsettled call gets its own window, whether or not it had a process yet: one that starts just
            // after the gate closed is taken down by the poll, and a task that never reports back must not hold
            deadlines[index] = Some(Instant::now() + STOP_GRACE);
        }
    }

    /// Take down calls that registered a process after the kill.
    pub(crate) fn retake(
        &self,
        plan: &[(String, ToolCall)],
        slots: &[Option<(ToolOutcome, Duration)>],
        deadlines: &[Option<Instant>],
        forced: &mut [Option<String>],
        reason: &str,
    ) {
        let running = self.ctx.running.keys();
        for (index, (dir, _)) in plan.iter().enumerate() {
            if slots[index].is_some() || deadlines[index].is_none() {
                continue;
            }
            if running.iter().any(|key| key == dir) {
                self.ctx.running.kill(dir);
                if forced[index].is_none() {
                    forced[index] = Some(reason.to_string());
                }
            }
        }
    }

    /// Wait for a governor slot, polling (there is nothing to be told about: the slot appears
    /// when another process closes its handle). The status line says so while it lasts.
    pub(crate) async fn await_slot(
        &mut self,
        rx: &mut Inbox,
        governor: &crate::governor::Governor,
        inbox_open: &mut bool,
    ) -> Result<SlotWait, String> {
        let mut announced = false;
        loop {
            if let Some(slot) = governor.try_acquire() {
                if announced {
                    self.emit(UiEvent::RetryingDone);
                }
                return Ok(SlotWait::Taken(Some(slot)));
            }
            if !announced {
                announced = true;
                let line = format!("waiting for a provider slot ({})", governor.slots());
                self.emit(UiEvent::Info(format!("» {line} …")));
                self.emit(UiEvent::Retrying {
                    line,
                    until: Instant::now() + Duration::from_secs(86_400),
                    // A slot wait is not a provider retry: no status, no attempt,
                    // so the state row keeps saying what it was doing.
                    status: None,
                    attempt: 0,
                });
            }
            match self.wait_for(rx, SLOT_POLL, inbox_open).await? {
                // The slot poll is a poll: a verb that ends the *wait* just means "look again", and the next
                // look takes the slot if it is free. The verb itself was already applied or queued.
                Waited::Timeout | Waited::Retry => {}
                Waited::End(end) => {
                    self.emit(UiEvent::RetryingDone);
                    return Ok(SlotWait::Interrupted(*end));
                }
            }
        }
    }

    /// Say what is happening and start the status line's countdown. The `error`
    /// event was already recorded, so NDJSON carries it; this is the human line.
    pub(crate) fn show_wait(&mut self, status: Option<u16>, wait: Duration, attempt: u32) {
        let line = crate::retry::wait_line(status, wait, attempt);
        self.emit(UiEvent::Info(format!("» {line}")));
        if !self.out.json && !self.out.tui {
            eprintln!("kelir: {line}");
        }
        self.emit(UiEvent::Retrying {
            line,
            until: Instant::now() + wait,
            status,
            attempt,
        });
    }
}
