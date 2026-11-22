//! The boundary a feature uses to reach the core. The core knows no feature by name:
//! features are dispatched through [`Feature`] in the fixed order documented on
//! `Features` (crate::features::Features).
//!
//! Only the core implements `Feature`; a feature never holds the log writer. Every append
//! goes through `Cx::record`, the core's single writer.

use std::path::PathBuf;

use serde_json::Value;
use tokio::sync::mpsc;

use crate::event::{EventBody, LogEvent};

/// The narrow view of the core a feature may use. Features never hold the log writer,
/// the inbox, or the UI sink: they go through this context. Fields not yet used by
/// an extracted feature are kept here so every later move reuses the same boundary.
#[allow(dead_code)]
pub struct Cx<'a> {
    pub(crate) writer: &'a mut crate::event::EventLog,
    pub(crate) blobs: &'a mut crate::event::Blobs,
    pub(crate) events: &'a mut Vec<LogEvent>,
    pub(crate) ui: &'a Option<mpsc::UnboundedSender<crate::tui::View>>,
    pub(crate) cfg: &'a crate::config::Config,
    pub(crate) dir: &'a PathBuf,
    pub(crate) id: &'a str,
    pub(crate) model: &'a mut String,
    pub(crate) provider_name: &'a mut String,
    pub(crate) client: &'a mut crate::provider::Client,
    pub(crate) active_provider: &'a mut crate::config::ProviderConfig,
    pub(crate) governor: &'a mut Option<crate::governor::Governor>,
    pub(crate) ctx: &'a crate::tools::Ctx,
    pub(crate) tally: &'a mut crate::usage::Tally,
    pub(crate) json: bool,
    pub(crate) tui: bool,
}

#[allow(dead_code)]
impl<'a> Cx<'a> {
    /// Append one feature-owned event through the core's single writer. Oversized
    /// payloads go to `blobs/` first, named by the seq the event is about to get.
    /// The event is also pushed into `cx.events`, so a feature that reads the fold
    /// sees what it just wrote. Core-owned side effects (naming the session from
    /// the first human prompt) stay in `Session::record`: features must not record
    /// `EventBody::User` events.
    pub(crate) fn record(&mut self, body: EventBody) -> Result<LogEvent, String> {
        let seq = self.writer.next_seq();
        let body = crate::event::spill_payloads(body, self.blobs, seq)?;
        let event = self
            .writer
            .append(body)
            .map_err(|error| format!("cannot append to the event log: {error}"))?;
        if self.json {
            use std::io::Write as _;
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let _ = writeln!(
                lock,
                "{}",
                serde_json::to_string(&event).unwrap_or_default()
            );
            let _ = lock.flush();
        }
        self.tally.observe(event.seq, &event.body);
        self.events.push(event.clone());
        Ok(event)
    }

    /// Send an informational UI event; a dead UI never fails a turn. Retry events
    /// carry core state (`retry_wait`) and must go through `Session::emit`, not here.
    pub(crate) fn emit(&self, event: crate::tui::View) {
        if let Some(ui) = self.ui {
            let _ = ui.send(event);
        }
    }

    /// Flush the log for a client that just got an answer.
    pub(crate) fn sync(&mut self) {
        let _ = self.writer.sync();
    }
}

/// What a hook wants the core to do next. `Continue` preserves the current loop.
pub enum Flow {
    Continue,
}

/// A feature at the loop's named boundaries. Static dispatch: no async-trait, no
/// boxed futures. Hooks are the only way a feature can affect the core.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot plug into the core: it must implement `Feature`",
    note = "only `core` implements the feature boundaries; a feature owns its own state"
)]
pub trait Feature: Send {
    fn name(&self) -> &'static str;

    /// Session start/reopen; may add events (e.g. gate notes).
    fn on_start(&mut self, _cx: &mut Cx<'_>) -> Result<(), String> {
        Ok(())
    }

    /// Before each step: apply queued work (model fetch result, source changes, mcp results).
    async fn before_step(&mut self, cx: &mut Cx<'_>) {
        let _ = cx;
    }

    /// Check on-disk sources at the point after compaction where the core used
    /// to call `Session::check_sources`. A feature without sources does nothing.
    fn check_sources(&mut self, _cx: &mut Cx<'_>) -> Result<(), String> {
        Ok(())
    }

    /// A verb this feature owns; `None` = not mine.
    fn on_verb(&mut self, _cx: &mut Cx<'_>, _name: &str, _args: &mut Value) -> Option<Value> {
        None
    }

    /// Contribute to the `status` answer.
    fn status(&self, _out: &mut serde_json::Map<String, Value>) {}
}

/// The order features run at every boundary. Side effects at a boundary are observable
/// in the log, so this order is fixed (today's code order):
/// models → sources → mcp → gates → bg → children → attachments → usage.
pub struct NoFeature;
