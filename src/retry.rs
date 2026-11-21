//! What to retry, how long to wait, and when to give up.
//! A fully autonomous agent has to ride out transient provider trouble: a `429` when several local
//! workers share one account, a `5xx` from a busy gateway, a socket that dies mid-stream.
//! This module is the whole policy as pure functions: the status classification, `Retry- After`
//! and reset-header parsing, full-jitter exponential backoff (base 2 s, factor 2, cap 120 s) and

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Exponential base: the first wait is drawn from `[0, 2 s)`.
pub const DEFAULT_BASE_MS: u64 = 2_000;
/// The wait never exceeds this, however many attempts there have been.
pub const DEFAULT_CAP_MS: u64 = 120_000;
/// Total budget per request by default: half an hour of waiting.
pub const DEFAULT_WAIT_SECS: u64 = 30 * 60;
/// A parsed provider hint is clamped to this, so an absurd `Retry-After` cannot
/// overflow anything (the budget still decides whether the wait happens).
pub const MAX_PARSED: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub const HINT_FLOOR: Duration = Duration::from_millis(250);

/// How many provider requests one budget allows.
pub const MAX_ATTEMPTS: u32 = 60;

/// The backoff schedule (`retry_base_ms`, `retry_cap_ms`, `retry_max_wait`,
/// `retry_max_attempts` in `config.toml`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub base_ms: u64,
    pub cap_ms: u64,
    /// Total waiting allowed per request. `None` is `retry_max_wait = 0`: wait
    /// as long as the provider needs.
    pub max_wait: Option<Duration>,
    pub max_attempts: Option<u32>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            base_ms: DEFAULT_BASE_MS,
            cap_ms: DEFAULT_CAP_MS,
            max_wait: Some(Duration::from_secs(DEFAULT_WAIT_SECS)),
            max_attempts: None,
        }
    }
}

impl Policy {
    /// The ceiling of the jitter for `attempt` (0-based): `base * 2^n`, capped.
    /// Attempt 0 is the wait after the *first* failure.
    pub fn ceiling(&self, attempt: u32) -> Duration {
        let scaled = self.base_ms.saturating_mul(1u64 << attempt.min(20));
        Duration::from_millis(scaled.min(self.cap_ms.max(1)))
    }
}

/// The random source the jitter draws from.
pub trait Rng {
    fn next_u64(&mut self) -> u64;
}

/// A small xorshift generator: no dependency, deterministic when seeded, and
/// good enough to spread jitter over a handful of local processes.
pub struct Xorshift(u64);

impl Xorshift {
    pub fn new(seed: u64) -> Self {
        // Zero is a fixed point of xorshift; any other seed works.
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }

    /// Seed from the clock and this process, so two kelir processes started
    /// together do not wait in lockstep.
    pub fn from_clock() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos() as u64)
            .unwrap_or(0x5eed);
        Self::new(now ^ (u64::from(std::process::id()) << 17))
    }
}

impl Rng for Xorshift {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// How long to wait before attempt `attempt + 1`.
pub fn delay(policy: &Policy, attempt: u32, hint: Option<Duration>, rng: &mut dyn Rng) -> Duration {
    if let Some(hint) = hint {
        // A hint is what the provider asked for, but never a zero wait: a gateway that answers `Retry-After:
        // 0` (or a reset header already in the past) for ever would otherwise get a request per millisecond
        let floor = policy.ceiling(0).min(HINT_FLOOR);
        return hint.max(floor).min(MAX_PARSED);
    }
    let nanos = policy.ceiling(attempt).as_nanos().min(u128::from(u64::MAX)) as u64;
    if nanos == 0 {
        return Duration::ZERO;
    }
    Duration::from_nanos(rng.next_u64() % (nanos + 1))
}

#[derive(Debug, Clone)]
pub struct Budget {
    total: Option<Duration>,
    spent: Duration,
    attempts: u32,
    cap: u32,
}

impl Budget {
    /// A budget of `total` waiting (`None` = unlimited), with `attempts` as an optional count cap
    /// inside it (`None` = the [`MAX_ATTEMPTS`] safety cap only).
    pub fn new(total: Option<Duration>, attempts: Option<u32>) -> Self {
        Self {
            total,
            spent: Duration::ZERO,
            attempts: 0,
            cap: attempts.unwrap_or(MAX_ATTEMPTS).min(MAX_ATTEMPTS),
        }
    }

    pub fn total(&self) -> Option<Duration> {
        self.total
    }

    pub fn spent(&self) -> Duration {
        self.spent
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    pub fn record_attempt(&mut self) {
        self.attempts = self.attempts.saturating_add(1);
    }

    /// Is there room for one more wait of `wait`? A wait that does not fit is not taken (the turn
    /// pauses instead): waiting 4 s of a 1 s budget is not what the budget says.
    pub fn fits(&self, wait: Duration) -> bool {
        if self.attempts >= self.cap {
            return false;
        }
        match self.total {
            None => true,
            Some(total) => self.spent.saturating_add(wait) <= total,
        }
    }

    /// Charge a wait (called only when [`Budget::fits`] said yes).
    pub fn charge(&mut self, wait: Duration) {
        self.spent = self.spent.saturating_add(wait);
    }
}

/// The HTTP statuses worth another request: 429, 408 (request timeout) and every 5xx, which covers 529
/// ("overloaded"). 400/401/403/404 are answers, not outages: retrying them buys the same answer again.
pub fn status_is_retryable(code: u16) -> bool {
    matches!(code, 408 | 429) || (500..=599).contains(&code)
}

/// A short reason for the screen and the log; never the response body, which
/// can echo a credential.
pub fn status_reason(code: u16) -> &'static str {
    match code {
        429 => "rate limited",
        408 => "request timeout",
        529 => "provider overloaded",
        500..=599 => "provider error",
        _ => "provider refused",
    }
}

/// "34 s", "900 ms", "1.5 min": a wait a person reads.
pub fn seconds_text(wait: Duration) -> String {
    let ms = wait.as_millis();
    if ms < 1_000 {
        return format!("{ms} ms");
    }
    let secs = wait.as_secs_f64();
    if secs < 60.0 {
        format!("{} s", secs.round() as u64)
    } else if secs < 3_600.0 {
        let minutes = secs / 60.0;
        if (minutes - minutes.round()).abs() < 0.05 {
            format!("{} min", minutes.round() as u64)
        } else {
            format!("{minutes:.1} min")
        }
    } else {
        format!("{:.1} h", secs / 3_600.0)
    }
}

/// The visible line for a wait: "rate limited (429): retrying in 34 s, attempt
/// 5, say stop to cancel".
pub fn wait_line(status: Option<u16>, wait: Duration, attempt: u32) -> String {
    let reason = status.map(status_reason).unwrap_or("provider error");
    let seconds = seconds_text(wait);
    match status {
        Some(code) => format!(
            "{reason} ({code}): retrying in {seconds}, attempt {attempt}, say stop to cancel"
        ),
        None => format!("{reason}: retrying in {seconds}, attempt {attempt}, say stop to cancel"),
    }
}

// ------------------------------------------------------------------ parsing

/// `Retry-After`: delta seconds, or an HTTP-date. Garbage is `None`, and the
/// exponential schedule takes over.
pub fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds).min(MAX_PARSED));
    }
    absolute(value, now)
}

/// One reset header (`x-ratelimit-reset*`, `anthropic-ratelimit-*-reset`, …).
pub fn parse_reset(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(number) = value.parse::<f64>() {
        if !number.is_finite() || number < 0.0 {
            return None;
        }
        // Epoch milliseconds or epoch seconds when the number is too large to
        // be a delta; anything smaller is a delta in seconds.
        let epoch = if number >= 1e12 {
            Some(Duration::from_millis(number as u64))
        } else if number >= 1e9 {
            Some(Duration::from_secs(number as u64))
        } else {
            None
        };
        if let Some(since_epoch) = epoch {
            return Some(until(since_epoch, now));
        }
        return Some(Duration::from_secs_f64(number).min(MAX_PARSED));
    }
    if let Some(wait) = parse_duration_spec(value) {
        return Some(wait.min(MAX_PARSED));
    }
    absolute(value, now)
}

fn until(since_epoch: Duration, now: SystemTime) -> Duration {
    let now = now.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    since_epoch.saturating_sub(now).min(MAX_PARSED)
}

/// A date or a timestamp, as a wait from `now`: HTTP-date or RFC 3339. A date
/// in the past is `Some(0)`, never a negative wait.
fn absolute(text: &str, now: SystemTime) -> Option<Duration> {
    let parsed = http_date(text).or_else(|| rfc3339(text))?;
    Some(
        parsed
            .duration_since(now)
            .unwrap_or(Duration::ZERO)
            .min(MAX_PARSED),
    )
}

/// Go-style durations ("1s", "6m0s", "120ms", "1h30m"): the shape the Anthropic and OpenAI reset
/// headers use.
pub fn parse_duration_spec(text: &str) -> Option<Duration> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let bytes = text.as_bytes();
    let mut index = 0usize;
    let mut total = 0f64;
    let mut any = false;
    while index < bytes.len() {
        let start = index;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
            index += 1;
        }
        if start == index {
            return None;
        }
        let number: f64 = text[start..index].parse().ok()?;
        if !number.is_finite() || number < 0.0 {
            return None;
        }
        let unit_start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
            index += 1;
        }
        let unit = &text[unit_start..index];
        let seconds = match unit {
            "ns" => number / 1e9,
            "us" | "\u{b5}s" => number / 1e6,
            "ms" => number / 1e3,
            "s" | "" => number,
            "m" => number * 60.0,
            "h" => number * 3_600.0,
            _ => return None,
        };
        total += seconds;
        any = true;
    }
    any.then(|| Duration::from_secs_f64(total).min(MAX_PARSED))
}

/// The wait one response's headers ask for: `Retry-After` when it is there and not zero,
/// otherwise the *shortest* reset header.
pub fn header_hint<'a>(
    headers: impl Iterator<Item = (&'a str, &'a str)>,
    now: SystemTime,
) -> Option<Duration> {
    let headers: Vec<(String, String)> = headers
        .map(|(name, value)| (name.to_ascii_lowercase(), value.to_string()))
        .collect();
    let now = headers
        .iter()
        .find(|(name, _)| name == "date")
        .and_then(|(_, value)| http_date(value))
        .unwrap_or(now);
    let mut retry_after = None;
    let mut reset: Option<Duration> = None;
    for (name, value) in &headers {
        if name == "retry-after" {
            retry_after = retry_after.or_else(|| parse_retry_after(value, now));
        } else if is_reset_header(name)
            && let Some(wait) = parse_reset(value, now)
            // An already-open window ("0s", a past date) says nothing about
            // when to come back; a window that is still closed is the answer.
            && !wait.is_zero()
        {
            reset = Some(reset.map_or(wait, |current: Duration| current.min(wait)));
        }
    }
    // A `Retry-After: 0` (or a date already past) says "retry now"; when the same response also
    // names a real reset window, the window is the better answer.
    match (retry_after, reset) {
        (Some(hint), Some(window)) if hint.is_zero() => Some(window),
        (Some(hint), _) => Some(hint),
        (None, reset) => reset,
    }
}

fn is_reset_header(name: &str) -> bool {
    if name == "ratelimit-reset" {
        return true;
    }
    if let Some(rest) = name.strip_prefix("x-ratelimit-reset") {
        return rest.is_empty() || rest.starts_with('-');
    }
    name.starts_with("anthropic-ratelimit-")
        && (name.ends_with("-reset") || name.ends_with("-reset-at"))
}

/// HTTP-date (RFC 7231): IMF-fixdate `Sun, 06 Nov 1994 08:49:37 GMT`, and the
/// obsolete asctime form `Sun Nov 6 08:49:37 1994`.
fn http_date(text: &str) -> Option<SystemTime> {
    let text = text.trim();
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    let (day, month, year, time) = if parts[0].ends_with(',') {
        // "Sun, 06 Nov 1994 08:49:37 GMT"
        if parts.len() < 6 || !parts[5].eq_ignore_ascii_case("GMT") {
            return None;
        }
        (parts[1], parts[2], parts[3], parts[4])
    } else {
        // asctime: "Sun Nov 6 08:49:37 1994"
        if parts.len() < 5 {
            return None;
        }
        (parts[2], parts[1], parts[4], parts[3])
    };
    // "06" -> 6; a day of all zeros parses to 0 and `from_calendar_date`
    // rejects it like any other out-of-range day.
    let day: u32 = day.trim_start_matches('0').parse().unwrap_or(0);
    let month = month_number(month)?;
    let year: i32 = year.parse().ok()?;
    let (hour, minute, second) = clock(time)?;
    let date = time::Date::from_calendar_date(year, month, day as u8).ok()?;
    let time = time::Time::from_hms(hour, minute, second).ok()?;
    let at = time::PrimitiveDateTime::new(date, time).assume_utc();
    Some(UNIX_EPOCH + Duration::new(at.unix_timestamp().max(0) as u64, 0))
}

/// RFC 3339 (`2023-11-14T22:14:50Z`, `2023-11-14T22:14:50+00:00`).
fn rfc3339(text: &str) -> Option<SystemTime> {
    let at =
        time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()?;
    Some(UNIX_EPOCH + Duration::new(at.unix_timestamp().max(0) as u64, 0))
}

fn month_number(name: &str) -> Option<time::Month> {
    use time::Month;
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        "jan" => Month::January,
        "feb" => Month::February,
        "mar" => Month::March,
        "apr" => Month::April,
        "may" => Month::May,
        "jun" => Month::June,
        "jul" => Month::July,
        "aug" => Month::August,
        "sep" => Month::September,
        "oct" => Month::October,
        "nov" => Month::November,
        "dec" => Month::December,
        _ => return None,
    })
}

/// "HH:MM:SS".
fn clock(text: &str) -> Option<(u8, u8, u8)> {
    let mut parts = text.split(':');
    let hour = parts.next()?.parse().ok()?;
    let minute = parts.next()?.parse().ok()?;
    let second = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((hour, minute, second))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed "now": 2023-11-14T22:13:20Z.
    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    struct Fixed(u64);

    impl Rng for Fixed {
        fn next_u64(&mut self) -> u64 {
            self.0
        }
    }

    #[test]
    fn the_classification_table_matches_the_brief() {
        for code in [408, 429, 500, 502, 503, 504, 529, 599] {
            assert!(status_is_retryable(code), "{code} must be retried");
        }
        for code in [200, 201, 400, 401, 403, 404, 422, 600] {
            assert!(!status_is_retryable(code), "{code} must not be retried");
        }
        assert_eq!(status_reason(429), "rate limited");
        assert_eq!(status_reason(529), "provider overloaded");
        assert_eq!(status_reason(503), "provider error");
    }

    #[test]
    fn backoff_is_exponential_then_capped() {
        let policy = Policy {
            base_ms: 2_000,
            cap_ms: 120_000,
            max_wait: None,
            max_attempts: None,
        };
        assert_eq!(policy.ceiling(0), Duration::from_millis(2_000));
        assert_eq!(policy.ceiling(1), Duration::from_millis(4_000));
        assert_eq!(policy.ceiling(2), Duration::from_millis(8_000));
        assert_eq!(policy.ceiling(5), Duration::from_millis(64_000));
        assert_eq!(policy.ceiling(6), Duration::from_millis(120_000));
        assert_eq!(policy.ceiling(100), Duration::from_millis(120_000));
        // A zero base is a zero schedule (no waiting), never a panic.
        let fast = Policy {
            base_ms: 0,
            cap_ms: 0,
            max_wait: None,
            max_attempts: None,
        };
        assert_eq!(fast.ceiling(3), Duration::ZERO);
    }

    #[test]
    fn full_jitter_stays_inside_the_ceiling_and_spreads() {
        let policy = Policy {
            base_ms: 2_000,
            cap_ms: 8_000,
            max_wait: None,
            max_attempts: None,
        };
        let mut rng = Xorshift::new(7);
        let mut low = 0;
        let mut high = 0;
        for attempt in 0..6u32 {
            let ceiling = policy.ceiling(attempt).as_nanos() as u64;
            let mut sum = 0u64;
            for _ in 0..400 {
                let wait = delay(&policy, attempt, None, &mut rng);
                assert!(wait <= policy.ceiling(attempt), "{wait:?}");
                let nanos = wait.as_nanos() as u64;
                sum += nanos;
                if nanos < ceiling / 2 {
                    low += 1;
                } else {
                    high += 1;
                }
            }
            let mean = sum / 400;
            // Uniform in [0, ceiling]: the mean is near half of it.
            assert!(
                mean > ceiling / 4 && mean < ceiling * 3 / 4,
                "attempt {attempt}: mean {mean} of ceiling {ceiling}"
            );
        }
        assert!(low > 0 && high > 0, "the jitter must reach both halves");
    }

    #[test]
    fn the_jitter_reaches_both_ends() {
        let policy = Policy::default();
        assert_eq!(delay(&policy, 0, None, &mut Fixed(0)), Duration::ZERO);
        let ceiling = policy.ceiling(0).as_nanos() as u64;
        let top = delay(&policy, 0, None, &mut Fixed(ceiling));
        assert_eq!(top.as_nanos() as u64, ceiling);
    }

    #[test]
    fn a_provider_hint_wins_over_the_schedule() {
        let policy = Policy::default();
        let hint = Duration::from_secs(34);
        assert_eq!(delay(&policy, 0, Some(hint), &mut Fixed(1)), hint);
        // Even a hint past the cap is honoured: the provider asked for it.
        let long = Duration::from_secs(600);
        assert_eq!(delay(&policy, 9, Some(long), &mut Fixed(1)), long);
    }

    #[test]
    fn retry_after_seconds_dates_and_garbage() {
        assert_eq!(
            parse_retry_after("120", now()),
            Some(Duration::from_secs(120))
        );
        assert_eq!(parse_retry_after("0", now()), Some(Duration::ZERO));
        assert_eq!(
            parse_retry_after("  1 ", now()),
            Some(Duration::from_secs(1))
        );
        // A date 90 s in the future.
        let date = "Tue, 14 Nov 2023 22:14:50 GMT";
        assert_eq!(
            parse_retry_after(date, now()),
            Some(Duration::from_secs(90))
        );
        // A date in the past means "now", never a negative wait.
        let past = "Tue, 14 Nov 2023 22:00:00 GMT";
        assert_eq!(parse_retry_after(past, now()), Some(Duration::ZERO));
        // Garbage is not a hint; the exponential schedule takes over.
        assert_eq!(parse_retry_after("soon", now()), None);
        assert_eq!(parse_retry_after("", now()), None);
        assert_eq!(parse_retry_after("-5", now()), None);
        // Huge values are clamped instead of overflowing.
        assert_eq!(parse_retry_after("999999999999", now()), Some(MAX_PARSED));
        // The obsolete asctime form parses too.
        assert_eq!(
            parse_retry_after("Tue Nov 14 22:14:50 2023", now()),
            Some(Duration::from_secs(90))
        );
    }

    #[test]
    fn reset_headers_in_every_shape_providers_write() {
        assert_eq!(parse_reset("1s", now()), Some(Duration::from_secs(1)));
        assert_eq!(parse_reset("6m0s", now()), Some(Duration::from_secs(360)));
        assert_eq!(
            parse_reset("120ms", now()),
            Some(Duration::from_millis(120))
        );
        assert_eq!(parse_reset("1m30s", now()), Some(Duration::from_secs(90)));
        assert_eq!(parse_reset("12", now()), Some(Duration::from_secs(12)));
        // Epoch seconds and milliseconds.
        assert_eq!(
            parse_reset("1700000060", now()),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            parse_reset("1700000060000", now()),
            Some(Duration::from_secs(60))
        );
        // A timestamp: HTTP-date and RFC 3339.
        assert_eq!(
            parse_reset("Tue, 14 Nov 2023 22:14:50 GMT", now()),
            Some(Duration::from_secs(90))
        );
        assert_eq!(
            parse_reset("2023-11-14T22:14:50Z", now()),
            Some(Duration::from_secs(90))
        );
        assert_eq!(parse_reset("nonsense", now()), None);
        assert_eq!(parse_reset("", now()), None);
    }

    #[test]
    fn an_absolute_hint_is_measured_from_the_responses_own_date() {
        // 22:13:20 is `now()`; the response says it was written at 22:13:50 and the provider wants the caller
        // back at 22:15:20, so the wait is 90 s whatever this machine's clock believes.
        let headers = [
            ("Date", "Tue, 14 Nov 2023 22:13:50 GMT"),
            ("Retry-After", "Tue, 14 Nov 2023 22:15:20 GMT"),
        ];
        assert_eq!(
            header_hint(headers.into_iter(), now()),
            Some(Duration::from_secs(90)),
            "the response's own Date is the clock for its absolute hints"
        );
        // A reset header written as an epoch is measured the same way.
        let epoch = [
            ("date", "Tue, 14 Nov 2023 22:13:50 GMT"),
            ("x-ratelimit-reset-requests", "1700000120"),
        ];
        assert_eq!(
            header_hint(epoch.into_iter(), now()),
            Some(Duration::from_secs(90))
        );
        // No Date header: the local clock, as before.
        let no_date = [("Retry-After", "Tue, 14 Nov 2023 22:14:50 GMT")];
        assert_eq!(
            header_hint(no_date.into_iter(), now()),
            Some(Duration::from_secs(90))
        );
    }

    #[test]
    fn a_hint_is_never_shorter_than_the_floor() {
        let policy = Policy {
            base_ms: 2_000,
            cap_ms: 120_000,
            max_wait: None,
            max_attempts: None,
        };
        // `Retry-After: 0` from a gateway that never stops saying it .
        assert_eq!(
            delay(&policy, 0, Some(Duration::ZERO), &mut Fixed(0)),
            HINT_FLOOR
        );
        // A past date is the same answer.
        assert_eq!(
            delay(&policy, 3, Some(Duration::ZERO), &mut Fixed(0)),
            HINT_FLOOR
        );
        // A fast configured schedule keeps its own (shorter) floor, so a test
        // or a person tuning the schedule is not overruled.
        let fast = Policy {
            base_ms: 20,
            cap_ms: 40,
            max_wait: None,
            max_attempts: None,
        };
        assert_eq!(
            delay(&fast, 0, Some(Duration::ZERO), &mut Fixed(0)),
            Duration::from_millis(20)
        );
        // A real hint is honoured as it is.
        assert_eq!(
            delay(&policy, 0, Some(Duration::from_secs(3)), &mut Fixed(0)),
            Duration::from_secs(3)
        );
        // A tiny but non-zero hint is lifted to the floor too.
        assert_eq!(
            delay(&policy, 0, Some(Duration::from_millis(1)), &mut Fixed(0)),
            HINT_FLOOR
        );
    }

    #[test]
    fn the_headers_are_read_in_priority_order() {
        let headers = [
            ("x-ratelimit-reset-requests", "0s"),
            ("x-ratelimit-reset-tokens", "2m"),
            ("x-ratelimit-reset", "30s"),
        ];
        assert_eq!(
            header_hint(headers.into_iter(), now()),
            Some(Duration::from_secs(30)),
            "the shortest window: the tight one is the one that fired"
        );
        // A zero `Retry-After` beside a real window: the window is the answer.
        let zero_after = [("Retry-After", "0"), ("x-ratelimit-reset-requests", "2m")];
        assert_eq!(
            header_hint(zero_after.into_iter(), now()),
            Some(Duration::from_secs(120))
        );
        let with_retry_after = [("Retry-After", "3"), ("x-ratelimit-reset-requests", "2m")];
        assert_eq!(
            header_hint(with_retry_after.into_iter(), now()),
            Some(Duration::from_secs(3)),
            "Retry-After wins"
        );
        let anthropic = [
            ("anthropic-ratelimit-tokens-reset", "45s"),
            ("anthropic-ratelimit-requests-reset", "10s"),
        ];
        assert_eq!(
            header_hint(anthropic.into_iter(), now()),
            Some(Duration::from_secs(10)),
            "the shorter of the two windows"
        );
        assert_eq!(header_hint(std::iter::empty(), now()), None);
    }

    #[test]
    fn the_budget_stops_the_waits_it_cannot_afford() {
        let mut budget = Budget::new(Some(Duration::from_secs(10)), None);
        assert!(budget.fits(Duration::from_secs(6)));
        budget.charge(Duration::from_secs(6));
        assert_eq!(budget.spent(), Duration::from_secs(6));
        assert!(budget.fits(Duration::from_secs(4)));
        assert!(!budget.fits(Duration::from_secs(5)));
        budget.charge(Duration::from_secs(4));
        assert!(
            budget.fits(Duration::ZERO),
            "an exactly spent budget still allows a free wait"
        );
        assert!(!budget.fits(Duration::from_millis(1)));
    }

    #[test]
    fn an_unlimited_budget_never_runs_out() {
        let mut budget = Budget::new(None, None);
        for _ in 0..50 {
            assert!(budget.fits(Duration::from_secs(3_600)));
            budget.charge(Duration::from_secs(3_600));
        }
        assert_eq!(budget.total(), None);
    }

    #[test]
    fn attempts_are_counted_and_bounded() {
        let mut budget = Budget::new(None, None);
        for _ in 0..MAX_ATTEMPTS {
            budget.record_attempt();
        }
        assert_eq!(budget.attempts(), MAX_ATTEMPTS);
        assert!(!budget.fits(Duration::ZERO), "a zero-wait loop must end");
        // A configured count cap is honoured inside an unlimited budget.
        let mut three = Budget::new(None, Some(3));
        for _ in 0..3 {
            assert!(three.fits(Duration::from_secs(1)));
            three.record_attempt();
        }
        assert!(!three.fits(Duration::from_secs(1)));
        assert_eq!(three.attempts(), 3);
    }

    #[test]
    fn the_wait_line_reads_like_the_brief() {
        assert_eq!(
            wait_line(Some(429), Duration::from_secs(34), 5),
            "rate limited (429): retrying in 34 s, attempt 5, say stop to cancel"
        );
        assert!(wait_line(None, Duration::from_millis(250), 2).contains("250 ms"));
        assert_eq!(seconds_text(Duration::from_millis(900)), "900 ms");
        assert_eq!(seconds_text(Duration::from_secs(59)), "59 s");
        assert_eq!(seconds_text(Duration::from_secs(120)), "2 min");
        assert_eq!(seconds_text(Duration::from_secs(90)), "1.5 min");
        assert_eq!(seconds_text(Duration::from_secs(3_600)), "1.0 h");
    }

    #[test]
    fn the_seed_is_never_zero_and_still_moves() {
        let mut rng = Xorshift::new(0);
        let first = rng.next_u64();
        let second = rng.next_u64();
        assert_ne!(first, 0);
        assert_ne!(first, second);
        let mut from_clock = Xorshift::from_clock();
        assert_ne!(from_clock.next_u64(), from_clock.next_u64());
    }

    #[test]
    fn a_duration_spec_needs_a_unit_for_no_reason_but_accepts_bare_seconds() {
        assert_eq!(parse_duration_spec("2"), Some(Duration::from_secs(2)));
        assert_eq!(
            parse_duration_spec("1.5s"),
            Some(Duration::from_millis(1_500))
        );
        assert_eq!(
            parse_duration_spec("1h30m"),
            Some(Duration::from_secs(5_400))
        );
        assert_eq!(parse_duration_spec("nope"), None);
        assert_eq!(parse_duration_spec("-1s"), None);
    }
}
