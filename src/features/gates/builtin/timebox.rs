//! The built-in `timebox` gate: a turn a person put a clock on does not end
//! before the clock runs out.
//!
//! The window starts at the turn's opening human message, read from the log's own
//! `ts`, so a resumed session measures from recorded facts and a `say` from a gate
//! cannot move the deadline. The length comes from the prompt ("you have 30
//! minutes") or, when the prompt states none, from the gate's `duration`.

use std::path::Path;
use std::time::Duration;

use super::super::{BuiltinGate, GateAnswer, GateCtx, GateDef, GateInput};
use crate::features::gates::Event;
use crate::log::event::{EventBody, LogEvent};

/// What the model is sent when it stops early. Placeholders: `{elapsed}`,
/// `{target}`, `{left}`.
pub const DEFAULT_STEERING: &str = "You stopped after {elapsed} of a {target} timebox. That is \
     not the answer yet: go deeper. Search a different angle, read the source rather than a \
     summary of it, re-run the check you trust least, and cover the part of the question you \
     have not touched. You have {left} left.";

/// How many times one turn may be sent back before the gate gives up when the
/// config says nothing. Each block is the model trying to stop, so this is room
/// to keep digging; more than it means the gate is wrong, and the budget stops a
/// model that would otherwise spin.
pub const MAX_DENIES: u32 = 10;

/// The words that make a duration in a prompt a budget rather than a fact: "you
/// have 30 minutes" is a clock, "explain the 5 minute rule" is not.
const CUES: &[&str] = &["you have", "you've got", "youve got", "timebox", "within"];

/// The built-in, with the options it was declared with.
pub struct Timebox {
    duration: Option<Duration>,
    only_when_asked: bool,
    steering: String,
    max_denies: u32,
}

impl Timebox {
    pub fn new(def: &GateDef) -> Result<Timebox, String> {
        let duration = match def.options.get("duration") {
            None => None,
            Some(value) => Some(parse_value(value).map_err(|why| {
                format!(
                    "a built-in `timebox` needs `duration` as `90s`, `15m`, `2h`, `1h30m` or an \
                     integer of seconds ({why})"
                )
            })?),
        };
        let only_when_asked = def
            .options
            .get("only_when_asked")
            .and_then(toml::Value::as_bool)
            .unwrap_or(false);
        let steering = def
            .options
            .get("steering")
            .and_then(toml::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| DEFAULT_STEERING.to_string());
        let max_denies = def
            .options
            .get("max_denies")
            .and_then(toml::Value::as_integer)
            .map_or(MAX_DENIES, |n| n.clamp(1, 500) as u32);
        Ok(Timebox {
            duration,
            only_when_asked,
            steering,
            max_denies,
        })
    }

    /// The window this turn gets: what the prompt asked for, else the configured
    /// duration unless the config says to wait for a person to ask.
    fn budget(&self, prompt: &str) -> Option<Duration> {
        let asked = budget_in_prompt(prompt);
        if asked.is_some() || self.only_when_asked {
            return asked;
        }
        self.duration
    }
}

impl BuiltinGate for Timebox {
    fn events(&self) -> &[Event] {
        &[Event::TurnEnd]
    }

    fn run<'a>(
        &'a mut self,
        _input: &'a GateInput,
        ctx: &'a GateCtx<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<GateAnswer, String>> + Send + 'a>>
    {
        Box::pin(async move {
            let Some((started, prompt)) = window_start(ctx.events, ctx.dir) else {
                // Nobody has said anything in this turn: there is no window.
                return Ok(GateAnswer::allow());
            };
            let Some(target) = self.budget(&prompt) else {
                return Ok(GateAnswer::allow());
            };
            let Some(started) = parse_ts(started) else {
                return Ok(GateAnswer::allow());
            };
            let elapsed = (time::OffsetDateTime::now_utc() - started)
                .try_into()
                .unwrap_or(Duration::ZERO);
            if elapsed >= target {
                let mut answer = GateAnswer::allow();
                answer.reason = format!("window closed ({} of {})", human(elapsed), human(target));
                return Ok(answer);
            }
            let text = fill(&self.steering, elapsed, target);
            let mut answer = GateAnswer::deny(text);
            answer.max_denies = self.max_denies;
            Ok(answer)
        })
    }
}

/// The current turn's latest human message, as `(ts, text)`.
///
/// "The current turn" is everything after the last `turn_ended`: a log is one
/// append-only file, and an earlier turn's prompt must not move today's
/// deadline. The latest human message wins, so "keep going, 10 more minutes"
/// restarts the window, which is what a person typing it means.
fn window_start<'a>(events: &'a [LogEvent], dir: &Path) -> Option<(&'a str, String)> {
    let after = events
        .iter()
        .rposition(|event| matches!(event.body, EventBody::TurnEnded { .. }))
        .map_or(0, |index| index + 1);
    events[after..]
        .iter()
        .rev()
        .find_map(|event| match &event.body {
            EventBody::User { text, from, .. } if crate::core::compact::human_typed(from) => {
                Some((
                    event.ts.as_str(),
                    text.resolve_text(dir).unwrap_or_default(),
                ))
            }
            _ => None,
        })
}

/// The steering text with its placeholders filled: `{elapsed}`, `{target}`, `{left}`.
///
/// The braces are not a format string: they are the names a person writes in
/// `steering`, so clippy's format-argument lint does not apply.
#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "the braces are documented placeholder names, not a format string"
)]
fn fill(steering: &str, elapsed: Duration, target: Duration) -> String {
    let left = target.saturating_sub(elapsed);
    let mut text = String::with_capacity(steering.len() + 24);
    let mut rest = steering;
    while let Some(at) = rest.find('{') {
        text.push_str(&rest[..at]);
        let tail = &rest[at..];
        let (word, skip) = if tail.starts_with("{elapsed}") {
            (human(elapsed), "{elapsed}")
        } else if tail.starts_with("{target}") {
            (human(target), "{target}")
        } else if tail.starts_with("{left}") {
            (human(left), "{left}")
        } else {
            text.push('{');
            rest = &tail[1..];
            continue;
        };
        text.push_str(&word);
        rest = &tail[skip.len()..];
    }
    text.push_str(rest);
    text
}

fn parse_ts(text: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()
}

/// A duration as a person writes it.
fn human(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs >= 3600 && secs % 3600 == 0 {
        return format!("{}h", secs / 3600);
    }
    if secs >= 60 {
        return format!("{}m", secs.div_ceil(60));
    }
    format!("{secs}s")
}

/// The budget a prompt states, as a person means it.
///
/// A phrase counts when its unit is the short form (`30m`, `90s`, `2h`), or when
/// one of [`CUES`] comes before it, so "you have 30 minutes" is a clock and
/// "explain the 5 minute rule" is a topic. Several phrases add up (`1h30m`); a
/// bare number with no unit is not a duration and is skipped.
pub fn budget_in_prompt(prompt: &str) -> Option<Duration> {
    let lower = prompt.to_lowercase();
    let mut total = 0u64;
    let mut found = false;
    for (at, number, unit) in quantities(&lower) {
        let short = unit.len() == 1;
        let cued = CUES.iter().any(|cue| lower[..at].contains(cue));
        if !short && !cued {
            continue;
        }
        let seconds = match unit.as_str() {
            "seconds" | "second" | "secs" | "sec" | "s" => number,
            "minutes" | "minute" | "mins" | "min" | "m" => number.saturating_mul(60),
            "hours" | "hour" | "hrs" | "hr" | "h" => number.saturating_mul(3600),
            _ => continue,
        };
        total = total.saturating_add(seconds);
        found = true;
    }
    found
        .then(|| Duration::from_secs(total))
        .filter(|d| !d.is_zero())
}

/// Every `<number> <unit>` in `text`, as `(byte offset, number, unit)`.
fn quantities(text: &str) -> Vec<(usize, u64, String)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if !bytes[at].is_ascii_digit() {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        let Ok(number) = text[start..at].parse::<u64>() else {
            continue;
        };
        let mut unit_at = at;
        while unit_at < bytes.len() && bytes[unit_at] == b' ' {
            unit_at += 1;
        }
        let unit_start = unit_at;
        while unit_at < bytes.len() && bytes[unit_at].is_ascii_alphabetic() {
            unit_at += 1;
        }
        found.push((start, number, text[unit_start..unit_at].to_string()));
        at = if unit_at > at { unit_at } else { at + 1 };
    }
    found
}

/// `duration` from the config: a string as a person writes it, or integer seconds.
fn parse_value(value: &toml::Value) -> Result<Duration, String> {
    match value {
        toml::Value::Integer(seconds) if *seconds > 0 => Ok(Duration::from_secs(*seconds as u64)),
        toml::Value::Integer(_) => Err("a duration is positive".into()),
        toml::Value::String(text) => {
            // A bare config value is a budget by definition: it needs no cue.
            let bare = format!("you have {text}");
            budget_in_prompt(&bare).ok_or_else(|| format!("`{text}` is not a duration"))
        }
        other => Err(format!(
            "{} is not a string or an integer",
            other.type_str()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_budget_is_read_the_way_people_write_it() {
        let cases: [(&str, Option<u64>); 11] = [
            ("research this, you have 30 minutes", Some(1800)),
            ("do x, you have 30 min", Some(1800)),
            ("you've got 1h30m to dig", Some(5400)),
            ("you have 2 hours, no rush", Some(7200)),
            ("timebox: 90s", Some(90)),
            ("1h30m", Some(5400)),
            ("you have 5m", Some(300)),
            // A duration the prompt does not present as a budget.
            ("explain the 5 minute rule", None),
            ("read 30 files", None),
            ("no clock at all", None),
            ("you have 0 minutes", None),
        ];
        for (prompt, want) in cases {
            assert_eq!(
                budget_in_prompt(prompt).map(|d| d.as_secs()),
                want,
                "{prompt:?}"
            );
        }
    }

    #[test]
    fn a_configured_duration_is_a_string_or_seconds() {
        let seconds = parse_value(&toml::Value::Integer(120)).unwrap();
        assert_eq!(seconds, Duration::from_secs(120));
        assert_eq!(
            parse_value(&toml::Value::Integer(0)).unwrap_err(),
            "a duration is positive"
        );
        for (text, want) in [("90s", 90), ("15m", 900), ("2h", 7200), ("1h30m", 5400)] {
            let value = toml::Value::String(text.into());
            let got = parse_value(&value).unwrap();
            assert_eq!(got, Duration::from_secs(want), "{text}");
        }
        let bad = toml::Value::String("soon".into());
        assert!(
            parse_value(&bad)
                .unwrap_err()
                .contains("`soon` is not a duration")
        );
    }

    #[test]
    fn the_gate_takes_the_prompts_budget_over_the_configured_one() {
        let def = GateDef {
            name: "box".into(),
            event: Event::TurnEnd,
            command: String::new(),
            builtin: Some("timebox".into()),
            options: std::iter::once(("duration".to_string(), toml::Value::String("15m".into())))
                .collect(),
            tools: Vec::new(),
            env: Default::default(),
            origin: "config".into(),
        };
        let gate = Timebox::new(&def).expect("a gate");
        assert_eq!(
            gate.budget("you have 30 minutes"),
            Some(Duration::from_secs(1800))
        );
        assert_eq!(gate.budget("just look"), Some(Duration::from_secs(900)));

        // `only_when_asked`: the configured duration is a fallback for nothing,
        // so a prompt with no clock gets no window.
        let mut asked = Timebox::new(&def).expect("a gate");
        asked.only_when_asked = true;
        assert_eq!(
            asked.budget("you have 30 minutes"),
            Some(Duration::from_secs(1800))
        );
        assert_eq!(asked.budget("just look"), None);
    }

    fn log(entries: Vec<(&str, EventBody)>) -> Vec<LogEvent> {
        entries
            .into_iter()
            .enumerate()
            .map(|(index, (ts, body))| LogEvent {
                seq: index as u64 + 1,
                ts: ts.to_string(),
                body,
            })
            .collect()
    }

    fn user(text: &str, from: &str) -> EventBody {
        EventBody::User {
            text: crate::log::event::Payload::text(text),
            from: from.into(),
            delivery: "next".into(),
            attachments: Vec::new(),
        }
    }

    fn ended() -> EventBody {
        EventBody::TurnEnded {
            outcome: "done".into(),
            reason: None,
            final_text: crate::log::event::Payload::text(""),
        }
    }

    #[test]
    fn the_window_starts_at_the_turns_latest_human_message() {
        let events = log(vec![
            ("2026-11-28T10:00:00Z", user("old prompt", "tui")),
            ("2026-11-28T10:05:00Z", ended()),
            ("2026-11-28T10:06:00Z", user("prompt, 30 minutes", "tui")),
            (
                "2026-11-28T10:07:00Z",
                user("[gate box] not accepted", "gate"),
            ),
        ]);
        let (ts, prompt) = window_start(&events, Path::new(".")).expect("a window");
        assert_eq!(ts, "2026-11-28T10:06:00Z");
        assert_eq!(prompt, "prompt, 30 minutes");
    }

    #[test]
    fn a_turn_with_no_human_message_has_no_window() {
        let events = log(vec![
            ("2026-11-28T10:00:00Z", ended()),
            ("2026-11-28T10:01:00Z", user("job wake-up", "job")),
        ]);
        assert!(window_start(&events, Path::new(".")).is_none());
    }

    #[test]
    fn durations_read_back_in_the_unit_a_person_used() {
        assert_eq!(human(Duration::from_secs(7200)), "2h");
        assert_eq!(human(Duration::from_secs(1800)), "30m");
        assert_eq!(human(Duration::from_secs(95)), "2m");
        assert_eq!(human(Duration::from_secs(30)), "30s");
    }
}
