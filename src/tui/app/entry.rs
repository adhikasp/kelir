//! App methods: entry.
use crate::tui::app::*;
use crate::tui::*;

impl App {
    pub fn push_text(&mut self, text: &str) {
        for line in text.lines() {
            self.append(Entry::Plain(line.to_string()));
        }
    }

    pub fn push_dim_text(&mut self, text: &str) {
        for line in text.lines() {
            self.append(Entry::Dim(line.to_string()));
        }
    }

    /// A prompt submitted here, echoed before the session records it.
    ///
    /// The *text* is what the log will carry, so that is what is remembered:
    /// `apply_log` matches an `EventBody::User` on its raw text, and a rendered
    /// row (`› hello`) would never match, so the log copy would be appended
    /// after it as a second, bare row.
    pub fn push_user(&mut self, text: &str) {
        self.own_user_echoes.push(text.to_string());
        self.push_user_row(&commands::echo_row(text, ""));
    }

    /// `/say-now`: the same user row, marked as taking effect immediately.
    pub fn push_user_now(&mut self, text: &str) {
        self.own_user_echoes.push(text.to_string());
        self.push_user_row(&commands::echo_row(text, "  (now)"));
    }

    /// A user row from the log: unlike a local echo it must not raise the
    /// counter that suppresses the log copy (attach has no local echo).
    pub(super) fn push_user_row(&mut self, text: &str) {
        for line in text.lines() {
            self.append(Entry::User(line.to_string()));
        }
    }

    pub fn push_lines(&mut self, lines: Vec<Line<'static>>) {
        for line in lines {
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            self.append(Entry::Dim(text));
        }
    }

    /// One blank row between two visual blocks, and never more than one.
    pub(super) fn separate(&mut self) {
        if let Some(last) = self.entries.last()
            && !last.text().trim().is_empty()
        {
            self.append(Entry::Plain(String::new()));
        }
    }

    pub(super) fn emit_markdown(&mut self, text: &str) {
        // The stream's tail was rendered as a draft entry; the finished text
        // replaces it rather than following it (or the answer appears twice).
        self.clear_draft();
        self.separate();
        let index = self.entries.len();
        self.entries.push(Entry::Markdown(text.to_string()));
        let lines = view::entry_lines(self, &self.entries[index], self.width);
        if self.scroll.is_some() {
            self.pending_new += lines.len();
        }
        self.cached_rows.extend(lines);
        self.draft.clear();
    }

    /// Apply one view event: a durable log fact or the few live-only facts.
    pub fn apply(&mut self, view: View) {
        match view {
            View::Live(live) => self.apply_live(live),
            View::Log(event) => self.apply_log(&event),
        }
    }

    pub(super) fn apply_live(&mut self, live: Live) {
        let now = Instant::now();
        match live {
            Live::Delta(text) => {
                self.status.phase = Phase::Working;
                self.activity.text(now, text.chars().count());
                self.clear_draft();
                let settled_bytes = self.stream.delivered_bytes();
                for closed in self.stream.push_blocks(&text) {
                    if !closed.text.trim().is_empty() {
                        let remainder = closed.text.get(settled_bytes..).unwrap_or("");
                        if !remainder.trim().is_empty() {
                            self.separate();
                            self.entries.push(Entry::Markdown(remainder.to_string()));
                            let lines =
                                view::entry_lines(self, self.entries.last().unwrap(), self.width);
                            self.cached_rows.extend(lines);
                        }
                    }
                }
                let layout =
                    crate::tui::markdown::Layout::with_glyphs(self.width, self.theme.glyphs);
                let from = self.stream.delivered_bytes();
                let settled = self.stream.settle(layout, 3);
                if !settled.is_empty() {
                    let to = self.stream.delivered_bytes();
                    let text = self.stream.open().get(from..to).unwrap_or("").to_string();
                    let lines: Vec<Line<'static>> = settled
                        .iter()
                        .map(|row| row.line(self.theme, &crate::tui::markdown::NoHighlight))
                        .collect();
                    self.pending_new += lines.len();
                    self.cached_rows.extend(lines.iter().cloned());
                    self.taken_rows = self.cached_rows.len();
                    self.entries.push(Entry::Settled {
                        text,
                        width: self.width,
                        lines,
                    });
                }
                let draft = self.stream.tail_remaining();
                self.set_draft(draft);
            }
            Live::Retrying {
                line,
                until,
                status,
                attempt,
            } => {
                let wait = until.saturating_duration_since(now);
                if self.activity.kind == Kind::RateLimited {
                    self.activity.tick_retry(wait);
                } else if status.is_some() || attempt > 0 {
                    let detail = status.map(|code| code.to_string()).unwrap_or_default();
                    self.activity.retry(now, wait, attempt, &detail);
                }
                if status.is_some() || attempt > 0 {
                    self.retry_deadline = Some(until);
                }
                self.status.retrying = Some((view::tui_wait_line(&line), until));
                self.note.clear();
            }
            Live::RetryingDone => {
                self.status.retrying = None;
                self.retry_deadline = None;
                self.note.clear();
                self.activity.retry_done(now);
            }
            Live::Reply(response) => {
                if response.ok {
                    let quiet = matches!(response.verb.as_deref(), Some("say") | Some("later"))
                        || (response.verb.as_deref() == Some("stop")
                            && response.applies.as_deref() == Some("dropped"));
                    if !quiet {
                        let verb = response.verb.unwrap_or_else(|| "verb".into());
                        let applies = response.applies.unwrap_or_else(|| "queued".into());
                        self.push_text(&format!("» {verb}: {applies}"));
                    }
                } else {
                    let message = response
                        .error
                        .unwrap_or_else(|| "the session refused the verb".into());
                    self.push_text(&format!("kelir: {message}"));
                    self.note = crate::util::one_line_visible(&message, 60);
                }
            }
            Live::Herdr { state } => self.status_records.push(herdr::record(state)),
            Live::Quit => self.quit = true,
        }
    }

    pub(super) fn apply_log(&mut self, event: &crate::log::event::LogEvent) {
        use crate::log::event::EventBody;
        let now = Instant::now();
        match &event.body {
            EventBody::SessionStarted {
                provider, model, ..
            } => {
                self.status.provider = provider.clone();
                self.status.model = model.clone();
            }
            EventBody::User { text, .. } => {
                let text = payload_text(text);
                if let Some(index) = self.own_user_echoes.iter().position(|echo| echo == &text) {
                    self.own_user_echoes.remove(index);
                } else if !text.is_empty() {
                    self.push_user_row(&text);
                }
            }
            EventBody::StepStarted {
                step,
                provider,
                model,
                ..
            } => {
                self.status.step = *step;
                self.status.provider = provider.clone();
                self.status.model = model.clone();
                self.status.turn_steps += 1;
                self.turn_steps = self.status.turn_steps;
                if let Some(index) = self.summary_entry.take()
                    && index < self.entries.len()
                {
                    self.entries.remove(index);
                    self.rewrap();
                }
                self.status.phase = Phase::Working;
                self.activity.step_started(now);
                self.retry_deadline = None;
            }
            EventBody::Assistant { text, usage, .. } => {
                let streamed = self.stream.finish().unwrap_or_default();
                let text = payload_text(text);
                let text = if streamed.trim().is_empty() {
                    text
                } else {
                    streamed
                };
                if !text.trim().is_empty() {
                    self.emit_markdown(&text);
                }
                self.stream = Stream::new();
                if let Some(usage) = usage {
                    self.add_usage(usage);
                }
            }
            EventBody::StepUsage(usage) => self.add_usage(usage),
            EventBody::CallStarted(start) if start.kind == "tool" => {
                let streamed = self.stream.finish().unwrap_or_default();
                if !streamed.trim().is_empty() {
                    self.emit_markdown(&streamed);
                }
                let input = start
                    .info
                    .input
                    .as_ref()
                    .map(payload_value)
                    .unwrap_or(serde_json::Value::Null);
                self.call_starts
                    .insert(start.id.clone(), (start.info.name.clone(), input.clone()));
                self.status.running += 1;
                self.activity
                    .tool_started(now, &view::call_label(&start.info.name, &input));
            }
            EventBody::CallSettled(settle) => {
                let (name, input) = self
                    .call_starts
                    .remove(&settle.id)
                    .unwrap_or_else(|| (settle.kind.clone(), serde_json::Value::Null));
                self.status.running = self.status.running.saturating_sub(1);
                self.activity.tool_settled(now);
                self.call_ordinal += 1;
                let tail = settle.tail.clone().unwrap_or_else(|| {
                    settle
                        .result
                        .as_ref()
                        .map(|value| crate::tui::last_line(&payload_text(value)))
                        .unwrap_or_default()
                });
                let call = CallLine {
                    ordinal: self.call_ordinal,
                    name: name.clone(),
                    head: view::call_head(&name, &input),
                    outcome: settle.outcome.clone(),
                    exit_code: settle.exit_code,
                    bytes: settle.bytes.unwrap_or_default(),
                    lines: settle.lines.unwrap_or_default(),
                    spilled: settle.spilled.unwrap_or_default(),
                    out_path: settle.out_path.clone().unwrap_or_default(),
                    tail,
                    elapsed: None,
                };
                self.last_out = Some(call.out_path.clone()).filter(|path| !path.is_empty());
                self.calls.insert(call.ordinal, call.clone());
                self.append(Entry::Call(call));
            }
            EventBody::TurnEnded {
                outcome,
                final_text,
                ..
            } => {
                let streamed = self.stream.finish().unwrap_or_default();
                if !streamed.trim().is_empty() {
                    self.emit_markdown(&streamed);
                }
                self.stream = Stream::new();
                self.activity.turn_ended(now, outcome);
                let final_text = payload_text(final_text);
                if outcome == "paused"
                    && let Some(reason) = view::pause_reason(&final_text)
                {
                    self.activity.paused(now, &reason);
                }
                self.status.phase = match outcome.as_str() {
                    "done" => Phase::Done,
                    "paused" => Phase::Paused,
                    "error" => Phase::Error,
                    _ => Phase::Idle,
                };
                self.status.running = 0;
                self.status.retrying = None;
                self.note.clear();
                self.turn_summary(outcome);
            }
            EventBody::SessionTitle { title } => self.set_title(title.clone()),
            EventBody::Error { message, .. } => {
                self.status.phase = Phase::Error;
                self.activity
                    .failed(now, &crate::util::one_line_visible(message, 60));
                self.append(Entry::Error(format!("kelir: {message}")));
            }
            EventBody::Note(note) => {
                let text = if note.text.contains("say stop") {
                    view::tui_wait_line(&note.text)
                } else {
                    note.text.clone()
                };
                self.push_text(&text);
            }
            EventBody::Source {
                kind,
                name,
                text,
                path,
                ..
            } => {
                // Where the text came from, not the text: a skill's `SKILL.md`
                // is a kilobyte of prose and it is now in the context, which
                // is the fact worth showing. A short body with no file (an
                // inline `text`) is still worth its line.
                let detail = match (path, text) {
                    (Some(path), _) => path.clone(),
                    (None, Some(text)) => {
                        let body = payload_text(text);
                        // A spilled body arrives as a preview: its newlines and
                        // its length are the tell, and its first line is not
                        // the fact — "in context" is.
                        if body.contains('\n') || body.len() >= crate::log::event::PREVIEW_BYTES {
                            "in context".to_string()
                        } else {
                            body
                        }
                    }
                    (None, None) => "in context".to_string(),
                };
                self.push_text(&format!("· {kind} {name}: {detail}"));
            }
            EventBody::Verb {
                name,
                from,
                boundary,
                error,
                ..
            } => {
                if boundary == "rejected" {
                    let message = error.clone().unwrap_or_else(|| format!("{name} refused"));
                    self.status.phase = Phase::Error;
                    self.activity
                        .failed(now, &crate::util::one_line_visible(&message, 60));
                    self.append(Entry::Error(format!("kelir: {message}")));
                } else if name != "status" && name != "observe" {
                    self.push_text(&format!("» {name} ({from}) · {boundary}"));
                }
            }
            EventBody::SessionEnded { reason } => {
                self.push_text(&format!("session ended: {reason}"));
            }
            _ => {}
        }
    }
}
