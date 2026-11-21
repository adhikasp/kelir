//! Jobs (M9): the data side. `jobs.toml`, durations, the `new_session_every`
//! rule, the date-change source, STATUS and the run history, and the plan the
//! session loop reads. The orchestration (lock, run, finalize, notify) is in
//! [`crate::jobrun`].
//!
//! kelir has no scheduler: `kelir job run <name>` is what a scheduler calls.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::event::{EventBody, LogEvent};

// ------------------------------------------------------------------ the plan

/// One `source` event recorded before the run's task.
#[derive(Debug, Clone)]
pub struct PlanSource {
    pub kind: String,
    pub name: String,
    pub text: String,
}

/// What the session loop needs to know about a named or job run. Installed once,
/// before the loop starts; the loop only reads it. Everything else about a job
/// lives in [`crate::jobrun`].
#[derive(Debug, Default)]
pub struct Plan {
    /// The id a NEW session must take (the name is already bound to it).
    pub planned_id: Option<String>,
    /// What the run's task message says it came from (`job`), when it is not a
    /// person typing it.
    pub task_from: Option<&'static str>,
    /// `task_from` also applies to the first task of a NEW session (a job's);
    /// otherwise only a resumed session's task is marked (`run --name`).
    pub on_new: bool,
    /// Sources recorded before the task, in order.
    pub sources: Vec<PlanSource>,
    /// The wall-clock cap of the run, if any.
    pub deadline: Option<Duration>,
    /// A job's declared `max_steps`: reaching it pauses the run even when the
    /// last allowed step produced a final answer (a scheduler resumes the job
    /// next run). Interactive `--max-steps` keeps STEER-4's rule that an answer
    /// on the cap-th step is `done`.
    pub job_step_cap: Option<u32>,
    /// The deadline fired: the turn was told to stop because of it.
    pub timed_out: AtomicBool,
    /// An error the loop could not put in a log (no session existed yet).
    pub error: std::sync::Mutex<Option<String>>,
}

impl Plan {
    /// Where the run's task message says it came from, if not the command line.
    pub fn task_from_for(&self, resumed: bool) -> Option<&'static str> {
        if CHANNEL_MODE.load(std::sync::atomic::Ordering::SeqCst) {
            return Some("channel");
        }
        self.task_from.filter(|_| resumed || self.on_new)
    }
}

static PLAN: OnceLock<Plan> = OnceLock::new();

/// Channel mode: the task of every session in this process comes from a remote
/// chat user, not a job. Set once by `kelir channel`; a CLI run never sets it.
static CHANNEL_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this process is `kelir channel`.
pub fn channel_mode() -> bool {
    CHANNEL_MODE.load(std::sync::atomic::Ordering::SeqCst)
}

pub fn set_channel_mode() {
    CHANNEL_MODE.store(true, std::sync::atomic::Ordering::SeqCst);
}

tokio::task_local! {
    /// The id of the session a channel turn is starting, scoped to that turn's
    /// task so concurrent conversations cannot take each other's id.
    static NEXT_ID: std::cell::RefCell<Option<String>>;
}

/// Run `future` (a session start) with `id` as the id `planned_id` hands out.
pub async fn with_next_id<F: std::future::Future>(id: String, future: F) -> F::Output {
    NEXT_ID
        .scope(std::cell::RefCell::new(Some(id)), future)
        .await
}

/// Install the plan. `false` if one was installed already (one run per process).
pub fn install(plan: Plan) -> bool {
    PLAN.set(plan).is_ok()
}

pub fn plan() -> Option<&'static Plan> {
    PLAN.get()
}

/// The id a new session must take, when a name decided it.
pub fn planned_id() -> Option<String> {
    if let Some(id) = plan().and_then(|plan| plan.planned_id.clone()) {
        return Some(id);
    }
    NEXT_ID
        .try_with(|slot| slot.borrow_mut().take())
        .ok()
        .flatten()
}

/// The run was stopped by its deadline.
pub fn timed_out() -> bool {
    plan().is_some_and(|plan| plan.timed_out.load(Ordering::SeqCst))
}

/// The loop failed before it had a log to say so in.
pub fn note_error(message: &str) {
    if let Some(plan) = plan()
        && let Ok(mut slot) = plan.error.lock()
    {
        slot.get_or_insert_with(|| message.chars().take(500).collect());
    }
}

// ---------------------------------------------------------------- durations

/// `90`, `90s`, `30m`, `12h`, `7d`, `2w`: a length of time. A bare number is
/// seconds.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, unit) = text.split_at(split);
    let count: u64 = digits
        .parse()
        .map_err(|_| format!("`{text}` is not a duration (try 90s, 30m, 12h, 7d)"))?;
    let seconds = match unit.trim() {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        other => {
            return Err(format!(
                "`{text}`: unknown unit `{other}` (use s, m, h, d or w)"
            ));
        }
    };
    count
        .checked_mul(seconds)
        .map(Duration::from_secs)
        .ok_or_else(|| format!("`{text}` is too long"))
}

// ------------------------------------------------------------- the job file

/// How a job's session is renewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotate {
    /// Never start a fresh session; compaction keeps the context bounded.
    Never,
    /// Start a fresh session when the old one is this old, or its context is
    /// large.
    Every(Duration),
}

impl Rotate {
    pub fn parse(text: &str) -> Result<Rotate, String> {
        if text.trim().eq_ignore_ascii_case("never") {
            return Ok(Rotate::Never);
        }
        let every = parse_duration(text).map_err(|why| format!("new_session_every: {why}"))?;
        if every.is_zero() {
            return Err(
                "new_session_every: a zero duration would start a session every run \
                        (use a fresh `session` name, or `never`)"
                    .into(),
            );
        }
        Ok(Rotate::Every(every))
    }
}

/// Where a job was declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Machine,
    Project,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawJob {
    task: Option<String>,
    task_file: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    max_steps: Option<u32>,
    timeout: Option<toml::Value>,
    session: Option<String>,
    new_session_every: Option<String>,
    notify: Option<OneOrMany>,
    schedule: Option<String>,
    cwd: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    jobs: BTreeMap<String, RawJob>,
}

/// A job, validated.
#[derive(Debug, Clone)]
pub struct JobDef {
    pub name: String,
    /// The task text, or the file it is read from at run time.
    pub task: Option<String>,
    pub task_file: Option<PathBuf>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub max_steps: Option<u32>,
    pub timeout: Option<Duration>,
    /// The named session every run resumes (default: the job's name).
    pub session: String,
    pub rotate: Rotate,
    pub notify: Vec<String>,
    /// For humans: kelir does not schedule.
    pub schedule: Option<String>,
    pub cwd: Option<PathBuf>,
    pub origin: Origin,
    pub file: PathBuf,
}

impl JobDef {
    /// The task text for this run.
    pub fn task_text(&self) -> Result<String, String> {
        if let Some(task) = &self.task {
            return Ok(task.clone());
        }
        let Some(path) = &self.task_file else {
            return Err(format!("job `{}` has no task", self.name));
        };
        let text = std::fs::read_to_string(path).map_err(|error| {
            format!(
                "job `{}`: cannot read task_file {}: {error}",
                self.name,
                crate::util::path_string(path)
            )
        })?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(format!(
                "job `{}`: task_file {} is empty",
                self.name,
                crate::util::path_string(path)
            ));
        }
        Ok(text)
    }
}

fn validate(name: &str, raw: RawJob, file: &Path, origin: Origin) -> Result<JobDef, String> {
    let at = |why: String| format!("{}: [jobs.{name}] {why}", crate::util::path_string(file));
    crate::names::validate(name).map_err(at)?;
    match (&raw.task, &raw.task_file) {
        (Some(_), Some(_)) => return Err(at("give `task` or `task_file`, not both".into())),
        (None, None) => return Err(at("needs `task` or `task_file`".into())),
        (Some(task), None) if task.trim().is_empty() => return Err(at("`task` is empty".into())),
        _ => {}
    }
    let timeout = match &raw.timeout {
        None => None,
        Some(toml::Value::String(text)) => Some(parse_duration(text).map_err(at)?),
        Some(toml::Value::Integer(seconds)) if *seconds >= 0 => {
            Some(Duration::from_secs(*seconds as u64))
        }
        Some(_) => {
            return Err(at(
                "`timeout` is a duration such as \"30m\" or a number of seconds".into(),
            ));
        }
    }
    .filter(|timeout| !timeout.is_zero());
    let session = raw.session.clone().unwrap_or_else(|| name.to_string());
    crate::names::validate(&session).map_err(|why| at(format!("session: {why}")))?;
    let rotate = match &raw.new_session_every {
        Some(text) => Rotate::parse(text).map_err(at)?,
        None => Rotate::Never,
    };
    let base = file.parent().unwrap_or(Path::new("."));
    let notify = match raw.notify {
        None => Vec::new(),
        Some(OneOrMany::One(one)) => vec![one],
        Some(OneOrMany::Many(many)) => many,
    };
    if notify.iter().any(|command| command.trim().is_empty()) {
        return Err(at("`notify` holds an empty command".into()));
    }
    Ok(JobDef {
        name: name.to_string(),
        task: raw.task,
        task_file: raw.task_file.map(|path| {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                base.join(path)
            }
        }),
        provider: raw.provider,
        model: raw.model,
        max_steps: raw.max_steps,
        timeout,
        session,
        rotate,
        notify,
        schedule: raw.schedule,
        cwd: raw.cwd.map(|path| {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                base.join(path)
            }
        }),
        origin,
        file: file.to_path_buf(),
    })
}

/// Parse one `jobs.toml`.
pub fn parse_file(text: &str, file: &Path, origin: Origin) -> Result<Vec<JobDef>, String> {
    let raw: RawFile = toml::from_str(text)
        .map_err(|error| format!("{}: {error}", crate::util::path_string(file)))?;
    raw.jobs
        .into_iter()
        .map(|(name, job)| validate(&name, job, file, origin))
        .collect()
}

/// The nearest `.kelir/jobs.toml`, walking up from `cwd`.
pub fn project_file(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .map(|dir| dir.join(".kelir").join("jobs.toml"))
        .find(|candidate| candidate.is_file())
}

/// The machine `jobs.toml`, then the project's: a job declared in both is the
/// project's.
pub fn load(cwd: &Path) -> Result<BTreeMap<String, JobDef>, String> {
    let mut jobs = BTreeMap::new();
    let machine = crate::paths::config_dir().join("jobs.toml");
    let project = project_file(cwd);
    for (file, origin) in [(Some(machine), Origin::Machine), (project, Origin::Project)] {
        let Some(file) = file else { continue };
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "cannot read {}: {error}",
                    crate::util::path_string(&file)
                ));
            }
        };
        for job in parse_file(&text, &file, origin)? {
            jobs.insert(job.name.clone(), job);
        }
    }
    Ok(jobs)
}

// ------------------------------------------------------------------- places

pub fn jobs_dir() -> PathBuf {
    crate::paths::state_dir().join("jobs")
}

pub fn job_dir(name: &str) -> PathBuf {
    jobs_dir().join(name)
}

// ------------------------------------------------------------- rotation rule

/// The share of the window past which a context counts as large.
pub const LARGE_CONTEXT: f64 = 0.9;

/// Why a fresh session starts instead of resuming the old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotateWhy {
    Age,
    Context,
}

/// Is it time for a fresh session? Pure: age of the session, the prompt size of
/// its last request, and the window that size is measured against.
pub fn should_rotate(
    rule: Rotate,
    age: Duration,
    last_prompt: Option<u64>,
    window: u64,
) -> Option<RotateWhy> {
    let Rotate::Every(every) = rule else {
        return None;
    };
    if age >= every {
        return Some(RotateWhy::Age);
    }
    match last_prompt {
        Some(prompt) if window > 0 && prompt as f64 >= window as f64 * LARGE_CONTEXT => {
            Some(RotateWhy::Context)
        }
        _ => None,
    }
}

/// How long ago a log began, from its first event's timestamp.
pub fn age_of(events: &[LogEvent], now: time::OffsetDateTime) -> Duration {
    events
        .first()
        .and_then(|event| {
            time::OffsetDateTime::parse(&event.ts, &time::format_description::well_known::Rfc3339)
                .ok()
        })
        .map(|start| {
            let seconds = (now - start).whole_seconds().max(0) as u64;
            Duration::from_secs(seconds)
        })
        .unwrap_or_default()
}

/// What an old session leaves for its successor: the last compaction summary
/// when it has one, else the last answer.
pub fn carry_text(events: &[LogEvent], dir: &Path) -> Option<String> {
    let summary = events.iter().rev().find_map(|event| match &event.body {
        EventBody::Compacted { summary, .. } => summary.resolve_text(dir).ok(),
        _ => None,
    });
    let text = summary.filter(|text| !text.trim().is_empty()).or_else(|| {
        events.iter().rev().find_map(|event| match &event.body {
            EventBody::TurnEnded { final_text, .. } => final_text
                .resolve_text(dir)
                .ok()
                .filter(|text| !text.trim().is_empty()),
            _ => None,
        })
    })?;
    Some(crate::util::truncate_chars(&text, 6000))
}

// ------------------------------------------------------------ the date source

/// The date the context last heard: the newest `clock` source, else the day the
/// session started.
pub fn last_announced_date(events: &[LogEvent]) -> Option<String> {
    last_announced_date_from(events, None)
}

/// [`last_announced_date`], with the date the session's frozen system prompt
/// was rendered with when it is known. The prompt is what the context actually
/// saw; the event's `ts` is wall-clock time and can disagree when `KELIR_TODAY`
/// overrode the prompt (STEER-9). Old logs with no resolvable prompt keep the
/// timestamp fallback.
pub fn last_announced_date_from(
    events: &[LogEvent],
    rendered_date: Option<&str>,
) -> Option<String> {
    let clock = events.iter().rev().find_map(|event| match &event.body {
        EventBody::Source { kind, name, .. } if kind == "clock" => Some(name.clone()),
        _ => None,
    });
    clock.or_else(|| {
        rendered_date.map(str::to_string).or_else(|| {
            events
                .iter()
                .find(|event| matches!(event.body, EventBody::SessionStarted { .. }))
                .map(|event| event.ts.chars().take(10).collect())
        })
    })
}

/// The tail `source` a resume across a date change adds, or `None` when the
/// context already knows today. The frozen system prompt holds the date it was
/// rendered on and never changes (the provider's cache lives there), so the
/// model learns a new day from the tail instead.
pub fn clock_source(events: &[LogEvent], today: &str) -> Option<PlanSource> {
    clock_source_from(events, today, None)
}

/// [`clock_source`], with the session's rendered date supplied by the caller.
pub fn clock_source_from(
    events: &[LogEvent],
    today: &str,
    rendered_date: Option<&str>,
) -> Option<PlanSource> {
    let last = last_announced_date_from(events, rendered_date)?;
    if last.as_str() >= today {
        return None;
    }
    Some(PlanSource {
        kind: "clock".into(),
        name: today.to_string(),
        text: format!("it is now {today} (UTC date); the date this session last knew was {last}"),
    })
}

// ------------------------------------------------------------------- STATUS

/// The numbers of one run (not of the whole session).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Tokens {
    pub prompt: u64,
    pub completion: u64,
    pub cached: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Status {
    pub schema: u32,
    pub job: String,
    pub session: Option<String>,
    pub session_name: Option<String>,
    pub run: String,
    pub started: String,
    pub finished: Option<String>,
    /// `running` while the run is in flight, then `done`, `error`, `stopped`,
    /// `timeout`, `skipped` or `paused`.
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub steps: u64,
    pub tokens: Tokens,
    /// `null` when no request had a known cost: unknown is not zero.
    pub cost_usd: Option<f64>,
    pub cost_source: Option<String>,
    pub cache_hit_ratio: Option<f64>,
    pub answer: String,
    pub error: Option<String>,
    pub schedule: Option<String>,
    pub pid: u32,
    /// Hooks that failed (they never change the exit code).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_errors: Vec<String>,
}

pub const STATUS_JSON: &str = "STATUS.json";
pub const STATUS_MD: &str = "STATUS.md";
pub const RUNS: &str = "runs.jsonl";
pub const LOCK: &str = "lock";
pub const NOTES: &str = "notes";

/// Write `STATUS.json` and `STATUS.md` atomically (each on its own: a reader
/// sees a whole old file or a whole new one).
pub fn write_status(dir: &Path, status: &Status) -> Result<(), String> {
    let json = serde_json::to_string_pretty(status).map_err(|error| error.to_string())?;
    crate::atomic::write_atomic(&dir.join(STATUS_JSON), json.as_bytes())
        .map_err(|error| format!("cannot write {STATUS_JSON}: {error}"))?;
    crate::atomic::write_atomic(&dir.join(STATUS_MD), render_md(status).as_bytes())
        .map_err(|error| format!("cannot write {STATUS_MD}: {error}"))
}

/// `Ok(None)` when the job never ran; `Err` when the file is unreadable.
pub fn read_status(dir: &Path) -> Result<Option<Status>, String> {
    let path = dir.join(STATUS_JSON);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| format!("{STATUS_JSON} is unreadable ({error})")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read {STATUS_JSON}: {error}")),
    }
}

/// The first lines of an answer, for STATUS.
pub fn head_of(answer: &str) -> String {
    let lines: Vec<&str> = answer.lines().take(12).collect();
    crate::util::truncate_chars(&lines.join("\n"), 1200)
}

fn compact_number(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// `STATUS.md`: short, for a person.
pub fn render_md(status: &Status) -> String {
    let mut out = format!("# job {}\n\n", status.job);
    out.push_str(&format!(
        "- outcome: **{}**{}\n",
        status.outcome,
        status
            .exit_code
            .map(|code| format!(" (exit {code})"))
            .unwrap_or_default()
    ));
    out.push_str(&format!(
        "- session: {}{}\n- run: {}\n",
        status.session.as_deref().unwrap_or("-"),
        status
            .session_name
            .as_ref()
            .map(|name| format!(" (`{name}`)"))
            .unwrap_or_default(),
        status.run
    ));
    out.push_str(&format!(
        "- started: {}\n- finished: {}\n",
        status.started,
        status.finished.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "- steps: {} · tokens: {} (cached {}) · cost: {}\n",
        status.steps,
        compact_number(status.tokens.total),
        status
            .cache_hit_ratio
            .map_or("-".to_string(), |ratio| format!("{:.0}%", ratio * 100.0)),
        status
            .cost_usd
            .map_or("unknown".to_string(), |cost| format!("${cost:.4}"))
    ));
    if let Some(schedule) = &status.schedule {
        out.push_str(&format!("- schedule: {schedule}\n"));
    }
    if let Some(error) = &status.error {
        out.push_str(&format!("\n## last error\n\n{error}\n"));
    }
    if !status.answer.trim().is_empty() {
        out.push_str(&format!("\n## answer (first lines)\n\n{}\n", status.answer));
    }
    out
}

/// Append one line to `runs.jsonl` in a single write, so concurrent appenders
/// (a skipped run beside the holder) cannot interleave inside a line.
pub fn append_run(dir: &Path, record: &Value) {
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
    let path = dir.join(RUNS);
    // A crash can leave a torn last line; the new record must not be glued to
    // it, so it starts on a line of its own.
    let torn = std::fs::File::open(&path)
        .ok()
        .and_then(|mut file| {
            let len = file.metadata().ok()?.len();
            if len == 0 {
                return Some(false);
            }
            file.seek(SeekFrom::Start(len - 1)).ok()?;
            let mut last = [0u8; 1];
            file.read_exact(&mut last).ok()?;
            Some(last[0] != b'\n')
        })
        .unwrap_or(false);
    let mut line = String::new();
    if torn {
        line.push('\n');
    }
    line.push_str(&record.to_string());
    line.push('\n');
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

/// The last `n` readable records of `runs.jsonl`, oldest first. Torn or corrupt
/// lines are skipped.
pub fn history(dir: &Path, n: usize) -> Vec<Value> {
    let text = std::fs::read_to_string(dir.join(RUNS)).unwrap_or_default();
    let mut rows: Vec<Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(Value::is_object)
        .collect();
    let drop = rows.len().saturating_sub(n);
    rows.drain(..drop);
    rows
}

/// A `runs.jsonl` record for a run that did not start.
pub fn skipped_record(job: &str, holder_pid: Option<u32>) -> Value {
    json!({
        "run": format!("run_{}", ulid::Ulid::new().to_string().to_lowercase()),
        "job": job,
        "outcome": "skipped",
        "started": crate::event::now_ts(),
        "finished": crate::event::now_ts(),
        "exit_code": 75,
        "holder_pid": holder_pid,
        "error": "another run of this job holds the lock",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse_with_units_and_refuse_nonsense() {
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_duration("12h").unwrap(), Duration::from_secs(43_200));
        assert_eq!(parse_duration("7d").unwrap(), Duration::from_secs(604_800));
        assert_eq!(
            parse_duration("2w").unwrap(),
            Duration::from_secs(1_209_600)
        );
        assert_eq!(parse_duration(" 5m ").unwrap(), Duration::from_secs(300));
        for bad in [
            "",
            "m",
            "-5m",
            "5x",
            "1.5h",
            "five",
            "99999999999999999999d",
        ] {
            assert!(parse_duration(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn new_session_every_is_never_or_a_nonzero_duration() {
        assert_eq!(Rotate::parse("never").unwrap(), Rotate::Never);
        assert_eq!(Rotate::parse("NEVER").unwrap(), Rotate::Never);
        assert_eq!(
            Rotate::parse("7d").unwrap(),
            Rotate::Every(Duration::from_secs(604_800))
        );
        assert!(Rotate::parse("0d").is_err());
        assert!(Rotate::parse("sometimes").is_err());
    }

    #[test]
    fn the_rotation_rule_is_age_or_a_large_context_and_never_under_never() {
        let week = Rotate::Every(Duration::from_secs(7 * 86_400));
        let day = Duration::from_secs(86_400);
        assert_eq!(
            should_rotate(Rotate::Never, day * 400, Some(10_000_000), 1000),
            None
        );
        assert_eq!(should_rotate(week, day * 3, Some(10), 1000), None);
        assert_eq!(
            should_rotate(week, day * 7, Some(10), 1000),
            Some(RotateWhy::Age)
        );
        assert_eq!(
            should_rotate(week, day, Some(900), 1000),
            Some(RotateWhy::Context)
        );
        assert_eq!(should_rotate(week, day, Some(899), 1000), None);
        // No request yet, or an unknown window: only age decides.
        assert_eq!(should_rotate(week, day, None, 1000), None);
        assert_eq!(should_rotate(week, day, Some(5), 0), None);
    }

    fn event(seq: u64, ts: &str, body: EventBody) -> LogEvent {
        LogEvent {
            seq,
            ts: ts.into(),
            body,
        }
    }

    fn started(ts: &str) -> LogEvent {
        event(
            1,
            ts,
            EventBody::SessionStarted {
                id: "ses_x".into(),
                parent: None,
                depth: 0,
                cwd: ".".into(),
                os: "x".into(),
                kelir_version: "0".into(),
                provider: "p".into(),
                model: "m".into(),
                system: crate::event::SystemInfo {
                    text: "s".to_string().into(),
                    hash: "h".into(),
                },
                sources: Vec::new(),
                tools: Vec::new(),
                reasoning_effort: None,
                key_origin: None,
            },
        )
    }

    /// A `KELIR_TODAY` override freezes the date in the system prompt; the
    /// event's wall-clock `ts` can be a different day (STEER-9 red test). The
    /// context knows the prompt's date, not the timestamp's.
    #[test]
    fn a_clock_source_uses_the_date_the_session_was_rendered_with() {
        let log = vec![started("2026-11-21T09:00:00Z")];
        let next = super::clock_source_from(&log, "2026-11-21", Some("2026-11-20"))
            .expect("the prompt's day is older than today");
        assert_eq!(next.name, "2026-11-21");
        assert!(
            next.text.contains("last knew was 2026-11-20"),
            "{}",
            next.text
        );
        // The same frozen day as today announces nothing.
        assert!(super::clock_source_from(&log, "2026-11-20", Some("2026-11-20")).is_none());
    }

    #[test]
    fn a_resume_on_the_same_day_adds_no_date_and_the_next_day_adds_one() {
        let log = vec![started("2026-11-20T09:00:00Z")];
        assert!(clock_source(&log, "2026-11-20").is_none());
        let next = clock_source(&log, "2026-11-21").unwrap();
        assert_eq!(next.kind, "clock");
        assert_eq!(next.name, "2026-11-21");
        assert!(next.text.contains("it is now 2026-11-21"), "{}", next.text);
        assert!(next.text.contains("2026-11-20"));
        // Once announced, the same day does not announce again.
        let mut log = log;
        log.push(event(
            2,
            "2026-11-21T09:00:00Z",
            EventBody::Source {
                kind: "clock".into(),
                name: "2026-11-21".into(),
                text: Some(next.text.clone().into()),
                path: None,
            },
        ));
        assert!(clock_source(&log, "2026-11-21").is_none());
        let after = clock_source(&log, "2026-11-23").unwrap();
        assert!(after.text.contains("last knew was 2026-11-21"));
        // A clock that went backwards never announces an earlier date.
        assert!(clock_source(&log, "2026-11-01").is_none());
    }

    #[test]
    fn a_session_age_is_read_from_its_first_event() {
        let log = vec![started("2026-11-14T00:00:00Z")];
        let now = time::OffsetDateTime::parse(
            "2026-11-21T00:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        assert_eq!(age_of(&log, now), Duration::from_secs(7 * 86_400));
        assert_eq!(age_of(&[], now), Duration::ZERO);
    }

    #[test]
    fn a_job_file_is_validated_key_by_key() {
        let file = Path::new("/x/jobs.toml");
        let jobs = parse_file(
            r#"
            [jobs.nightly]
            task = "do it"
            timeout = "30m"
            session = "nightly-main"
            new_session_every = "7d"
            notify = "notify-me"
            schedule = "every night"
            max_steps = 12
            [jobs.other]
            task_file = "t.md"
            timeout = 90
            notify = ["a", "b"]
            "#,
            file,
            Origin::Machine,
        )
        .unwrap();
        let nightly = jobs.iter().find(|job| job.name == "nightly").unwrap();
        assert_eq!(nightly.timeout, Some(Duration::from_secs(1800)));
        assert_eq!(nightly.session, "nightly-main");
        assert_eq!(nightly.notify, vec!["notify-me"]);
        assert_eq!(nightly.max_steps, Some(12));
        assert_eq!(nightly.rotate, Rotate::Every(Duration::from_secs(604_800)));
        let other = jobs.iter().find(|job| job.name == "other").unwrap();
        assert_eq!(
            other.session, "other",
            "the session defaults to the job name"
        );
        assert_eq!(other.timeout, Some(Duration::from_secs(90)));
        assert_eq!(other.task_file.as_deref(), Some(Path::new("/x/t.md")));
        assert_eq!(other.notify, vec!["a", "b"]);

        for bad in [
            "[jobs.x]\n",
            "[jobs.x]\ntask = \"a\"\ntask_file = \"b\"\n",
            "[jobs.x]\ntask = \"a\"\ntypo = 1\n",
            "[jobs.X]\ntask = \"a\"\n",
            "[jobs.x]\ntask = \"a\"\ntimeout = \"soon\"\n",
            "[jobs.x]\ntask = \"a\"\nsession = \"Bad Name\"\n",
            "[jobs.x]\ntask = \"a\"\nnew_session_every = \"0d\"\n",
            "[jobs.x]\ntask = \"a\"\nnotify = [\"\"]\n",
            "[jobs.x]\ntask = \" \"\n",
        ] {
            assert!(
                parse_file(bad, file, Origin::Machine).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    fn sample() -> Status {
        Status {
            schema: 1,
            job: "nightly".into(),
            session: Some("ses_a".into()),
            session_name: Some("nightly".into()),
            run: "run_1".into(),
            started: "2026-11-21T00:00:00Z".into(),
            finished: Some("2026-11-21T00:01:00Z".into()),
            outcome: "done".into(),
            exit_code: Some(0),
            steps: 3,
            tokens: Tokens {
                prompt: 1000,
                completion: 200,
                cached: 800,
                total: 1200,
            },
            cost_usd: Some(0.0123),
            cost_source: Some("provider".into()),
            cache_hit_ratio: Some(0.8),
            answer: "all good".into(),
            error: None,
            schedule: Some("0 3 * * *".into()),
            pid: 1,
            notify_errors: Vec::new(),
        }
    }

    #[test]
    fn status_round_trips_and_the_human_file_is_short() {
        let temp = tempfile::tempdir().unwrap();
        let status = sample();
        write_status(temp.path(), &status).unwrap();
        assert_eq!(read_status(temp.path()).unwrap().unwrap(), status);
        let md = std::fs::read_to_string(temp.path().join(STATUS_MD)).unwrap();
        assert!(md.contains("**done**") && md.contains("$0.0123") && md.contains("80%"));
        assert!(md.contains("all good") && md.contains("0 3 * * *"), "{md}");
        assert!(md.lines().count() < 20, "{md}");
        // No temp files linger.
        let names: Vec<String> = std::fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn a_corrupt_or_missing_status_is_an_answer_not_a_crash() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(read_status(temp.path()).unwrap(), None);
        std::fs::write(temp.path().join(STATUS_JSON), "{ not json").unwrap();
        let error = read_status(temp.path()).unwrap_err();
        assert!(error.contains("unreadable"), "{error}");
        std::fs::write(temp.path().join(STATUS_JSON), "").unwrap();
        assert!(read_status(temp.path()).is_err());
    }

    #[test]
    fn unknown_cost_is_null_not_zero() {
        let mut status = sample();
        status.cost_usd = None;
        let text = serde_json::to_string(&status).unwrap();
        assert!(text.contains("\"cost_usd\":null"), "{text}");
        assert!(render_md(&status).contains("cost: unknown"));
    }

    #[test]
    fn history_keeps_the_last_n_and_skips_torn_lines() {
        let temp = tempfile::tempdir().unwrap();
        for n in 0..5 {
            append_run(temp.path(), &json!({"run": n, "outcome": "done"}));
        }
        let mut raw = std::fs::read_to_string(temp.path().join(RUNS)).unwrap();
        raw.push_str("{\"run\": 5, \"outc");
        std::fs::write(temp.path().join(RUNS), raw).unwrap();
        append_run(temp.path(), &json!({"run": 6, "outcome": "error"}));
        let rows = history(temp.path(), 3);
        let runs: Vec<i64> = rows
            .iter()
            .map(|row| row["run"].as_i64().unwrap())
            .collect();
        assert_eq!(runs, vec![3, 4, 6]);
        assert!(history(&temp.path().join("nope"), 3).is_empty());
    }

    #[test]
    fn a_skipped_record_says_who_held_the_lock() {
        let record = skipped_record("nightly", Some(42));
        assert_eq!(record["outcome"], "skipped");
        assert_eq!(record["exit_code"], 75);
        assert_eq!(record["holder_pid"], 42);
    }
}
