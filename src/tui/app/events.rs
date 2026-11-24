//! App methods: events.
use crate::tui::app::*;

impl App {
    pub(super) fn add_usage(&mut self, usage: &crate::log::event::Usage) {
        self.status.prompt_tokens += usage.prompt_tokens;
        self.status.last_prompt_tokens = usage.prompt_tokens;
        self.status.completion_tokens += usage.completion_tokens;
        self.status.cached_tokens += usage.cached_tokens;
        self.turn_tokens += usage.prompt_tokens + usage.completion_tokens;
    }

    /// The one line for turns that need a human: nothing after a `done` turn.
    pub(super) fn turn_summary(&mut self, outcome: &str) {
        if outcome != "done" {
            self.separate();
            let steps = self.turn_steps.max(self.status.turn_steps);
            let summary = activity::summary_line(self.theme, outcome, steps, self.turn_tokens);
            let text: String = summary
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            self.append(Entry::Dim(text));
            self.summary_entry = Some(self.entries.len().saturating_sub(1));
        }
        self.turn_steps = 0;
        self.turn_tokens = 0;
    }

    pub(super) fn clear_draft(&mut self) {
        if let Some(index) = self.draft_entry.take()
            && index < self.entries.len()
        {
            let old_draft = std::mem::take(&mut self.draft);
            let rows = view::entry_lines(self, &Entry::Markdown(old_draft), self.width).len();
            self.entries.remove(index);
            let keep = self.cached_rows.len().saturating_sub(rows);
            self.cached_rows.truncate(keep);
        }
        self.draft.clear();
    }

    pub(super) fn set_draft(&mut self, text: String) {
        if self.draft == text && self.draft_entry.is_some() {
            return;
        }
        self.clear_draft();
        if !text.trim().is_empty() {
            self.separate();
            self.draft_entry = Some(self.entries.len());
            self.entries.push(Entry::Markdown(text.clone()));
            let rows =
                view::entry_lines(self, &self.entries[self.draft_entry.unwrap()], self.width);
            self.cached_rows.extend(rows);
        }
        self.draft = text;
    }
}
