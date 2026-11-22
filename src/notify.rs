//! Job notify hooks and event notifications.
//! Gate-like: a hook runs through the configured shell with a scrubbed environment (the platform
//! allow-list, never a provider key or a session token), at most [`TIMEOUT`] seconds, its whole
//! tree killed when it ends, and whatever it does or fails to do never changes the session's
//! outcome.

use std::path::Path;
use std::time::Duration;

pub const TIMEOUT: Duration = Duration::from_secs(30);
/// How much of a failing hook's stderr is kept.
const STDERR_TAIL: usize = 200;

fn timeout() -> Duration {
    crate::envs::var("KELIR_NOTIFY_TIMEOUT_MS")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(TIMEOUT)
}

/// Run every hook, in order. Returns one line per failure.
pub async fn run_hooks(
    hooks: &[String],
    shell: &[String],
    cwd: &Path,
    dir: &Path,
    job_env: &[(&str, &str)],
) -> Vec<String> {
    let mut errors = Vec::new();
    for (index, hook) in hooks.iter().enumerate() {
        if let Err(why) = run_one(hook, shell, cwd, dir, index, job_env).await {
            eprintln!("kelir: job: notify hook {} failed: {why}", index + 1);
            errors.push(format!("hook {}: {why}", index + 1));
        }
    }
    errors
}

async fn run_one(
    hook: &str,
    shell: &[String],
    cwd: &Path,
    dir: &Path,
    index: usize,
    job_env: &[(&str, &str)],
) -> Result<(), String> {
    let Some(program) = shell.first() else {
        return Err("no shell configured".into());
    };
    let shell_args: Vec<String> = shell.iter().skip(1).cloned().collect();
    let line = crate::gates::adapt_for_shell(hook, program);
    let scripts = dir.join("notify");
    std::fs::create_dir_all(&scripts).map_err(|error| error.to_string())?;
    let input = crate::shellcmd::input(&line, &shell_args, &scripts, &format!("n{index}"))?;
    let mut hook_args = shell_args.clone();
    match &input {
        crate::shellcmd::ShellInput::Argument(text) => {
            hook_args.push(text.clone());
        }
        crate::shellcmd::ShellInput::Script { path, keep_switch } => {
            let mut script_args =
                crate::shellcmd::script_args(&shell_args, keep_switch.as_deref(), path);
            hook_args.append(&mut script_args);
        }
    }
    let spec = crate::sys::runner::Spec {
        cwd: Some(cwd.to_path_buf()),
        env: crate::sys::runner::Env::Scrubbed {
            extra: job_env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        },
        stdin: crate::sys::runner::Stdin::Null,
        timeout: Some(timeout()),
        output_cap: 0,
        stderr_cap: 4096,
        no_window: true,
        job_name: {
            #[cfg(windows)]
            {
                Some(crate::bg::job_name(
                    "notify",
                    &format!(
                        "{}-{}",
                        std::process::id(),
                        ulid::Ulid::generate().to_string().to_lowercase()
                    ),
                ))
            }
            #[cfg(not(windows))]
            {
                None
            }
        },
        unix_guard: None,
        ..crate::sys::runner::Spec::new(program, hook_args)
    };
    let limit = timeout();
    let finished = crate::sys::runner::run(spec).await?;
    if finished.timed_out {
        return Err(format!("timed out after {} ms", limit.as_millis()));
    }
    let Some(status) = finished.status else {
        return Err("it did not finish".into());
    };
    if status.success() {
        return Ok(());
    }
    let tail = String::from_utf8_lossy(&finished.stderr);
    let tail = tail.trim().replace(['\r', '\n'], " ");
    let code = status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "a signal".into());
    Err(if tail.is_empty() {
        format!("exited with {code}")
    } else {
        format!(
            "exited with {code}: {}",
            crate::util::truncate_chars(&tail, STDERR_TAIL)
        )
    })
}

// ------------------------------------------------------------- notifications

use std::collections::BTreeMap;
use std::io::Write as _;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Hooks that are still running, so a one-shot session can give them a short grace period at exit.
static PENDING: OnceLock<Mutex<Vec<tokio::task::JoinHandle<()>>>> = OnceLock::new();

fn pending() -> &'static Mutex<Vec<tokio::task::JoinHandle<()>>> {
    PENDING.get_or_init(|| Mutex::new(Vec::new()))
}

pub const KINDS: [&str; 5] = [
    "turn_done",
    "error",
    "paused",
    "bg_settled",
    "child_settled",
];

/// Nothing is enabled by default: no hooks, no terminal records.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub hooks: BTreeMap<String, Vec<String>>,
    /// Emit an OSC terminal record when stdout is a TTY.
    pub terminal: bool,
    /// `osc9` (iTerm2/Ghostty) or `osc777` (rxvt/kitty); default `osc9`.
    pub terminal_kind: String,
    /// Minimum seconds between two notifications of the same kind.
    pub rate_limit_secs: u64,
    pub title: String,
}

impl Config {
    /// The hooks for one event kind (empty when not configured).
    pub fn hooks_for(&self, kind: &str) -> &[String] {
        self.hooks.get(kind).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Whether this kind has anything to do.
    pub fn enabled(&self, kind: &str) -> bool {
        !self.hooks_for(kind).is_empty() || self.terminal
    }
}

/// OSC 9 (iTerm2, Ghostty, WezTerm) with a BEL terminator.
pub fn osc9(message: &str) -> String {
    format!("\u{1b}]9;{message}\u{7}")
}

/// OSC 777 (kitty, rxvt-unicode): two fields, `notify;message`.
pub fn osc777(message: &str) -> String {
    format!("\u{1b}]777;notify;{message}\u{7}")
}

/// The terminal record for this config, or `None` when disabled. Control bytes in the message
/// are folded so a hook-supplied string cannot inject a second escape sequence.
pub fn terminal_record(config: &Config, message: &str) -> Option<String> {
    if !config.terminal {
        return None;
    }
    let clean: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if config.terminal_kind == "osc777" {
        Some(osc777(&clean))
    } else {
        Some(osc9(&clean))
    }
}

/// The message a terminal record carries: the title prefix and the event kind.
pub fn message(config: &Config, kind: &str, detail: &str) -> String {
    let tail = crate::util::truncate_chars(detail.trim(), 120);
    let head = if config.title.is_empty() {
        format!("kelir: {kind}")
    } else {
        format!("{}: {kind}", config.title)
    };
    if tail.is_empty() {
        head
    } else {
        format!("{head} · {tail}")
    }
}

/// One rate-limit clock, keyed by (session dir, kind). A repeated burst of
/// errors cannot start a thousand toasts.
static LAST: OnceLock<Mutex<BTreeMap<(String, String), Instant>>> = OnceLock::new();

fn last() -> &'static Mutex<BTreeMap<(String, String), Instant>> {
    LAST.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// A pure check: given the last time a kind fired, does `now` pass the limit?
pub fn passes(last: Option<Instant>, now: Instant, limit: Duration) -> bool {
    match last {
        Some(at) => now.duration_since(at) >= limit,
        None => true,
    }
}

/// Claim a notification slot for `(dir, kind)`. Returns false when it is too
/// soon after the previous one.
pub fn claim(dir: &std::path::Path, kind: &str, limit: Duration) -> bool {
    let key = (crate::util::path_string(dir), kind.to_string());
    let mut map = last().lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    let allowed = passes(map.get(&key).copied(), now, limit);
    if allowed {
        map.insert(key, now);
        if map.len() > 512 {
            let cutoff = now.checked_sub(Duration::from_secs(3600)).unwrap_or(now);
            map.retain(|_, at| *at >= cutoff);
        }
    }
    allowed
}

pub fn reset_rate_limit() {
    let mut map = last().lock().unwrap_or_else(|e| e.into_inner());
    map.clear();
}

/// Write a terminal record, but only when stdout is a real terminal. A pipe
/// (a `--json` job, a test) never sees an escape sequence.
pub fn write_terminal(record: &str) {
    use std::io::IsTerminal as _;
    if std::io::stdout().is_terminal() {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(record.as_bytes());
        let _ = out.flush();
    }
}

/// A spawned notification: its outcome is reported back through `finish`, and
/// the handle is dropped so the session never waits for it.
pub struct Dispatch {
    pub kind: String,
    pub hooks: usize,
    pub terminal: bool,
    pub error: Option<String>,
}

/// Fire one event's notifications. Hooks run in a background task; the caller
/// records the returned [`Dispatch`] as a `notify` event and moves on.
pub fn fire(
    cfg: &Config,
    extra: &BTreeMap<String, Vec<String>>,
    dir: &std::path::Path,
    shell: &[String],
    kind: &str,
    detail: &str,
) -> Option<Dispatch> {
    let mut hooks: Vec<String> = cfg.hooks_for(kind).to_vec();
    if let Some(more) = extra.get(kind) {
        hooks.extend(more.iter().cloned());
    }
    let terminal = cfg.terminal;
    if hooks.is_empty() && !terminal {
        return None;
    }
    let limit = Duration::from_secs(cfg.rate_limit_secs);
    if !claim(dir, kind, limit) {
        return Some(Dispatch {
            kind: kind.to_string(),
            hooks: 0,
            terminal: false,
            error: Some("rate limited".into()),
        });
    }
    let msg = message(cfg, kind, detail);
    if let Some(record) = terminal_record(cfg, &msg) {
        write_terminal(&record);
    }
    let count = hooks.len();
    if count > 0 {
        let shell = shell.to_vec();
        let dir = dir.to_path_buf();
        let env: Vec<(String, String)> = vec![
            ("KELIR_NOTIFY_EVENT".into(), kind.to_string()),
            ("KELIR_NOTIFY_MESSAGE".into(), msg.clone()),
        ];
        let handle = tokio::spawn(async move {
            let vars: Vec<(&str, &str)> = env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            let _ = run_hooks(&hooks, &shell, &dir, &dir, &vars).await;
        });
        {
            let mut pending = pending().lock().unwrap_or_else(|e| e.into_inner());
            pending.retain(|handle| !handle.is_finished());
            pending.push(handle);
        }
    }
    Some(Dispatch {
        kind: kind.to_string(),
        hooks: count,
        terminal,
        error: None,
    })
}

/// Give outstanding hooks a short grace period before a one-shot process exits.
pub async fn wait_outstanding(limit: Duration) {
    let handles: Vec<_> = std::mem::take(&mut *pending().lock().unwrap_or_else(|e| e.into_inner()));
    if handles.is_empty() {
        return;
    }
    let _ = tokio::time::timeout(limit, async {
        for handle in handles {
            let _ = handle.await;
        }
    })
    .await;
}

#[cfg(test)]
mod m11_tests {
    use super::*;

    #[test]
    fn osc_emitter_has_exact_bytes() {
        assert_eq!(osc9("hello"), "\u{1b}]9;hello\u{7}");
        assert_eq!(osc777("hello"), "\u{1b}]777;notify;hello\u{7}");
    }

    #[test]
    fn terminal_record_sanitises_control_bytes_and_respects_disabled() {
        let mut cfg = Config::default();
        assert_eq!(terminal_record(&cfg, "x"), None);
        cfg.terminal = true;
        assert_eq!(
            terminal_record(&cfg, "a\u{1b}]9;evil"),
            Some(osc9("a ]9;evil"))
        );
        cfg.terminal_kind = "osc777".into();
        assert_eq!(terminal_record(&cfg, "x"), Some(osc777("x")));
    }

    #[test]
    fn rate_limit_passes_first_then_blocks_until_the_window_closes() {
        let now = Instant::now();
        let limit = Duration::from_secs(10);
        assert!(passes(None, now, limit));
        assert!(!passes(Some(now), now, limit));
        assert!(passes(Some(now), now + limit, limit));
    }

    #[test]
    fn claim_is_per_dir_and_kind() {
        reset_rate_limit();
        let limit = Duration::from_secs(60);
        let dir = std::path::Path::new("C:/somewhere/a");
        assert!(claim(dir, "error", limit));
        assert!(!claim(dir, "error", limit));
        assert!(claim(dir, "paused", limit));
        assert!(claim(
            std::path::Path::new("C:/somewhere/b"),
            "error",
            limit
        ));
        reset_rate_limit();
    }

    #[test]
    fn config_hooks_are_per_kind() {
        let mut cfg = Config::default();
        cfg.hooks.insert("turn_done".into(), vec!["a".into()]);
        assert_eq!(cfg.hooks_for("turn_done"), ["a"]);
        assert!(cfg.hooks_for("error").is_empty());
        assert!(!cfg.enabled("error"));
        cfg.terminal = true;
        assert!(cfg.enabled("error"));
    }
}
