//! Spend and status you can see (STEER-8).
//!
//! Three small things live here, none of them in the request path:
//!
//! * the **price table** and the cost arithmetic: when the provider reports no
//!   cost, a model with a known price gets an estimate, and a model without one
//!   gets *no* estimate (never a wrong one). A provider's own cost always wins.
//! * the **[`Tally`]**: what a session has spent and what state it is in, folded
//!   from the event log (so a resumed session starts where the log left off).
//! * the **`status` verb's answer**: the tally plus the live fields only the
//!   process knows, as JSON, and the text a human reads from it.
//!
//! Warnings (`warn_prompt_tokens`, `warn_cost`) are decided by the tally and
//! only ever go to the log and the screen. They are never put in the model's
//! context.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::event::EventBody;

/// The date the built-in prices were read from the providers' public model
/// lists. Printed by `kelir ctl status`, so a stale table is visible.
pub const PRICES_AS_OF: &str = "2026-11-20";

/// The prompt size above which the session warns (`warn_prompt_tokens`).
pub const DEFAULT_WARN_PROMPT_TOKENS: u64 = 150_000;

/// US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    /// Cached input tokens; the plain input price when a provider has no
    /// cache discount.
    pub cached_input: f64,
    pub output: f64,
}

struct Entry {
    provider: &'static str,
    model: &'static str,
    price: Price,
}

const fn price(input: f64, cached_input: f64, output: f64) -> Price {
    Price {
        input,
        cached_input,
        output,
    }
}

/// The built-in table. Data, not logic: add a row, never an `if`. Only
/// models whose price the provider publishes are here; everything else is
/// "unknown", which is an honest answer (Go bills against an allowance, so it
/// has no rows). `[prices."<model>"]` in `config.toml` overrides a row and adds
/// models.
const TABLE: &[Entry] = &[
    Entry {
        provider: "openrouter",
        model: "deepseek/deepseek-v4.1-flash-1017",
        price: price(0.30, 0.006, 1.20),
    },
    Entry {
        provider: "openrouter",
        model: "deepseek/deepseek-v4.1-flash",
        price: price(0.30, 0.006, 1.20),
    },
    Entry {
        provider: "openrouter",
        model: "deepseek/deepseek-v4-pro",
        price: price(0.90, 0.20, 1.50),
    },
    Entry {
        provider: "openrouter",
        model: "deepseek/deepseek-v4-flash",
        price: price(0.09, 0.018, 0.18),
    },
];

/// Where a price came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceFrom {
    Config,
    Table,
}

/// The price of `model` on `provider`: the config's, then the built-in table's.
/// Matching is on the whole id (case-insensitive): a `:batch` variant is a
/// different price and must not borrow its parent's.
pub fn lookup(
    provider: &str,
    model: &str,
    overrides: &BTreeMap<String, Price>,
) -> Option<(Price, PriceFrom)> {
    if let Some((_, found)) = overrides
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(model))
    {
        return Some((*found, PriceFrom::Config));
    }
    TABLE
        .iter()
        .find(|entry| entry.provider == provider && entry.model.eq_ignore_ascii_case(model))
        .map(|entry| (entry.price, PriceFrom::Table))
}

/// What a request cost at `price`. Cached tokens are part of `prompt_tokens`
/// (that is how both providers report them), so the plain input rate applies
/// to the rest. Rounded to a billionth of a dollar, which removes float noise
/// and nothing a person could see.
pub fn estimate(price: &Price, prompt: u64, cached: u64, completion: u64) -> f64 {
    let cached = cached.min(prompt);
    let plain = prompt - cached;
    let micro = plain as f64 * price.input
        + cached as f64 * price.cached_input
        + completion as f64 * price.output;
    round_usd(micro / 1_000_000.0)
}

fn round_usd(value: f64) -> f64 {
    (value * 1e9).round() / 1e9
}

/// Where a recorded cost came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostSource {
    /// The provider's own number.
    Provider,
    /// Tokens times a known price.
    Table,
    /// No cost reported and no price known: the cost is 0.0 here only because
    /// it is not known.
    Unknown,
}

impl CostSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CostSource::Provider => "provider",
            CostSource::Table => "table",
            CostSource::Unknown => "unknown",
        }
    }
}

/// The cost to record for one request. A provider cost above zero always wins;
/// a reported zero (Go says `0.0` for everything) is not a cost, so the price
/// table is tried, and a model it does not know stays `unknown`.
pub fn resolve(
    reported: f64,
    price: Option<&Price>,
    prompt: u64,
    cached: u64,
    completion: u64,
) -> (f64, CostSource) {
    if reported.is_finite() && reported > 0.0 {
        return (reported, CostSource::Provider);
    }
    match price {
        Some(price) => (
            estimate(price, prompt, cached, completion),
            CostSource::Table,
        ),
        None => (0.0, CostSource::Unknown),
    }
}

/// The source of a `usage` event, including one written before the field
/// existed (a cost above zero was the provider's; zero was nothing).
pub fn source_of(cost: f64, recorded: Option<&str>) -> CostSource {
    match recorded {
        Some("provider") => CostSource::Provider,
        Some("table") => CostSource::Table,
        Some(_) => CostSource::Unknown,
        None if cost > 0.0 => CostSource::Provider,
        None => CostSource::Unknown,
    }
}

// ---------------------------------------------------------------- the tally

/// The most recent model request.
#[derive(Debug, Clone, Default)]
pub struct LastUsage {
    pub prompt: u64,
    pub cached: u64,
    pub completion: u64,
    pub cost: f64,
    pub unknown_cost: bool,
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

/// What a session has spent and what it is doing, derived from the log.
#[derive(Debug, Clone, Default)]
pub struct Tally {
    /// Model requests that reported usage (summaries not counted).
    pub requests: u32,
    pub prompt: u64,
    pub completion: u64,
    pub cached: u64,
    pub cache_write: u64,
    /// The sum of the costs that are known.
    pub cost: f64,
    pub by_provider: u32,
    pub by_table: u32,
    /// Requests whose cost is not known (no provider cost, no price).
    pub unpriced: u32,
    pub last: Option<LastUsage>,
    pub last_usage_seq: u64,
    pub last_error: Option<LastError>,
    /// `idle`, `running`, `done`, `stopped`, `paused` or `error`.
    pub state: String,
    pub pause_reason: Option<String>,
    open_jobs: BTreeSet<String>,
    warned_prompt: bool,
    warned_cost: bool,
}

/// The thresholds `warn_prompt_tokens` and `warn_cost` set.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Warn {
    pub prompt_tokens: Option<u64>,
    pub cost: Option<f64>,
}

impl Tally {
    /// Fold a whole log (what a resumed session starts from). No warning fires
    /// for a state the log was already in: the thresholds are armed against it.
    pub fn from_events(events: &[crate::event::LogEvent], warn: Warn) -> Self {
        let mut tally = Tally {
            state: "idle".into(),
            ..Tally::default()
        };
        for event in events {
            tally.observe(event.seq, &event.body);
        }
        let _ = tally.warnings(warn);
        tally
    }

    /// Take one event in log order.
    pub fn observe(&mut self, seq: u64, body: &EventBody) {
        match body {
            EventBody::Usage {
                prompt_tokens,
                completion_tokens,
                cached_tokens,
                cache_write_tokens,
                cost,
                cost_source,
                kind,
                ..
            } => {
                let source = source_of(*cost, cost_source.as_deref());
                self.prompt += prompt_tokens;
                self.completion += completion_tokens;
                self.cached += cached_tokens;
                self.cache_write += cache_write_tokens;
                match source {
                    CostSource::Provider => self.by_provider += 1,
                    CostSource::Table => self.by_table += 1,
                    CostSource::Unknown => self.unpriced += 1,
                }
                if source != CostSource::Unknown {
                    self.cost = round_usd(self.cost + cost);
                }
                // A summary is spend but not a step, and not the context the
                // next request carries.
                if kind.is_none() {
                    self.requests += 1;
                    self.last = Some(LastUsage {
                        prompt: *prompt_tokens,
                        cached: *cached_tokens,
                        completion: *completion_tokens,
                        cost: *cost,
                        unknown_cost: source == CostSource::Unknown,
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

    /// The warnings the log now owes: each threshold speaks **once per
    /// crossing**. The prompt threshold re-arms when the prompt falls back under
    /// it (a compaction); spend only grows, so the cost threshold speaks once.
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
        if let Some(limit) = warn.cost
            && self.cost > limit
            && !self.warned_cost
        {
            self.warned_cost = true;
            out.push(Warning {
                kind: "cost",
                message: format!(
                    "this session's estimated cost is ${:.4}, over warn_cost (${limit})",
                    self.cost
                ),
                value: self.cost,
                threshold: limit,
            });
        }
        out
    }

    pub fn open_jobs(&self) -> usize {
        self.open_jobs.len()
    }

    /// `provider`, `table`, `mixed` or `unknown`.
    fn cost_source(&self) -> &'static str {
        match (self.by_provider > 0, self.by_table > 0) {
            (true, true) => "mixed",
            (true, false) => "provider",
            (false, true) => "table",
            (false, false) => "unknown",
        }
    }
}

/// One warning, as the log records it.
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
    /// Milliseconds left on the wait at the moment of the answer.
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
    pub window: u64,
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
        .filter(|_| live.window > 0)
        .map(|last| ((last.prompt as f64 / live.window as f64) * 1000.0).round() / 10.0);
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
        "cost": {
            // `null` when no request has a known cost: unknown is not zero.
            "usd": if tally.by_provider + tally.by_table > 0 { json!(tally.cost) } else { Value::Null },
            "source": tally.cost_source(),
            "unpriced_requests": tally.unpriced,
            "prices_as_of": PRICES_AS_OF,
        },
        "warn": {
            "prompt_tokens": live.warn.prompt_tokens,
            "cost": live.warn.cost,
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
        .map_or("-".into(), |ratio| format!("{:.0}%", ratio * 100.0))
}

/// The status as a human reads it: one fact per line, nothing to decode.
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
            let window = num(&["context", "window"]);
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
    let unpriced = num(&["cost", "unpriced_requests"]);
    let cost = match get(&["cost", "usd"]).as_f64() {
        Some(usd) => {
            let mut cost = format!("${usd:.4} ({})", text(&["cost", "source"]));
            if unpriced > 0 {
                cost.push_str(&format!(" + {unpriced} request(s) unpriced"));
            }
            cost
        }
        None => "unknown (the provider reports none and the model has no price)".to_string(),
    };
    lines.push(format!("cost      {cost}"));
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
    let warn_prompt = get(&["warn", "prompt_tokens"]).as_u64();
    let warn_cost = get(&["warn", "cost"]).as_f64();
    if warn_prompt.is_some() || warn_cost.is_some() {
        let mut parts = Vec::new();
        if let Some(limit) = warn_prompt {
            parts.push(format!("prompt > {}", compact(limit)));
        }
        if let Some(limit) = warn_cost {
            parts.push(format!("cost > ${limit}"));
        }
        lines.push(format!("warns at  {}", parts.join(" · ")));
    }
    lines.push(format!(
        "          (built-in prices as of {})",
        text(&["cost", "prices_as_of"])
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLASH: Price = Price {
        input: 0.30,
        cached_input: 0.006,
        output: 1.20,
    };

    fn usage(
        prompt: u64,
        cached: u64,
        cost: f64,
        source: Option<&str>,
        kind: Option<&str>,
    ) -> EventBody {
        EventBody::Usage {
            step: 1,
            prompt_tokens: prompt,
            completion_tokens: 10,
            cached_tokens: cached,
            cache_write_tokens: 0,
            cost,
            cost_source: source.map(str::to_string),
            allowance: None,
            kind: kind.map(str::to_string),
        }
    }

    #[test]
    fn estimate_prices_cached_and_plain_input_apart() {
        // 400 plain * 0.30 + 600 cached * 0.006 + 200 out * 1.20 = 363.6 µ$
        let cost = estimate(&FLASH, 1000, 600, 200);
        assert_eq!(cost, 0.0003636);
    }

    #[test]
    fn estimate_never_goes_negative_when_cached_exceeds_prompt() {
        let cost = estimate(&FLASH, 100, 500, 0);
        assert_eq!(cost, estimate(&FLASH, 100, 100, 0));
    }

    #[test]
    fn estimate_rounds_to_a_billionth_of_a_dollar() {
        // 1 token at 0.30/M is 3e-7; the sum of float noise must not leak.
        assert_eq!(estimate(&FLASH, 1, 0, 0), 0.0000003);
        assert_eq!(estimate(&FLASH, 0, 0, 0), 0.0);
        let big = estimate(&FLASH, 3_333_333, 1_111_111, 777_777);
        assert_eq!(big, round_usd(big), "{big}");
    }

    #[test]
    fn a_provider_cost_always_wins_over_the_table() {
        let (cost, source) = resolve(0.5, Some(&FLASH), 1000, 0, 100);
        assert_eq!((cost, source), (0.5, CostSource::Provider));
    }

    #[test]
    fn a_reported_zero_falls_back_to_the_table_and_then_to_unknown() {
        let (cost, source) = resolve(0.0, Some(&FLASH), 1000, 600, 200);
        assert_eq!((cost, source), (0.0003636, CostSource::Table));
        let (cost, source) = resolve(0.0, None, 1000, 600, 200);
        assert_eq!((cost, source), (0.0, CostSource::Unknown));
        // NaN and negative numbers are not costs either.
        assert_eq!(resolve(f64::NAN, None, 1, 0, 1).1, CostSource::Unknown);
        assert_eq!(resolve(-1.0, None, 1, 0, 1).1, CostSource::Unknown);
    }

    #[test]
    fn lookup_is_exact_and_the_config_wins() {
        let none = BTreeMap::new();
        assert!(lookup("openrouter", "deepseek/deepseek-v4.1-flash-1017", &none).is_some());
        assert!(lookup("openrouter", "DeepSeek/DeepSeek-V4.1-Flash-1017", &none).is_some());
        // a variant is not its parent
        assert!(lookup("openrouter", "deepseek/deepseek-v4.1-flash:batch", &none).is_none());
        assert!(lookup("opencode-go", "deepseek-v4.1-flash", &none).is_none());
        let mut over = BTreeMap::new();
        over.insert(
            "deepseek/deepseek-v4.1-flash-1017".to_string(),
            price(1.0, 1.0, 1.0),
        );
        let (found, from) =
            lookup("openrouter", "deepseek/deepseek-v4.1-flash-1017", &over).unwrap();
        assert_eq!((found.input, from), (1.0, PriceFrom::Config));
        // an override also adds a model the table does not know, on any provider
        over.insert("glm-5.2".into(), price(2.0, 2.0, 4.0));
        assert!(lookup("opencode-go", "glm-5.2", &over).is_some());
    }

    #[test]
    fn an_old_usage_event_without_a_source_is_read_by_its_cost() {
        assert_eq!(source_of(0.01, None), CostSource::Provider);
        assert_eq!(source_of(0.0, None), CostSource::Unknown);
        assert_eq!(source_of(0.0, Some("table")), CostSource::Table);
    }

    #[test]
    fn the_tally_sums_known_costs_and_counts_unknown_ones_apart() {
        let mut tally = Tally::default();
        tally.observe(2, &usage(1000, 500, 0.001, Some("provider"), None));
        tally.observe(4, &usage(2000, 1000, 0.002, Some("table"), None));
        tally.observe(6, &usage(3000, 0, 0.0, Some("unknown"), None));
        assert_eq!(tally.requests, 3);
        assert_eq!(tally.cost, 0.003);
        assert_eq!(
            (tally.by_provider, tally.by_table, tally.unpriced),
            (1, 1, 1)
        );
        assert_eq!(tally.cost_source(), "mixed");
        assert_eq!(tally.last.as_ref().unwrap().prompt, 3000);
    }

    #[test]
    fn a_summary_is_spend_but_not_a_step_or_the_context() {
        let mut tally = Tally::default();
        tally.observe(2, &usage(1000, 0, 0.001, Some("provider"), None));
        tally.observe(4, &usage(9000, 0, 0.002, Some("provider"), Some("compact")));
        assert_eq!(tally.requests, 1);
        assert_eq!(tally.cost, 0.003);
        assert_eq!(tally.last.as_ref().unwrap().prompt, 1000);
    }

    #[test]
    fn the_prompt_warning_fires_once_per_crossing_and_rearms_below() {
        let warn = Warn {
            prompt_tokens: Some(150_000),
            cost: None,
        };
        let mut tally = Tally::default();
        tally.observe(2, &usage(100_000, 0, 0.0, None, None));
        assert!(tally.warnings(warn).is_empty());
        tally.observe(4, &usage(150_001, 0, 0.0, None, None));
        let first = tally.warnings(warn);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].kind, "prompt_tokens");
        // still over: silent
        tally.observe(6, &usage(160_000, 0, 0.0, None, None));
        assert!(tally.warnings(warn).is_empty());
        // a compaction brings it down, and the next climb speaks again
        tally.observe(8, &usage(20_000, 0, 0.0, None, None));
        assert!(tally.warnings(warn).is_empty());
        tally.observe(10, &usage(151_000, 0, 0.0, None, None));
        assert_eq!(tally.warnings(warn).len(), 1);
    }

    #[test]
    fn the_exact_threshold_is_not_over_it() {
        let warn = Warn {
            prompt_tokens: Some(150_000),
            cost: None,
        };
        let mut tally = Tally::default();
        tally.observe(2, &usage(150_000, 0, 0.0, None, None));
        assert!(tally.warnings(warn).is_empty());
    }

    #[test]
    fn the_cost_warning_fires_once() {
        let warn = Warn {
            prompt_tokens: None,
            cost: Some(0.01),
        };
        let mut tally = Tally::default();
        tally.observe(2, &usage(10, 0, 0.005, Some("provider"), None));
        assert!(tally.warnings(warn).is_empty());
        tally.observe(4, &usage(10, 0, 0.006, Some("provider"), None));
        assert_eq!(tally.warnings(warn).len(), 1);
        tally.observe(6, &usage(10, 0, 0.006, Some("provider"), None));
        assert!(tally.warnings(warn).is_empty());
    }

    #[test]
    fn no_threshold_means_no_warning() {
        let mut tally = Tally::default();
        tally.observe(2, &usage(900_000, 0, 5.0, Some("provider"), None));
        assert!(tally.warnings(Warn::default()).is_empty());
    }

    #[test]
    fn a_resumed_log_does_not_repeat_a_warning_it_already_owed() {
        let warn = Warn {
            prompt_tokens: Some(1000),
            cost: None,
        };
        let events = vec![crate::event::LogEvent::new(
            2,
            usage(5000, 0, 0.0, None, None),
        )];
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
            window: 200_000,
            running,
            retry,
            warn: Warn {
                prompt_tokens: Some(150_000),
                cost: None,
            },
        }
    }

    #[test]
    fn status_is_complete_before_any_request() {
        let tally = Tally::from_events(&[], Warn::default());
        let json = status_json(&tally, &live(None, &[]));
        assert_eq!(json["state"], "idle");
        assert_eq!(json["requests"], 0);
        assert!(json["context"]["last_prompt_tokens"].is_null());
        assert!(json["cache"]["last_hit_ratio"].is_null());
        assert!(json["cost"]["usd"].is_null(), "unknown is not zero");
        assert_eq!(json["cost"]["source"], "unknown");
        assert!(json["last_error"].is_null());
        assert!(json["retry"].is_null());
        let text = render_text(&json);
        assert!(text.contains("no request yet"), "{text}");
        assert!(text.contains("cost      unknown"), "{text}");
    }

    #[test]
    fn status_reports_context_percent_cache_and_cost_after_steps() {
        let mut tally = Tally::default();
        tally.observe(2, &usage(100_000, 75_000, 0.0, Some("table"), None));
        tally.cost = 0.0123;
        let json = status_json(&tally, &live(None, &[]));
        assert_eq!(json["context"]["last_prompt_tokens"], 100_000);
        assert_eq!(json["context"]["percent"], 50.0);
        assert_eq!(json["cache"]["last_hit_ratio"], 0.75);
        assert_eq!(json["cost"]["source"], "table");
        let text = render_text(&json);
        assert!(text.contains("50.0% of 200.0k"), "{text}");
        assert!(text.contains("75% last"), "{text}");
        assert!(text.contains("$0.0123 (table)"), "{text}");
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
        tally.observe(4, &usage(10, 0, 0.0, None, None));
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
