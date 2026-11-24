//! The compaction half of the session loop.
// These private loop modules expose crate-visible helpers to the feature modules.
#![allow(clippy::redundant_pub_crate)]

use super::types::*;
use super::*;

impl Session {
    /// Compact the context if something asked for it: the `compact` tool, the `compact` verb, or
    /// the auto trigger at the configured fraction of the model's window.
    pub(crate) async fn maybe_compact(
        &mut self,
        rx: &mut Inbox,
    ) -> Result<Option<TurnEnd>, String> {
        if self.compact.request.is_none() {
            // Nothing to compact before the first step: the estimate would be little more than the
            // system prompt, and the "conversation" is the task alone.
            let has_history = self
                .log
                .events
                .iter()
                .any(|event| matches!(event.body, EventBody::StepStarted { .. }));
            if has_history && compact::plan(&self.log.events, self.cfg.compact_keep).is_some() {
                let folded = crate::log::fold::fold(&self.log.events, &self.log.dir)?;
                let estimate = compact::estimate_tokens(&folded.request_messages());
                // The provider's own count is the honest size; the estimate
                // covers the step after a compaction.
                let tokens = self.compact.last_prompt_tokens.unwrap_or(estimate);
                let window = self.features.models.window;
                // After a compaction that did not shrink anything, wait for the context to grow past it
                // instead of paying for a summary on every step.
                let above_floor = self
                    .compact
                    .floor
                    .map(|floor| tokens > floor)
                    .unwrap_or(true);
                if above_floor
                    && compact::should_auto(
                        window,
                        self.cfg.auto_compact_at,
                        self.cfg.compact_at_tokens,
                        tokens,
                    )
                {
                    self.compact.request = Some(CompactRequest {
                        by: "auto",
                        focus: None,
                        immediate: false,
                    });
                }
            }
        }
        let Some(request) = self.compact.request.take() else {
            return Ok(None);
        };
        self.run_compact(rx, &request).await
    }

    /// Run a requested compaction and report what it did.
    pub(crate) async fn run_compact(
        &mut self,
        rx: &mut Inbox,
        request: &CompactRequest,
    ) -> Result<Option<TurnEnd>, String> {
        match self.compact(rx, request).await {
            Ok(CompactRun::Applied(info)) => {
                self.ui_note(format!(
                    "» compacted through seq {}: ~{} → ~{} tokens ({} pinned) · cache restarts",
                    info.through_seq, info.tokens_before, info.tokens_after, info.pinned
                ));
                if info.tokens_after >= info.tokens_before {
                    self.ui_note(format!(
                        "» compact: it did not shrink the context (~{} → ~{} tokens); the event \
                         records both",
                        info.tokens_before, info.tokens_after
                    ));
                }
            }
            Ok(CompactRun::Nothing) => {
                self.ui_note("» compact: nothing old enough to summarise".to_string());
            }
            Ok(CompactRun::Skipped { before, would_be }) => {
                // A summary that cannot shrink the context buys nothing, so it is not requested
                // again until the context grows.
                let first_refusal = self.compact.floor.is_none();
                self.compact.floor = Some(self.compact.last_prompt_tokens.unwrap_or(before));
                let message = format!(
                    "automatic compaction skipped: the summary and pins would not be smaller \
                     than the range they replace (~{before} → ~{would_be} tokens)"
                );
                self.ui_note(format!("» compact skipped: {message}"));
                // One line in the log per episode, not one per step: the
                // exception is the decision, the repeats add nothing.
                if first_refusal {
                    self.record(EventBody::Error {
                        location: "compact".into(),
                        message,
                        retryable: false,
                        status: None,
                        attempt: None,
                        retry_in_ms: None,
                    })?;
                    self.sync();
                }
            }
            Ok(CompactRun::Stopped(reason)) => return Ok(Some(TurnEnd::Stopped(reason))),
            Ok(CompactRun::End) => return Ok(Some(TurnEnd::End)),
            Err(message) => {
                // A failed summary is not a failed turn: the context stays as
                // it was and the next step runs.
                self.record(EventBody::Error {
                    location: "compact".into(),
                    message,
                    retryable: true,
                    status: None,
                    attempt: None,
                    retry_in_ms: None,
                })?;
                self.sync();
            }
        }
        Ok(None)
    }

    /// The size of the request the fold would build with `preview` appended.
    pub(crate) fn preview_tokens(&self, preview: &EventBody, seq: u64) -> Result<u64, String> {
        let mut events = self.log.events.clone();
        events.push(LogEvent::new(seq, preview.clone()));
        let folded = crate::log::fold::fold(&events, &self.log.dir)?;
        Ok(compact::estimate_tokens(&folded.request_messages()))
    }

    /// Summarise the range and write the `compacted` event.
    pub(crate) async fn compact(
        &mut self,
        rx: &mut Inbox,
        request: &CompactRequest,
    ) -> Result<CompactRun, String> {
        let Some(plan) = compact::plan(&self.log.events, self.cfg.compact_keep) else {
            return Ok(CompactRun::Nothing);
        };
        // The transcript is the fold of the range, not the raw events: a compaction that covers an
        // earlier one summarises its summary instead of losing it .
        let range = crate::log::fold::range_events(&self.log.events, plan.through_seq);
        let folded_range = crate::log::fold::fold(&range, &self.log.dir)?;
        let transcript = compact::render_transcript(&folded_range.messages);
        // Both numbers describe the whole request the provider sees, so asking
        // "did it shrink?" is a real question about the context.
        let whole = crate::log::fold::fold(&self.log.events, &self.log.dir)?;
        let tokens_before = compact::estimate_tokens(&whole.request_messages());
        let mut body = EventBody::Compacted {
            through_seq: plan.through_seq,
            summary: Payload::text(""),
            pinned_seqs: plan.pinned_seqs.clone(),
            by: request.by.to_string(),
            tokens_before,
            tokens_after: 0,
            cache_restart: true,
        };
        // The smallest possible result: an empty summary. If even that is not smaller, no summary
        // can be, so an automatic compaction declines before paying for one.
        let smallest = self.preview_tokens(&body, self.log.writer.next_seq())?;
        if request.by == "auto" && smallest >= tokens_before {
            return Ok(CompactRun::Skipped {
                before: tokens_before,
                would_be: smallest,
            });
        }

        self.ui_note(format!(
            "» compacting {} pinned message(s) through seq {} · ~{} tokens",
            plan.pinned_seqs.len(),
            plan.through_seq,
            tokens_before
        ));
        let summarising = self.capabilities(compact::summarise_request(
            &self.provider.client,
            &self.provider.model,
            &self.cfg.compact_prompt,
            &transcript,
            request.focus.as_deref(),
        ));
        let assembler = match self
            .stream(rx, RequestFor::Built(&summarising), true)
            .await?
        {
            StreamEnd::Assembled(assembler) => assembler,
            StreamEnd::Stopped(reason) => return Ok(CompactRun::Stopped(reason)),
            StreamEnd::End => return Ok(CompactRun::End),
            // A summary is tried once and has no budget to use, so it can never pause; if that ever
            // changes, a paused summary is a summary that did not happen.
            StreamEnd::Paused(_) => return Ok(CompactRun::End),
        };
        // The summary's tokens belong in the log like any step's, marked so a
        // reader (and `kelir ls`) can tell it is not a step of the turn
        if let Some(usage) = &assembler.usage {
            self.record(EventBody::StepUsage(crate::log::event::Usage {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                cached_tokens: usage.cached_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                allowance: usage.allowance.clone(),
                kind: Some("compact".into()),
            }))?;
            self.note_warnings();
            self.sync();
        }
        let summary = assembler.text.trim().to_string();
        if summary.is_empty() {
            return Err("the summarising request returned no text".to_string());
        }
        if let EventBody::Compacted { summary: slot, .. } = &mut body {
            *slot = Payload::text(summary);
        }
        // The size after: fold the log as it *will* be, so the number in the
        // event is the one the next request is built from.
        let tokens_after = self.preview_tokens(&body, self.log.writer.next_seq())?;
        if request.by == "auto" && tokens_after >= tokens_before {
            return Ok(CompactRun::Skipped {
                before: tokens_before,
                would_be: tokens_after,
            });
        }
        if let EventBody::Compacted {
            tokens_after: slot, ..
        } = &mut body
        {
            *slot = tokens_after;
        }
        let pinned = plan.pinned_seqs.len();
        // A `source` message folded into the summary is no longer in the context: the watcher will say what
        // the file holds at the next boundary, even when its bytes did not change.
        self.features
            .sources
            .forget_folded(&self.log.events, plan.through_seq, &plan.pinned_seqs);
        self.record(body)?;
        self.sync();
        // The next trigger measures the compacted fold, not the old size.
        self.compact.last_prompt_tokens = None;
        self.compact.floor = None;
        Ok(CompactRun::Applied(CompactedInfo {
            through_seq: plan.through_seq,
            tokens_before,
            tokens_after,
            pinned,
        }))
    }
}
