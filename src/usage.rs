//! Token tally and status you can see.
//! Three small things live here, none of them in the request path: the tally a resumed
//! session folds from the log, the prompt-size warning, and the `status` answer.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::event::EventBody;

/// The prompt size above which the session warns (`warn_prompt_tokens`).
pub const DEFAULT_WARN_PROMPT_TOKENS: u64 = 150_000;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LastUsage {
    pub prompt: u64,
    pub cached: u64,
    pub completion: u64,
}

/// The most recent `error` event.
#[derive(Debug, Clone)]
pub struct LastError {
    pub seq: u64,
    pub location: String,
    pub message: String,
    pub status: Option<u16>,
    pub retryable: bool,
    pub attempt: Option<u32>,
}

/// What a session used and what it is doing, derived from the log.
#[derive(Debug, Clone, Default)]
pub struct Tally {
    /// Model requests that reported usage (summaries not counted).
    pub requests: u32,
    pub prompt: u64,
    pub completion: u64,
    pub cached: u64,
    pub cache_write: u64,
    pub last: Option<LastUsage>,
    pub last_usage_seq: u64,
    pub last_error: Option<LastError>,
    /// `idle`, `running`, `done`, `stopped`, `paused` or `error`.
    pub state: String,
    pub pause_reason: Option<String>,
    open_jobs: BTreeSet<String>,
    warned_prompt: bool,
}

/// The threshold `warn_prompt_tokens` sets.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Warn {
    pub prompt_tokens: Option<u64>,
}

impl Tally {
    /// Fold a whole log (what a resumed session starts from). No warning fires
    /// for a state the log was already in: the threshold is armed against it.
    pub fn from_events(events: &[crate::event::LogEvent], warn: Warn) -> Self {
        let own = crate::child::own_start(events);
        let mut tally = Tally {
            state: "idle".into(),
            ..Tally::default()
        };
        for (index, event) in events.iter().enumerate() {
            if index == own {
                // A brief's log begins with a copy of its parent's: those tokens are
                // the parent's, already counted there. Keep only the last request
                // (the context the brief starts from), not the sums.
                tally.reset_tokens();
            }
            tally.observe(event.seq, &event.body);
        }
        let _ = tally.warnings(warn);
        tally
    }

    fn reset_tokens(&mut self) {
        self.requests = 0;
        self.prompt = 0;
        self.completion = 0;
        self.cached = 0;
        self.cache_write = 0;
    }

    pub fn observe(&mut self, seq: u64, body: &EventBody) {
        match body {
            EventBody::Usage {
                prompt_tokens,
                completion_tokens,
                cached_tokens,
                cache_write_tokens,
                kind,
                ..
            } => {
                self.prompt += prompt_tokens;
                self.completion += completion_tokens;
                self.cached += cached_tokens;
                self.cache_write += cache_write_tokens;
                // A summary is not a step, and not the context the next request carries.
                if kind.is_none() {
                    self.requests += 1;
                    self.last = Some(LastUsage {
                        prompt: *prompt_tokens,
                        cached: *cached_tokens,
                        completion: *completion_tokens,
                    });
                    self.last_usage_seq = seq;
                }
            }
            EventBody::StepStarted { .. } | EventBody::User { .. } => {
                self.state = "running".into();
                self.pause_reason = None;
            }
            EventBody::TurnEnded {
                outcome, reason, ..
            } => {
                self.state = outcome.clone();
                self.pause_reason = reason.clone();
            }
            EventBody::Error {
                location,
                message,
                status,
                retryable,
                attempt,
                ..
            } => {
                self.last_error = Some(LastError {
                    seq,
                    location: location.clone(),
                    message: crate::util::truncate_chars(message, 300),
                    status: *status,
                    retryable: *retryable,
                    attempt: *attempt,
                });
            }
            EventBody::BgStarted { bg_id, .. } => {
                self.open_jobs.insert(bg_id.clone());
            }
            EventBody::BgSettled { bg_id, .. } => {
                self.open_jobs.remove(bg_id);
            }
            EventBody::SessionStarted { .. } if self.state.is_empty() => {
                self.state = "idle".into();
            }
            _ => {}
        }
    }

    /// The warnings the log now owes: each threshold speaks **once per crossing**.
    pub fn warnings(&mut self, warn: Warn) -> Vec<Warning> {
        let mut out = Vec::new();
        if let (Some(limit), Some(last)) = (warn.prompt_tokens, &self.last) {
            if last.prompt > limit {
                if !self.warned_prompt {
                    self.warned_prompt = true;
                    out.push(Warning {
                        kind: "prompt_tokens",
                        message: format!(
                            "the last prompt was {} tokens, over warn_prompt_tokens ({})",
                            last.prompt, limit
                        ),
                        value: last.prompt as f64,
                        threshold: limit as f64,
                    });
                }
            } else {
                self.warned_prompt = false;
            }
        }
        out
    }

    pub fn open_jobs(&self) -> usize {
        self.open_jobs.len()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Warning {
    pub kind: &'static str,
    pub message: String,
    pub value: f64,
    pub threshold: f64,
}

// ------------------------------------------------------------ the status verb

/// A retry (or a wait for a provider slot) in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryNote {
    pub status: Option<u16>,
    pub attempt: u32,
    pub remaining_ms: u64,
    /// A wait for a local provider slot, not a provider failure.
    pub slot: bool,
}

/// The fields only the live process knows.
pub struct Live<'a> {
    pub id: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub turn: u32,
    pub turn_steps: u32,
    pub max_steps: Option<u32>,
    pub session_steps: u32,
    pub window: Option<u64>,
    pub window_source: &'a str,
    pub running: &'a [String],
    pub retry: Option<RetryNote>,
    pub warn: Warn,
}

fn ratio(part: u64, whole: u64) -> Option<f64> {
    (whole > 0).then(|| ((part as f64 / whole as f64) * 1000.0).round() / 1000.0)
}

/// The `status` answer. Every field is always present (`null` when there is
/// nothing to say yet), so a script never has to test for a key.
pub fn status_json(tally: &Tally, live: &Live<'_>) -> Value {
    let state = match live.retry {
        Some(note) if note.slot => "waiting_for_slot",
        Some(_) => "retrying",
        None => tally.state.as_str(),
    };
    let last = tally.last.as_ref();
    let context_percent = last
        .zip(live.window.filter(|w| *w > 0))
        .map(|(last, window)| ((last.prompt as f64 / window as f64) * 1000.0).round() / 10.0);
    let error = tally.last_error.as_ref().map(|error| {
        json!({
            "seq": error.seq,
            "where": error.location,
            "message": error.message,
            "status": error.status,
            "retryable": error.retryable,
            "attempt": error.attempt,
            // A later successful request means it passed.
            "recovered": tally.last_usage_seq > error.seq,
        })
    });
    let retry = live.retry.map(|note| {
        json!({
            "kind": if note.slot { "slot" } else { "provider" },
            "status": note.status,
            "attempt": note.attempt,
            "remaining_ms": note.remaining_ms,
        })
    });
    json!({
        "session": live.id,
        "state": state,
        "pause_reason": tally.pause_reason,
        "provider": live.provider,
        "model": live.model,
        "turn": live.turn,
        "turn_steps": live.turn_steps,
        "max_steps": live.max_steps,
        "session_steps": live.session_steps,
        "requests": tally.requests,
        "context": {
            "last_prompt_tokens": last.map(|last| last.prompt),
            "window": live.window,
            "window_source": live.window_source,
            "percent": context_percent,
        },
        "cache": {
            "last_hit_ratio": last.and_then(|last| ratio(last.cached, last.prompt)),
            "session_hit_ratio": ratio(tally.cached, tally.prompt),
        },
        "tokens": {
            "prompt": tally.prompt,
            "completion": tally.completion,
            "cached": tally.cached,
            "cache_write": tally.cache_write,
            "total": tally.prompt + tally.completion,
        },
        "warn": {
            "prompt_tokens": live.warn.prompt_tokens,
        },
        "last_error": error,
        "retry": retry,
        "running_calls": live.running,
        "bg_jobs": tally.open_jobs(),
        "children": 0,
    })
}

/// `12345` -> `12.3k`, `1500000` -> `1.5M`.
fn compact(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

fn percent(value: &Value) -> String {
    value
        .as_f64()
        .map_or("-".to_string(), |ratio| format!("{:.0}%", ratio * 100.0))
}

pub fn render_text(state: &Value) -> String {
    let get = |path: &[&str]| {
        path.iter()
            .try_fold(state, |value, key| value.get(*key))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let text = |path: &[&str]| get(path).as_str().unwrap_or("?").to_string();
    let num = |path: &[&str]| get(path).as_u64().unwrap_or(0);

    let mut lines = Vec::new();
    let mut head = format!("{} · {}", text(&["session"]), text(&["state"]));
    if let Some(reason) = get(&["pause_reason"]).as_str() {
        head.push_str(&format!(" ({reason})"));
    }
    lines.push(head);
    lines.push(format!(
        "model     {} / {}",
        text(&["provider"]),
        text(&["model"])
    ));
    let cap = get(&["max_steps"])
        .as_u64()
        .map_or("no limit".to_string(), |cap| format!("limit {cap}"));
    lines.push(format!(
        "steps     {} this turn ({cap}) · {} in the session · turn {}",
        num(&["turn_steps"]),
        num(&["session_steps"]),
        num(&["turn"]),
    ));
    let context = match get(&["context", "last_prompt_tokens"]).as_u64() {
        Some(prompt) => {
            let window = get(&["context", "window"]).as_u64().unwrap_or(0);
            let share = get(&["context", "percent"])
                .as_f64()
                .map_or(String::new(), |p| {
                    format!(" = {p:.1}% of {}", compact(window))
                });
            format!("{} tokens{share}", compact(prompt))
        }
        None => "no request yet".to_string(),
    };
    lines.push(format!("context   last prompt {context}"));
    lines.push(format!(
        "cache     {} last · {} session",
        percent(&get(&["cache", "last_hit_ratio"])),
        percent(&get(&["cache", "session_hit_ratio"])),
    ));
    lines.push(format!(
        "tokens    {} total ({} in, {} cached, {} out)",
        compact(num(&["tokens", "total"])),
        compact(num(&["tokens", "prompt"])),
        compact(num(&["tokens", "cached"])),
        compact(num(&["tokens", "completion"])),
    ));
    if let Some(retry) = get(&["retry"]).as_object() {
        let what = if retry.get("kind").and_then(Value::as_str) == Some("slot") {
            "waiting for a provider slot".to_string()
        } else {
            let status = retry
                .get("status")
                .and_then(Value::as_u64)
                .map_or(String::new(), |status| format!("http {status}, "));
            format!(
                "retrying ({status}attempt {})",
                retry.get("attempt").and_then(Value::as_u64).unwrap_or(0)
            )
        };
        let wait = retry
            .get("remaining_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        lines.push(format!(
            "retry     {what}, next try in {}s",
            wait.div_ceil(1000)
        ));
    }
    if let Some(error) = get(&["last_error"]).as_object() {
        let status = error
            .get("status")
            .and_then(Value::as_u64)
            .map_or(String::new(), |status| format!("http {status} "));
        let recovered = if error.get("recovered").and_then(Value::as_bool) == Some(true) {
            " (since recovered)"
        } else {
            ""
        };
        lines.push(format!(
            "error     {status}{}{recovered}",
            error.get("message").and_then(Value::as_str).unwrap_or("?")
        ));
    }
    let running = get(&["running_calls"]);
    let running = running.as_array().map_or(0, Vec::len);
    lines.push(format!(
        "work      {running} running call(s) · {} background job(s) · {} child session(s)",
        num(&["bg_jobs"]),
        num(&["children"]),
    ));
    if let Some(kids) = get(&["child_list"]).as_array().filter(|a| !a.is_empty()) {
        for kid in kids {
            lines.push(format!(
                "child     {} d{} {} {}{}",
                kid["session"].as_str().unwrap_or("?"),
                kid["depth"].as_u64().unwrap_or(0),
                kid["kind"].as_str().unwrap_or("?"),
                kid["state"].as_str().unwrap_or("?"),
                kid["pid"]
                    .as_u64()
                    .map_or(String::new(), |pid| format!(" pid {pid}")),
            ));
        }
    }
    if num(&["gates", "declared"]) > 0 || num(&["gates", "runs"]) > 0 {
        let last = get(&["gates", "last"]);
        let last = match (last["gate"].as_str(), last["decision"].as_str()) {
            (Some(gate), Some(decision)) => format!(
                " · last {} {gate} -> {decision}",
                last["event"].as_str().unwrap_or("?")
            ),
            _ => String::new(),
        };
        lines.push(format!(
            "gates     {} declared · {} run(s) · {} denied · {} rewritten · {} failed open{last}",
            num(&["gates", "declared"]),
            num(&["gates", "runs"]),
            num(&["gates", "denied"]),
            num(&["gates", "rewritten"]),
            num(&["gates", "failed_open"]),
        ));
    }
    if let Some(servers) = get(&["mcp"]).as_array().filter(|a| !a.is_empty()) {
        let parts: Vec<String> = servers
            .iter()
            .map(|s| {
                let tools = s["tools"]
                    .as_u64()
                    .map_or(String::new(), |n| format!(", {n} tools"));
                format!(
                    "{} ({}{tools})",
                    s["name"].as_str().unwrap_or("?"),
                    s["state"].as_str().unwrap_or("?")
                )
            })
            .collect();
        lines.push(format!("mcp       {}", parts.join(" · ")));
    }
    if let Some(warn_prompt) = get(&["warn", "prompt_tokens"]).as_u64() {
        lines.push(format!("warns at  prompt > {}", compact(warn_prompt)));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(prompt: u64, cached: u64, kind: Option<&str>) -> EventBody {
        EventBody::Usage {
            step: 1,
            prompt_tokens: prompt,
            completion_tokens: 10,
            cached_tokens: cached,
            cache_write_tokens: 0,
            allowance: None,
            kind: kind.map(str::to_string),
        }
    }

    #[test]
    fn the_tally_sums_tokens_of_all_requests() {
        let mut tally = Tally::default();
        tally.observe(2, &usage(1000, 500, None));
        tally.observe(4, &usage(2000, 1000, None));
        tally.observe(6, &usage(3000, 0, None));
        assert_eq!(tally.requests, 3);
        assert_eq!(tally.prompt, 6000);
        assert_eq!(tally.completion, 30);
        assert_eq!(tally.cached, 1500);
        assert_eq!(tally.last.as_ref().unwrap().prompt, 3000);
    }

    #[test]
    fn a_summary_is_not_a_step_or_the_context() {
        let mut tally = Tally::default();
        tally.observe(2, &usage(1000, 0, None));
        tally.observe(4, &usage(9000, 0, Some("compact")));
        assert_eq!(tally.requests, 1);
        assert_eq!(tally.prompt, 10_000);
        assert_eq!(tally.last.as_ref().unwrap().prompt, 1000);
    }

    #[test]
    fn the_prompt_warning_fires_once_per_crossing_and_rearms_below() {
        let warn = Warn {
            prompt_tokens: Some(150_000),
        };
        let mut tally = Tally::default();
        tally.observe(2, &usage(100_000, 0, None));
        assert!(tally.warnings(warn).is_empty());
        tally.observe(4, &usage(150_001, 0, None));
        let first = tally.warnings(warn);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].kind, "prompt_tokens");
        // still over: silent
        tally.observe(6, &usage(160_000, 0, None));
        assert!(tally.warnings(warn).is_empty());
        // a compaction brings it down, and the next climb speaks again
        tally.observe(8, &usage(20_000, 0, None));
        assert!(tally.warnings(warn).is_empty());
        tally.observe(10, &usage(151_000, 0, None));
        assert_eq!(tally.warnings(warn).len(), 1);
    }

    #[test]
    fn the_exact_threshold_is_not_over_it() {
        let warn = Warn {
            prompt_tokens: Some(150_000),
        };
        let mut tally = Tally::default();
        tally.observe(2, &usage(150_000, 0, None));
        assert!(tally.warnings(warn).is_empty());
    }

    #[test]
    fn no_threshold_means_no_warning() {
        let mut tally = Tally::default();
        tally.observe(2, &usage(900_000, 0, None));
        assert!(tally.warnings(Warn::default()).is_empty());
    }

    #[test]
    fn a_resumed_log_does_not_repeat_a_warning_it_already_owed() {
        let warn = Warn {
            prompt_tokens: Some(1000),
        };
        let events = vec![crate::event::LogEvent::new(2, usage(5000, 0, None))];
        let mut tally = Tally::from_events(&events, warn);
        assert!(tally.warnings(warn).is_empty());
    }

    fn live<'a>(retry: Option<RetryNote>, running: &'a [String]) -> Live<'a> {
        Live {
            id: "s1",
            provider: "openrouter",
            model: "m",
            turn: 1,
            turn_steps: 2,
            max_steps: Some(5),
            session_steps: 2,
            window: Some(200_000),
            window_source: "api",
            running,
            retry,
            warn: Warn {
                prompt_tokens: Some(150_000),
            },
        }
    }

    #[test]
    fn status_window_is_null_until_known() {
        let mut tally = Tally::default();
        tally.observe(2, &usage(100_000, 0, None));
        let mut l = live(None, &[]);
        l.window = None;
        l.window_source = "default";
        let json = status_json(&tally, &l);
        assert!(json["context"]["window"].is_null());
        assert!(json["context"]["percent"].is_null());
        assert_eq!(json["context"]["window_source"], "default");
        let text = render_text(&json);
        assert!(text.contains("100.0k tokens"), "{text}");
    }

    #[test]
    fn status_is_complete_before_any_request() {
        let tally = Tally::from_events(&[], Warn::default());
        let json = status_json(&tally, &live(None, &[]));
        assert_eq!(json["state"], "idle");
        assert_eq!(json["requests"], 0);
        assert!(json["context"]["last_prompt_tokens"].is_null());
        assert!(json["cache"]["last_hit_ratio"].is_null());
        assert_eq!(json["tokens"]["total"], 0);
        assert!(json["last_error"].is_null());
        assert!(json["retry"].is_null());
        let text = render_text(&json);
        assert!(text.contains("no request yet"), "{text}");
    }

    #[test]
    fn status_reports_context_percent_cache_and_tokens_after_steps() {
        let mut tally = Tally::default();
        tally.observe(2, &usage(100_000, 75_000, None));
        let json = status_json(&tally, &live(None, &[]));
        assert_eq!(json["context"]["last_prompt_tokens"], 100_000);
        assert_eq!(json["context"]["percent"], 50.0);
        assert_eq!(json["cache"]["last_hit_ratio"], 0.75);
        assert_eq!(json["tokens"]["cached"], 75_000);
        let text = render_text(&json);
        assert!(text.contains("50.0% of 200.0k"), "{text}");
        assert!(text.contains("75% last"), "{text}");
        assert!(text.contains("limit 5"), "{text}");
    }

    #[test]
    fn a_retry_in_progress_shows_in_state_and_text() {
        let tally = Tally::default();
        let note = RetryNote {
            status: Some(429),
            attempt: 2,
            remaining_ms: 4200,
            slot: false,
        };
        let json = status_json(&tally, &live(Some(note), &["c1".to_string()]));
        assert_eq!(json["state"], "retrying");
        assert_eq!(json["retry"]["status"], 429);
        assert_eq!(json["running_calls"][0], "c1");
        let text = render_text(&json);
        assert!(
            text.contains("retrying (http 429, attempt 2), next try in 5s"),
            "{text}"
        );
    }

    #[test]
    fn a_recovered_error_says_so() {
        let mut tally = Tally::default();
        tally.observe(
            2,
            &EventBody::Error {
                location: "provider".into(),
                message: "rate limited".into(),
                retryable: true,
                status: Some(429),
                attempt: Some(1),
                retry_in_ms: Some(1000),
            },
        );
        let before = status_json(&tally, &live(None, &[]));
        assert_eq!(before["last_error"]["recovered"], false);
        tally.observe(4, &usage(10, 0, None));
        let after = status_json(&tally, &live(None, &[]));
        assert_eq!(after["last_error"]["recovered"], true);
        assert!(render_text(&after).contains("(since recovered)"));
    }

    #[test]
    fn a_paused_turn_names_its_reason() {
        let mut tally = Tally::default();
        tally.observe(
            2,
            &EventBody::TurnEnded {
                outcome: "paused".into(),
                reason: Some("steps".into()),
                final_text: String::new().into(),
            },
        );
        let json = status_json(&tally, &live(None, &[]));
        assert_eq!(json["state"], "paused");
        assert_eq!(json["pause_reason"], "steps");
        assert!(render_text(&json).contains("paused (steps)"));
    }
}
