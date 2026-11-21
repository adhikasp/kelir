//! What a named or job run adds to a session's log (M9): the date-change source
//! and the plan's own sources. Kept apart from `session.rs` so the loop has two
//! calls to these and nothing else.

use super::*;

impl Session {
    /// The date the session's frozen system prompt was rendered with (the
    /// `date:` line in its Environment block). The event's `ts` is wall-clock
    /// time; with a `KELIR_TODAY` override the two can disagree, and the model
    /// only knows what the prompt said (STEER-9).
    fn rendered_date(&self) -> Option<String> {
        let text = self.events.iter().find_map(|event| {
            if let EventBody::SessionStarted { system, .. } = &event.body {
                system.text.resolve_text(&self.dir).ok()
            } else {
                None
            }
        })?;
        text.lines()
            .find_map(|line| line.strip_prefix("date: "))
            .map(str::trim)
            .filter(|date| {
                date.len() == 10 && date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-'
            })
            .map(str::to_string)
    }

    /// Record the sources a run owes the context before its task: on a resume,
    /// the new date if the day changed (the frozen system prompt keeps the old
    /// one, the provider's cache lives in it), then the plan's sources (a job's
    /// one status line, a carried summary). Everything is appended at the tail.
    pub(super) fn record_plan_sources(&mut self, resumed: bool) -> Result<(), String> {
        let mut sources: Vec<crate::jobs::PlanSource> = Vec::new();
        // A child or a brief reads its parent's log; the date is the parent's
        // business.
        if resumed
            && self.kids.link.is_none()
            && let Some(clock) = crate::jobs::clock_source_from(
                &self.events,
                &today(),
                self.rendered_date().as_deref(),
            )
        {
            sources.push(clock);
        }
        if let Some(plan) = crate::jobs::plan() {
            sources.extend(plan.sources.iter().cloned());
        }
        for source in sources {
            self.record(EventBody::Source {
                kind: source.kind,
                name: source.name,
                text: Some(Payload::text(source.text)),
                path: None,
            })?;
        }
        Ok(())
    }
}
