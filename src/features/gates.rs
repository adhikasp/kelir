//! The gates feature: builds the session's gate runner from config and skills, runs
//! gates at their events, and logs gate runs, notes, usage and context through the
//! core's single writer. The core keeps no gate by name.
//!
//! `GatesCore` is the narrow view of the core this feature needs. The session implements
//! it; the feature never holds the event writer.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::core::boundary::Feature;
use crate::event::{EventBody, LogEvent, Payload};
use crate::tui::UiEvent;

/// The core operations the gates feature needs. `Session` implements this.
pub(crate) trait GatesCore {
    fn record(&mut self, body: EventBody) -> Result<LogEvent, String>;
    fn sync(&mut self);
    fn emit(&self, event: UiEvent);
    fn turn(&self) -> u32;
    fn tui(&self) -> bool;
    fn task_from(&self) -> &'static str;
    fn gate_ctx(&self) -> crate::gates::GateCtx<'_>;
    fn gates_config(&self) -> GatesConfig<'_>;
    fn events(&self) -> &[LogEvent];
}

/// What the feature reads from the core to build the gate runner once at start.
pub(crate) struct GatesConfig<'a> {
    pub(crate) gates: &'a [crate::gates::GateDef],
    pub(crate) project_gates: &'a [crate::gates::GateDef],
    pub(crate) skills_dirs: &'a [std::path::PathBuf],
    pub(crate) trusted_projects: &'a [std::path::PathBuf],
    pub(crate) config_dir: Option<&'a std::path::Path>,
    pub(crate) cwd: &'a std::path::Path,
    pub(crate) shell: &'a [String],
    pub(crate) id: &'a str,
    pub(crate) dir: &'a std::path::Path,
}

/// The gates feature's state, moved out of `Session`.
pub(crate) struct Gates {
    runner: Option<Arc<crate::gates::Runner>>,
    /// A `turn_end` gate already blocked this turn: it blocks at most once.
    turn_blocked: bool,
    /// Tool calls the turn has started, given to `turn_end` gates.
    turn_tool_calls: u32,
    /// The command-line task, held back until `session_start` gates ran and
    /// `prompt_submit` gates saw it: (text, delivery, from).
    initial_task: Option<(String, &'static str, &'static str)>,
    /// Problems found while reading gate declarations; logged once at start.
    notes: Vec<String>,
    /// Context contributed before the user message it belongs to was recorded.
    pending_context: Vec<crate::gates::Context>,
}

impl Gates {
    pub(crate) fn new() -> Self {
        Self {
            runner: None,
            turn_blocked: false,
            turn_tool_calls: 0,
            initial_task: None,
            notes: Vec::new(),
            pending_context: Vec::new(),
        }
    }

    /// Gates from `[gates]` and from skills bound with `kelir-gate`; `None`
    /// when there are none (the default), so nothing below costs a thing.
    pub(crate) fn build<C: GatesCore>(&mut self, core: &C) {
        // A kelir that a gate started runs no gates: a gate never triggers gates.
        if crate::gates::started_by_a_gate(crate::envs::var("KELIR_GATE_EVENT").as_deref()) {
            self.runner = None;
            return;
        }
        let cfg = core.gates_config();
        let mut defs = cfg.gates.to_vec();
        for def in cfg.project_gates.to_vec() {
            crate::gates::push_unique(&mut defs, def);
        }
        let places =
            crate::sources::places(cfg.cwd, cfg.skills_dirs, cfg.config_dir.map(PathBuf::from));
        let found = crate::sources::discover(&places);
        let mut notes = Vec::new();
        for def in crate::gates::from_skills(&found, &mut notes) {
            crate::gates::push_unique(&mut defs, def);
        }
        let trusted = crate::gates::project_trusted(cfg.cwd, cfg.trusted_projects);
        let defs = crate::gates::apply_trust(defs, trusted, &mut notes);
        self.notes = notes;
        if defs.is_empty() {
            self.runner = None;
            return;
        }
        self.runner = Some(Arc::new(crate::gates::Runner::new(
            defs,
            cfg.shell.to_vec(),
            cfg.cwd.to_path_buf(),
            cfg.id.to_string(),
            cfg.dir.to_path_buf(),
        )));
    }

    /// Whether any gate would run at `event` for `tool`.
    pub(crate) fn gated(&self, event: crate::gates::Event, tool: Option<&str>) -> bool {
        self.runner
            .as_ref()
            .is_some_and(|gates| gates.has(event, tool))
    }

    pub(crate) fn runner(&self) -> Option<Arc<crate::gates::Runner>> {
        self.runner.clone()
    }

    pub(crate) fn turn_blocked(&self) -> bool {
        self.turn_blocked
    }

    pub(crate) fn set_turn_blocked(&mut self, blocked: bool) {
        self.turn_blocked = blocked;
    }

    pub(crate) fn turn_tool_calls(&self) -> u32 {
        self.turn_tool_calls
    }

    pub(crate) fn add_turn_tool_calls(&mut self, n: u32) {
        self.turn_tool_calls += n;
    }

    pub(crate) fn reset_turn(&mut self) {
        self.turn_blocked = false;
        self.turn_tool_calls = 0;
    }

    /// Log what the gate runner ran, one `gate` event each.
    fn record_runs<C: GatesCore>(
        &mut self,
        core: &mut C,
        runs: &[crate::gates::Run],
    ) -> Result<(), String> {
        for run in runs {
            core.record(EventBody::Gate {
                gate: run.gate.clone(),
                event: run.event.clone(),
                decision: run.decision.clone(),
                reason: run.reason.clone(),
                ms: run.ms,
                rewritten: run.rewritten,
                error: run.error.clone(),
            })?;
        }
        Ok(())
    }

    /// Run the gates of one event and log every run. A session without gates
    /// answers `Allow` without looking at the payload.
    pub(crate) async fn gate<C: GatesCore>(
        &mut self,
        core: &mut C,
        event: crate::gates::Event,
        tool: Option<&str>,
        payload: Value,
    ) -> Result<crate::gates::Verdict, String> {
        let Some(gates) = self.runner.clone() else {
            return Ok(crate::gates::Verdict::Allow);
        };
        if !gates.has(event, tool) {
            return Ok(crate::gates::Verdict::Allow);
        }
        let chain = gates
            .run_event(event, tool, core.turn(), payload, Some(&core.gate_ctx()))
            .await;
        self.record_runs(core, &chain.runs)?;
        for note in gates.take_notes() {
            let _ = core.record(EventBody::Error {
                location: "gate".into(),
                message: note,
                retryable: false,
                status: None,
                attempt: None,
                retry_in_ms: None,
            });
        }
        for usage in &chain.usages {
            let _ = core.record(EventBody::StepUsage(crate::event::Usage {
                prompt_tokens: usage.1.prompt_tokens,
                completion_tokens: usage.1.completion_tokens,
                cached_tokens: usage.1.cached_tokens,
                cache_write_tokens: usage.1.cache_write_tokens,
                allowance: None,
                kind: Some(usage.0.clone()),
            }));
        }
        if event == crate::gates::Event::PromptSubmit {
            self.pending_context.extend(chain.contexts);
        } else {
            self.record_context(core, &chain.contexts);
        }
        core.sync();
        Ok(chain.verdict)
    }

    /// The one place a gate's `context` becomes a `source` event, shared by
    /// external and built-in gates.
    fn record_context<C: GatesCore>(&mut self, core: &mut C, contexts: &[crate::gates::Context]) {
        for context in contexts {
            let _ = core.record(EventBody::Source {
                kind: context.kind.clone(),
                name: context.kind.clone(),
                text: Some(Payload::text(context.text.clone())),
                path: None,
            });
        }
    }

    /// Append contexts held back until the user message they belong to exists.
    pub(crate) fn flush_context<C: GatesCore>(&mut self, core: &mut C) {
        let pending = std::mem::take(&mut self.pending_context);
        self.record_context(core, &pending);
        core.sync();
    }

    /// Problems found reading gate declarations: logged once, never fatal.
    pub(crate) fn flush_notes<C: GatesCore>(&mut self, core: &mut C) {
        for note in std::mem::take(&mut self.notes) {
            let _ = core.record(EventBody::Error {
                location: "gate".into(),
                message: note,
                retryable: false,
                status: None,
                attempt: None,
                retry_in_ms: None,
            });
        }
    }

    /// The command-line task: written at once, or held back for the gates
    /// (`session_start`, then `prompt_submit`) when there are any.
    pub(crate) fn submit_task<C: GatesCore>(
        &mut self,
        core: &mut C,
        text: String,
        delivery: &'static str,
    ) -> Result<(), String> {
        if self.gated(crate::gates::Event::PromptSubmit, None) {
            self.initial_task = Some((text, delivery, core.task_from()));
            return Ok(());
        }
        core.record(EventBody::User {
            text: text.into(),
            from: core.task_from().into(),
            delivery: delivery.to_string(),
            attachments: Vec::new(),
        })?;
        Ok(())
    }

    /// A human prompt through the `prompt_submit` gates: the text to record,
    /// or `None` when a gate dropped it (the person is told why).
    pub(crate) async fn prompt<C: GatesCore>(
        &mut self,
        core: &mut C,
        text: String,
        from: &str,
    ) -> Result<Option<String>, String> {
        if matches!(from, "job" | "gate" | "parent" | "brief" | "child")
            || !self.gated(crate::gates::Event::PromptSubmit, None)
        {
            return Ok(Some(text));
        }
        let payload = json!({"text": text, "from": from});
        match self
            .gate(core, crate::gates::Event::PromptSubmit, None, payload)
            .await?
        {
            crate::gates::Verdict::Allow => Ok(Some(text)),
            crate::gates::Verdict::Rewrite(value) => Ok(Some(
                value
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or(&text)
                    .to_string(),
            )),
            crate::gates::Verdict::Deny { gate, reason } => {
                let message = format!(
                    "prompt dropped by gate {gate}{}",
                    if reason.is_empty() {
                        String::new()
                    } else {
                        format!(": {reason}")
                    }
                );
                // stderr even under `--json` (stdout stays NDJSON), like a pause.
                if !core.tui() {
                    eprintln!("kelir: {message}");
                }
                core.emit(UiEvent::Info(format!("» {message}")));
                Ok(None)
            }
        }
    }

    /// `session_start` gates, then the held-back command-line task through
    /// `prompt_submit`. `false` means a gate dropped the task.
    pub(crate) async fn start<C: GatesCore>(
        &mut self,
        core: &mut C,
        provider: &str,
        model: &str,
        resumed: bool,
    ) -> bool {
        self.flush_notes(core);
        let payload = json!({
            "provider": provider,
            "model": model,
            "resumed": resumed,
        });
        let _ = self
            .gate(core, crate::gates::Event::SessionStart, None, payload)
            .await;
        let Some((text, delivery, from)) = self.initial_task.take() else {
            return true;
        };
        match self.prompt(core, text, from).await {
            Ok(Some(text)) => {
                let _ = core.record(EventBody::User {
                    text: text.into(),
                    from: from.into(),
                    delivery: delivery.to_string(),
                    attachments: Vec::new(),
                });
                self.flush_context(core);
                true
            }
            _ => false,
        }
    }

    /// `session_end` gates (side effects only), just before `session_ended`.
    pub(crate) async fn end<C: GatesCore>(&mut self, core: &mut C, reason: &str) {
        let _ = self
            .gate(
                core,
                crate::gates::Event::SessionEnd,
                None,
                json!({"reason": reason}),
            )
            .await;
    }

    /// The `status` verb's gate fields, folded from the log.
    pub(crate) fn status(&self, core: &dyn GatesCore) -> Value {
        let mut runs = 0u64;
        let mut denied = 0u64;
        let mut rewritten = 0u64;
        let mut failed_open = 0u64;
        let mut last = Value::Null;
        for event in core.events() {
            if let EventBody::Gate {
                gate,
                event: at,
                decision,
                rewritten: rewrote,
                error,
                ..
            } = &event.body
            {
                runs += 1;
                denied += u64::from(decision == "deny");
                rewritten += u64::from(*rewrote);
                failed_open += u64::from(error.is_some());
                last = json!({"gate": gate, "event": at, "decision": decision});
            }
        }
        json!({
            "declared": self.runner.as_ref().map_or(0, |g| g.defs().len()),
            "runs": runs,
            "denied": denied,
            "rewritten": rewritten,
            "failed_open": failed_open,
            "last": last,
        })
    }
}

impl Feature for Gates {
    fn name(&self) -> &'static str {
        "gates"
    }
}

pub(crate) fn apply_turn_verdict(
    session: &mut crate::core::session::types::Session,
    verdict: crate::gates::Verdict,
    blocked: bool,
) -> Result<bool, String> {
    use crate::event::EventBody;
    match verdict {
        crate::gates::Verdict::Deny { gate, reason } if !blocked => {
            session.gates_do(|gates, _| gates.set_turn_blocked(true));
            let reason = if reason.trim().is_empty() {
                "no reason given".to_string()
            } else {
                reason
            };
            session.record(EventBody::User {
                text: format!("[gate {gate}] Your answer was not accepted: {reason}").into(),
                from: "gate".into(),
                delivery: "next".into(),
                attachments: Vec::new(),
            })?;
            session.sync();
            Ok(true)
        }
        crate::gates::Verdict::Deny { gate, .. } => {
            // Blocks at most once per turn: a second deny is logged, not enforced.
            let _ = session.record(EventBody::Error {
                location: "gate".into(),
                message: format!(
                    "turn_end: the deny from gate {gate} was ignored (this turn was already blocked once)"
                ),
                retryable: false,
                status: None,
                attempt: None,
                retry_in_ms: None,
            });
            Ok(false)
        }
        crate::gates::Verdict::Rewrite(value) => {
            if let Some(new) = value.get("text").and_then(serde_json::Value::as_str) {
                session.run.final_text = new.to_string();
            }
            Ok(false)
        }
        crate::gates::Verdict::Allow => Ok(false),
    }
}

/// Apply a `before_tool` verdict: returns the (possibly rewritten) call input
/// and, when the gate denied it, the reason to record on the outcome.
pub(crate) async fn apply_tool_verdict(
    session: &mut crate::core::session::types::Session,
    name: &str,
    call_id: &str,
    input: serde_json::Value,
) -> Result<(serde_json::Value, Option<(String, String)>), String> {
    let payload = serde_json::json!({
        "tool": name,
        "call_id": call_id,
        "input": input,
    });
    match session
        .gates_call(crate::gates::Event::BeforeTool, Some(name), payload)
        .await?
    {
        crate::gates::Verdict::Deny { gate, reason } => Ok((input, Some((gate, reason)))),
        crate::gates::Verdict::Rewrite(args) => Ok((args, None)),
        crate::gates::Verdict::Allow => Ok((input, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_gate_feature_has_no_runner_and_a_stable_name() {
        let gates = Gates::new();
        assert!(!gates.gated(crate::gates::Event::TurnEnd, None));
        assert!(gates.runner().is_none());
        assert_eq!(Feature::name(&gates), "gates");
    }

    #[test]
    fn turn_counters_are_scoped_to_the_turn() {
        let mut gates = Gates::new();
        gates.add_turn_tool_calls(3);
        gates.set_turn_blocked(true);
        assert_eq!(gates.turn_tool_calls(), 3);
        assert!(gates.turn_blocked());
        gates.reset_turn();
        assert_eq!(gates.turn_tool_calls(), 0);
        assert!(!gates.turn_blocked());
    }
}
