//! `kelir observe <session>`: what the model will see on its next request (M5-B).
//!
//! Read-only and offline: it reads the session's `events.jsonl` (a live session
//! writes it as it goes, so the answer is current to the last event), folds it
//! with the same [`crate::fold::fold`] the loop uses, and builds the request
//! with the same [`crate::provider::build_request`] and the same two capability
//! decisions (`parallel_tool_calls`, `reasoning_effort`) the loop applies. That
//! shared path is the point: `--raw` prints the body that would be sent, and the
//! test that matters compares it byte for byte with the body a mock server
//! received for the next request.
//!
//! Known limits, stated rather than hidden: a field the provider refused with a
//! 400 earlier in the *live* process (`refused_fields`) is process memory, not
//! log, so an observe of a session that dropped one still shows it; and a
//! `model` swap is read from the last `step_started`, so a swap not yet applied
//! is not shown. It also builds the request with the config and the on-disk
//! models cache as they are *now* (`cache_ttl`, the provider table, vision
//! support), not the running session's: edit the config after a session starts
//! and `--raw` can differ from that session's next wire body (m5bfix N3). No key is ever read for this: the body does not carry one.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

use crate::config::Config;
use crate::event::{EventBody, LogEvent};

/// Everything `observe` knows, before it is printed.
pub struct Observed {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub body: Value,
    pub url: String,
    pub system: String,
    pub tools: Vec<String>,
    pub messages: Vec<Value>,
    /// `(kind, name)` of the frozen sources, then every `source` message so far.
    pub sources: Vec<(String, String, bool)>,
    /// The newest pushed hint, if the session had one.
    pub hint: Option<String>,
    pub estimate: u64,
    pub steps: u32,
    pub cache: CacheStats,
    /// The effective gate configuration (declarations only, no environment
    /// values) and every gate run the log holds (M8).
    pub gates: Vec<Value>,
    pub fired: Vec<Value>,
}

/// Cache hit ratio from the `usage` events.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CacheStats {
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub last_prompt: u64,
    pub last_cached: u64,
}

impl CacheStats {
    pub fn ratio(prompt: u64, cached: u64) -> Option<f64> {
        (prompt > 0).then(|| cached as f64 / prompt as f64)
    }
}

/// Build the observation of the session in `dir`.
pub fn build(dir: &Path, id: &str, cfg: &Config) -> Result<Observed, String> {
    let path = dir.join("events.jsonl");
    let read = crate::event::read_events(&path)
        .map_err(|error| format!("cannot read {}: {error}", crate::util::path_string(&path)))?;
    let events = read.events;
    let (mut provider, mut model, tools) = events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::SessionStarted {
                provider,
                model,
                tools,
                ..
            } => Some((provider.clone(), model.clone(), tools.clone())),
            _ => None,
        })
        .ok_or_else(|| format!("session {id} has no session_started event"))?;
    let mut steps = 0;
    for event in &events {
        if let EventBody::StepStarted {
            provider: step_provider,
            model: step_model,
            ..
        } = &event.body
        {
            provider = step_provider.clone();
            model = step_model.clone();
            steps += 1;
        }
    }
    let provider_cfg = cfg.provider(&provider).map_err(|error| error.to_string())?;
    let folded = crate::fold::fold(&events, dir)?;
    let messages = folded.request_messages();

    let endpoint = crate::models::endpoint_id(&provider, &provider_cfg.base_url);
    let cache = crate::models::load(&endpoint);
    let list = cache.as_ref().map(|cache| cache.models.as_slice());
    let parallel = crate::models::parallel_calls(list, &model);
    let allowed = crate::models::reasoning_effort_allowed(
        list,
        &model,
        provider_cfg.reasoning_effort_explicit,
    );
    let all = crate::tools::specs();
    let specs: Vec<Value> = tools
        .iter()
        .filter_map(|name| {
            all.iter()
                .find(|spec| spec["function"]["name"].as_str() == Some(name.as_str()))
                .cloned()
        })
        .collect();
    let mut request = crate::provider::build_request(
        &provider_cfg,
        id,
        &model,
        &messages,
        &specs,
        cfg.cache_ttl.as_deref(),
        parallel,
    );
    if !allowed {
        request = request.without_field("reasoning_effort");
    }

    Ok(Observed {
        id: id.to_string(),
        provider,
        model,
        url: request.url.clone(),
        body: request.body.clone(),
        system: folded.system.clone(),
        tools,
        messages: messages.iter().skip(1).map(|m| m.to_wire()).collect(),
        sources: sources_of(&events),
        hint: last_hint(&events, dir),
        estimate: crate::compact::estimate_tokens(&messages),
        steps,
        cache: cache_stats(&events),
        gates: effective_gates(&events, cfg),
        fired: fired_gates(&events),
    })
}

/// The gates a session started with would have: `[gates]` from the config as
/// it is now, plus skills bound with `kelir-gate` found from the session's cwd.
fn effective_gates(events: &[LogEvent], cfg: &Config) -> Vec<Value> {
    let cwd = events.iter().find_map(|event| match &event.body {
        EventBody::SessionStarted { cwd, .. } => Some(std::path::PathBuf::from(cwd)),
        _ => None,
    });
    let mut defs = cfg.gates.clone();
    for def in cfg.project_gates.clone() {
        crate::gates::push_unique(&mut defs, def);
    }
    let mut trusted = false;
    if let Some(cwd) = cwd {
        trusted = crate::gates::project_trusted(&cwd, &cfg.trusted_projects);
        let places =
            crate::sources::places(&cwd, &cfg.skills_dirs, Some(crate::paths::config_dir()));
        let found = crate::sources::discover(&places);
        for def in crate::gates::from_skills(&found, &mut Vec::new()) {
            crate::gates::push_unique(&mut defs, def);
        }
    }
    // An untrusted project's gates are listed too, marked, so `observe` shows why
    // nothing ran.
    let mut out = crate::gates::describe(&defs);
    if !trusted {
        for (value, def) in out.iter_mut().zip(&defs) {
            if crate::gates::is_project_gate(def) {
                value["trusted"] = serde_json::json!(false);
            }
        }
    }
    out
}

/// Every `gate` event of the log, in order.
fn fired_gates(events: &[LogEvent]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::Gate {
                gate,
                event: at,
                decision,
                reason,
                ms,
                rewritten,
                error,
            } => Some(serde_json::json!({
                "seq": event.seq,
                "gate": gate,
                "event": at,
                "decision": decision,
                "reason": reason,
                "ms": ms,
                "rewritten": rewritten,
                "error": error,
            })),
            _ => None,
        })
        .collect()
}

fn sources_of(events: &[LogEvent]) -> Vec<(String, String, bool)> {
    let mut out: Vec<(String, String, bool)> = crate::sources::sources_of(events)
        .into_iter()
        .map(|source| {
            let kind = crate::sources::kind_of(&source).event_kind().to_string();
            (kind, source.name, true)
        })
        .collect();
    for event in events {
        if let EventBody::Source { kind, name, .. } = &event.body {
            out.push((kind.clone(), name.clone(), false));
        }
    }
    out
}

fn last_hint(events: &[LogEvent], dir: &Path) -> Option<String> {
    events.iter().rev().find_map(|event| match &event.body {
        EventBody::Source {
            kind,
            text: Some(text),
            ..
        } if kind == crate::hint::SOURCE_KIND => text.resolve_text(dir).ok(),
        _ => None,
    })
}

fn cache_stats(events: &[LogEvent]) -> CacheStats {
    let mut stats = CacheStats::default();
    for event in events {
        if let EventBody::Usage {
            prompt_tokens,
            cached_tokens,
            kind: None,
            ..
        } = &event.body
        {
            stats.prompt_tokens += prompt_tokens;
            stats.cached_tokens += cached_tokens;
            stats.last_prompt = *prompt_tokens;
            stats.last_cached = *cached_tokens;
        }
    }
    stats
}

/// The exact JSON body, as the loop serialises it.
pub fn raw(observed: &Observed) -> String {
    serde_json::to_string(&observed.body).unwrap_or_default()
}

/// The human view: every part of what the model sees, in order.
pub fn render(observed: &Observed) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "session {}  {}/{}  {} step(s) so far",
        observed.id, observed.provider, observed.model, observed.steps
    );
    let _ = writeln!(out, "next request: POST {}", observed.url);
    let _ = writeln!(
        out,
        "estimate: ~{} tokens (bytes/4 of the messages; the provider's own count is in `usage`)",
        observed.estimate
    );
    let overall = CacheStats::ratio(observed.cache.prompt_tokens, observed.cache.cached_tokens);
    let last = CacheStats::ratio(observed.cache.last_prompt, observed.cache.last_cached);
    let _ = writeln!(
        out,
        "cache: {} overall, {} on the last step",
        percent(overall),
        percent(last)
    );
    let _ = writeln!(out, "\n== sources ({}) ==", observed.sources.len());
    for (kind, name, frozen) in &observed.sources {
        let _ = writeln!(
            out,
            "  {kind:<11} {name}  ({})",
            if *frozen {
                "in the system"
            } else {
                "tail message"
            }
        );
    }
    let _ = writeln!(out, "\n== hint ==");
    match &observed.hint {
        Some(text) => {
            let _ = writeln!(out, "{}", text.trim_end());
        }
        None => {
            let _ = writeln!(out, "(none)");
        }
    }
    let _ = writeln!(
        out,
        "
== gates ({}) ==",
        observed.gates.len()
    );
    if observed.gates.is_empty() {
        let _ = writeln!(out, "(none configured)");
    }
    for gate in &observed.gates {
        let _ = writeln!(
            out,
            "  {:<14} {:<10} {}  [{}]",
            gate["event"].as_str().unwrap_or(""),
            gate["name"].as_str().unwrap_or(""),
            gate["command"].as_str().unwrap_or(""),
            gate["origin"].as_str().unwrap_or("")
        );
    }
    let _ = writeln!(
        out,
        "
== gates fired ({}) ==",
        observed.fired.len()
    );
    for run in observed.fired.iter().rev().take(20).rev() {
        let _ = writeln!(
            out,
            "  #{} {} {} -> {} ({} ms){}{}",
            run["seq"],
            run["event"].as_str().unwrap_or(""),
            run["gate"].as_str().unwrap_or(""),
            run["decision"].as_str().unwrap_or(""),
            run["ms"],
            match run["reason"].as_str() {
                Some(reason) if !reason.is_empty() => format!(": {reason}"),
                _ => String::new(),
            },
            match run["error"].as_str() {
                Some(error) => format!("  [failed open: {error}]"),
                None => String::new(),
            }
        );
    }
    let _ = writeln!(out, "\n== tools ({}) ==", observed.tools.len());
    let _ = writeln!(out, "{}", observed.tools.join(", "));
    let _ = writeln!(out, "\n== system ({} bytes) ==", observed.system.len());
    let _ = writeln!(out, "{}", observed.system.trim_end());
    let _ = writeln!(out, "\n== messages ({}) ==", observed.messages.len());
    for (index, message) in observed.messages.iter().enumerate() {
        let _ = writeln!(out, "[{}] {}", index + 1, message_line(message));
    }
    out
}

fn percent(ratio: Option<f64>) -> String {
    match ratio {
        Some(ratio) => format!("{:.0}%", ratio * 100.0),
        None => "n/a".to_string(),
    }
}

fn message_line(message: &Value) -> String {
    let role = message["role"].as_str().unwrap_or("?");
    let mut text = String::new();
    match &message["content"] {
        Value::String(content) => text.push_str(content),
        Value::Array(parts) => {
            for part in parts {
                match part["type"].as_str() {
                    Some("text") => text.push_str(part["text"].as_str().unwrap_or("")),
                    Some("image_url") => text.push_str("[image]"),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    if let Some(calls) = message["tool_calls"].as_array() {
        for call in calls {
            let _ = write!(
                text,
                "\n    -> {}({})",
                call["function"]["name"].as_str().unwrap_or("?"),
                call["function"]["arguments"].as_str().unwrap_or("")
            );
        }
    }
    if let Some(id) = message["tool_call_id"].as_str() {
        return format!("{role} ({id}): {text}");
    }
    format!("{role}: {text}")
}

/// `kelir observe <session> [--raw]`. Exit 0 on success, 1 on any failure.
pub fn cli(args: &crate::cli::ObserveArgs) -> u8 {
    run(&args.session, args.raw, args.config.as_deref())
}

pub fn run(spec: &str, raw_body: bool, config: Option<&Path>) -> u8 {
    use std::io::Write;
    let cfg = match crate::config::load(config) {
        Ok(cfg) => cfg,
        Err(error) => {
            eprintln!("kelir: {error}");
            return 2;
        }
    };
    let id = match crate::paths::resolve_session(spec) {
        Ok(id) => id,
        Err(message) => {
            eprintln!("kelir: {message}");
            return 1;
        }
    };
    let dir = crate::paths::session_dir(&id);
    match build(&dir, &id, &cfg) {
        Ok(observed) => {
            let text = if raw_body {
                raw(&observed)
            } else {
                render(&observed)
            };
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(text.as_bytes());
            let _ = stdout.flush();
            0
        }
        Err(message) => {
            eprintln!("kelir: observe: {message}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_message_line_shows_text_images_and_calls() {
        let plain = json!({"role": "user", "content": "hello"});
        assert_eq!(message_line(&plain), "user: hello");
        let parts = json!({"role": "user", "content": [
            {"type": "text", "text": "look"},
            {"type": "image_url", "image_url": {"url": "data:..."}}
        ]});
        assert_eq!(message_line(&parts), "user: look[image]");
        let call = json!({"role": "assistant", "content": null, "tool_calls": [
            {"id": "c1", "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}}
        ]});
        assert!(message_line(&call).contains("-> bash({\"command\":\"ls\"})"));
        let result = json!({"role": "tool", "tool_call_id": "c1", "content": "ok"});
        assert_eq!(message_line(&result), "tool (c1): ok");
    }

    #[test]
    fn the_cache_ratio_is_none_before_any_usage() {
        assert_eq!(CacheStats::ratio(0, 0), None);
        assert_eq!(CacheStats::ratio(200, 150), Some(0.75));
        assert_eq!(percent(None), "n/a");
        assert_eq!(percent(Some(0.75)), "75%");
    }
}
