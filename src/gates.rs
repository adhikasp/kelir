//! Gates: the only way a veto or a nudge enters the core.
//! A gate is an executable or a built-in Rust gate the session runs at one of six named events.
//! It gets the event as JSON and may answer `allow`, `deny` or `rewrite` as JSON on stdout (built-ins
//! return the same types in process).
//! Opinions ("verify before you claim", "check the note you edited") live in gates, never in the

pub mod builtin;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Windows job names handed out by this process.
#[cfg(windows)]
static JOBS: AtomicU64 = AtomicU64::new(0);

/// Gate runs started by this process (part of each run's `KELIR_GATE_RUN` token).
static RUNS: AtomicU64 = AtomicU64::new(0);

/// At most this many gate processes are alive in one process at any moment.
pub const MAX_LIVE: usize = 8;

/// A child session (a descendant of the root) may have at most this many gate processes alive.
pub const MAX_LIVE_CHILD: usize = 2;

/// The most gate processes one root and all its live descendants can have alive.
pub const fn root_bound(max_live_children: usize) -> usize {
    MAX_LIVE + max_live_children * MAX_LIVE_CHILD
}

/// The cap that applies to this process (set once by a child session).
static CAP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(MAX_LIVE);

/// A session that is a child lowers its cap to [`MAX_LIVE_CHILD`].
pub fn limit_for_child() {
    CAP.store(MAX_LIVE_CHILD, std::sync::atomic::Ordering::SeqCst);
}

static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn live() -> usize {
    LIVE.load(std::sync::atomic::Ordering::SeqCst)
}

/// One of the [`MAX_LIVE`] slots; released on drop, whatever happened.
struct Slot;

impl Slot {
    fn take() -> Result<Slot, String> {
        use std::sync::atomic::Ordering::SeqCst;
        let mut now = LIVE.load(SeqCst);
        loop {
            let cap = CAP.load(SeqCst);
            if now >= cap {
                return Err(format!(
                    "{cap} gates are already running; this run was skipped"
                ));
            }
            match LIVE.compare_exchange(now, now + 1, SeqCst, SeqCst) {
                Ok(_) => return Ok(Slot),
                Err(seen) => now = seen,
            }
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The env var that marks every process of one gate run.
pub const RUN_ENV: &str = "KELIR_GATE_RUN";

/// A token no other run shares: this process, a counter and the clock.
fn run_token() -> String {
    let n = RUNS.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{n}-{nanos:x}", std::process::id())
}

/// Unix: after a gate run, nothing of it is left, even what left its process
/// group, and a killed session cannot orphan it.
#[cfg(unix)]
pub mod reap {
    use super::RUN_ENV;

    /// Kill the gate's descendants (by parent links) and every process that carries the run's token in its
    /// environment, until none is left.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn kill_run(root: u32, token: &str) {
        let me = std::process::id();
        let needle = format!("{RUN_ENV}={token}");
        let mut quiet = 0;
        for _ in 0..20 {
            let table = process_table();
            let mut victims: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
            let mut frontier = vec![root];
            while let Some(parent) = frontier.pop() {
                for (pid, ppid) in &table {
                    if *ppid == parent && *pid != me && victims.insert(*pid) {
                        frontier.push(*pid);
                    }
                }
            }
            for pid in marked_pids(&needle) {
                if pid != me {
                    victims.insert(pid);
                }
            }
            // A descendant can be reparented to init and become invisible to the parent-link walk before `ps` sees
            // it with the run token. Tolerate a couple of empty passes for macOS `ps` lag, then stop.
            if victims.is_empty() {
                quiet += 1;
                if quiet >= 3 {
                    return;
                }
            } else {
                quiet = 0;
                for pid in victims {
                    // Safe: SIGKILL to a pid read from /proc or ps a moment ago.
                    unsafe {
                        libc::kill(pid as libc::pid_t, libc::SIGKILL);
                    }
                }
            }
            // macOS `ps` can lag a freshly forked child; `/proc` on Linux is
            // immediate. Only the former needs a delay between sweeps.
            #[cfg(target_os = "macos")]
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn kill_run(_root: u32, _token: &str) {}

    /// `(pid, ppid)` of every process, read from `/proc` on Linux.
    #[cfg(target_os = "linux")]
    fn process_table() -> Vec<(u32, u32)> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                // `pid (comm) S ppid …`: comm may hold spaces and parentheses.
                let rest = &stat[stat.rfind(')')? + 1..];
                let ppid: u32 = rest.split_whitespace().nth(1)?.parse().ok()?;
                Some((pid, ppid))
            })
            .collect()
    }

    /// macOS has no `/proc`: `ps` is the portable way to read the parent
    /// links. Only pid/ppid are parsed (no command line, no environment).
    #[cfg(target_os = "macos")]
    fn process_table() -> Vec<(u32, u32)> {
        let output = crate::sys::runner::std_command_name("ps")
            .args(["-ax", "-o", "pid=,ppid="])
            .output();
        let Ok(output) = output else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pid: u32 = fields.next()?.parse().ok()?;
                let ppid: u32 = fields.next()?.parse().ok()?;
                Some((pid, ppid))
            })
            .collect()
    }

    /// macOS: one `ps axww -E` gives both pids and their environments.
    #[cfg(target_os = "macos")]
    fn marked_pids(needle: &str) -> std::collections::BTreeSet<u32> {
        let Ok(output) = crate::sys::runner::std_command_name("ps")
            .args(["axww", "-E", "-o", "pid=,command="])
            .output()
        else {
            return std::collections::BTreeSet::new();
        };
        if !output.status.success() {
            return std::collections::BTreeSet::new();
        }
        parse_marked_pids(&String::from_utf8_lossy(&output.stdout), needle)
    }

    /// Pure parser for the `ps` output: a line starts with the pid, the rest
    /// is the command and its `ps e` environment. Only whole entries count.
    #[cfg(target_os = "macos")]
    pub(crate) fn parse_marked_pids(ps: &str, needle: &str) -> std::collections::BTreeSet<u32> {
        ps.lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pid: u32 = fields.next()?.parse().ok()?;
                line.split_whitespace()
                    .any(|entry| entry == needle)
                    .then_some(pid)
            })
            .collect()
    }

    #[cfg(target_os = "linux")]
    fn marked_pids(needle: &str) -> std::collections::BTreeSet<u32> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return std::collections::BTreeSet::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
                std::fs::read(format!("/proc/{pid}/environ"))
                    .map(|bytes| bytes.split(|b| *b == 0).any(|e| e == needle.as_bytes()))
                    .unwrap_or(false)
                    .then_some(pid)
            })
            .collect()
    }

    /// Owns the cleanup of one run: a guard process that outlives a killed
    /// session, and the final sweep. Dropping it sweeps too (a cancelled run).
    pub struct Reaper {
        pid: u32,
        token: String,
        guard: Option<std::process::Child>,
        done: bool,
    }

    impl Reaper {
        /// `pid` is the gate shell (also its process group).
        pub fn start(pid: u32, token: &str, ttl: std::time::Duration) -> Self {
            let guard = crate::sys::runner::start_guard(&[
                "gate-guard".into(),
                "--parent".into(),
                std::process::id().to_string(),
                "--group".into(),
                pid.to_string(),
                "--token".into(),
                token.to_string(),
                "--ttl".into(),
                ttl.as_secs().to_string(),
            ]);
            Self {
                pid,
                token: token.to_string(),
                guard,
                done: false,
            }
        }

        /// The sweep. `group` also signals the gate's process group (a run that
        /// was cancelled rather than finished).
        pub fn reap(&mut self, group: bool) {
            if self.done {
                return;
            }
            self.done = true;
            if group {
                // Safe: the group this run created.
                unsafe {
                    libc::killpg(self.pid as libc::pid_t, libc::SIGKILL);
                }
            }
            kill_run(self.pid, &self.token);
            if let Some(mut guard) = self.guard.take() {
                let _ = guard.kill();
                let _ = guard.wait();
            }
        }
    }

    impl Drop for Reaper {
        fn drop(&mut self) {
            self.reap(true);
        }
    }

    /// `kelir gate-guard`: wait for the session (the parent) to disappear, then
    /// kill the gate's tree. Gives up after `ttl` seconds either way.
    pub fn guard_main(parent: u32, group: u32, token: &str, ttl: u64) -> u8 {
        let started = std::time::Instant::now();
        while started.elapsed().as_secs() < ttl {
            std::thread::sleep(std::time::Duration::from_millis(200));
            // Safe: signal 0 only probes; getppid has no preconditions.
            let alive = unsafe { libc::kill(parent as libc::pid_t, 0) } == 0
                && unsafe { libc::getppid() } as u32 == parent;
            if !alive {
                // Safe: the gate's own group.
                unsafe {
                    libc::killpg(group as libc::pid_t, libc::SIGKILL);
                }
                kill_run(group, token);
                return 0;
            }
        }
        0
    }
}

/// A kelir started by a gate (`KELIR_GATE_EVENT` is in its environment) runs
/// no gates of its own: a gate never triggers gates.
pub fn started_by_a_gate(marker: Option<&str>) -> bool {
    marker.is_some_and(|value| !value.trim().is_empty())
}

/// A gate that has not answered after this long is killed and allowed.
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// More stdout than this is garbage (and the rest is discarded).
pub const OUTPUT_MAX: usize = 1024 * 1024;
/// The longest `reason` kept in a `gate` event or shown to the model.
pub const REASON_MAX: usize = 2000;
/// How much of a crashed gate's stderr goes into the `error` field.
const STDERR_TAIL: usize = 300;
/// After the gate ended, how long its output pipes get to close.
const DRAIN: Duration = Duration::from_secs(2);

/// The gate timeout: [`TIMEOUT`], unless a test shortened it.
pub fn timeout() -> Duration {
    crate::envs::var("KELIR_TEST_GATE_TIMEOUT_MS")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(TIMEOUT)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    SessionStart,
    PromptSubmit,
    BeforeTool,
    AfterTool,
    TurnEnd,
    SessionEnd,
}

impl Event {
    pub const ALL: [Event; 6] = [
        Event::SessionStart,
        Event::PromptSubmit,
        Event::BeforeTool,
        Event::AfterTool,
        Event::TurnEnd,
        Event::SessionEnd,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Event::SessionStart => "session_start",
            Event::PromptSubmit => "prompt_submit",
            Event::BeforeTool => "before_tool",
            Event::AfterTool => "after_tool",
            Event::TurnEnd => "turn_end",
            Event::SessionEnd => "session_end",
        }
    }

    pub fn parse(name: &str) -> Option<Event> {
        let name = name.trim().replace('-', "_").to_ascii_lowercase();
        Event::ALL.into_iter().find(|event| event.name() == name)
    }

    /// Whether `deny` does anything at this event (the SPEC table).
    pub fn can_deny(self) -> bool {
        matches!(
            self,
            Event::PromptSubmit | Event::BeforeTool | Event::TurnEnd
        )
    }

    /// Whether `rewrite` does anything at this event.
    pub fn can_rewrite(self) -> bool {
        !matches!(self, Event::SessionStart | Event::SessionEnd)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateDef {
    pub name: String,
    pub event: Event,
    /// Empty for a built-in gate, which runs in this process instead of a shell.
    pub command: String,
    /// `Some(name)` selects a Rust built-in (`hint` is the first one).
    pub builtin: Option<String>,
    /// The built-in's own options, from its gate table.
    pub options: toml::Table,
    /// `before_tool`/`after_tool` only: the tool names this gate sees (empty is every tool).
    /// `mcp:<server>` covers every tool of a server and a trailing `*` is a prefix match.
    pub tools: Vec<String>,
    /// Extra environment, from the gate's own `env` table.
    pub env: BTreeMap<String, String>,
    /// Where the declaration came from: `config` or `skill <name>`.
    pub origin: String,
}

impl GateDef {
    /// Whether this gate runs for `tool` (always true for events without one).
    pub fn matches(&self, tool: Option<&str>) -> bool {
        let Some(tool) = tool else { return true };
        self.tools.is_empty() || self.tools.iter().any(|pattern| tool_matches(pattern, tool))
    }
}

fn tool_matches(pattern: &str, tool: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        return tool.starts_with(prefix);
    }
    if pattern == tool {
        return true;
    }
    // `mcp:server` names every tool of the server.
    pattern.starts_with("mcp:")
        && !pattern.contains('.')
        && tool
            .strip_prefix(pattern)
            .is_some_and(|rest| rest.starts_with('.'))
}

// ------------------------------------------------------------------- config

/// Read the `[gates]` table: each key an event name, each value a command string, a table, or
/// a list of those. A table with `builtin = "<name>"` declares a built-in gate.
pub fn from_toml(value: &toml::Value) -> Result<Vec<GateDef>, String> {
    let table = value
        .as_table()
        .ok_or_else(|| "`gates` must be a table of event names".to_string())?;
    let mut defs: Vec<GateDef> = Vec::new();
    for (key, entry) in table {
        if let Some(name) = Event::parse(key) {
            let items: Vec<&toml::Value> = match entry {
                toml::Value::Array(items) => items.iter().collect(),
                other => vec![other],
            };
            for item in items {
                let def = one_from_toml(name, item)?;
                push_unique(&mut defs, def);
            }
        } else if let toml::Value::Table(options) = entry
            && options.contains_key("builtin")
        {
            // `[gates.<name>]` declares one named gate; its event and options
            // live in the same table.
            let def = builtin_from_toml(key, options)?;
            push_unique(&mut defs, def);
        } else {
            return Err(format!(
                "[gates]: `{key}` is not an event (the six are {})",
                Event::ALL.map(Event::name).join(", ")
            ));
        }
    }
    Ok(defs)
}

fn builtin_from_toml(name: &str, options: &toml::Table) -> Result<GateDef, String> {
    let at = format!("[gates].{name}");
    if !valid_name(name) {
        return Err(format!("{at}: `name` is letters, digits, `-`, `_`, `.`"));
    }
    let builtin = options
        .get("builtin")
        .and_then(toml::Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| format!("{at}: `builtin` needs a name"))?;
    if !builtin::is_known(builtin) {
        return Err(format!(
            "{at}: unknown builtin `{builtin}` (the known ones are {})",
            builtin::NAMES.join(", ")
        ));
    }
    let event_name = options
        .get("event")
        .and_then(toml::Value::as_str)
        .unwrap_or(builtin::default_event(builtin));
    let event = Event::parse(event_name).ok_or_else(|| {
        format!(
            "{at}: `{event_name}` is not an event (the six are {})",
            Event::ALL.map(Event::name).join(", ")
        )
    })?;
    if !builtin::supports_event(builtin, event) {
        return Err(format!(
            "{at}: the built-in `{builtin}` supports only {}",
            builtin::supported_events(builtin)
        ));
    }
    builtin::validate_options(builtin, options, &at)?;
    let mut tools = Vec::new();
    if let Some(value) = options.get("tools").or_else(|| options.get("tool")) {
        match value {
            toml::Value::String(tool) => tools.push(tool.clone()),
            toml::Value::Array(list) => {
                for tool in list {
                    tools.push(
                        tool.as_str()
                            .ok_or_else(|| format!("{at}: `tools` holds strings"))?
                            .to_string(),
                    );
                }
            }
            _ => return Err(format!("{at}: `tools` is a string or a list")),
        }
    }
    if !tools.is_empty() && !matches!(event, Event::BeforeTool | Event::AfterTool) {
        return Err(format!("{at}: `tools` applies to before_tool/after_tool"));
    }
    Ok(GateDef {
        name: name.to_string(),
        event,
        command: String::new(),
        builtin: Some(builtin.to_string()),
        options: options.clone(),
        tools,
        env: BTreeMap::new(),
        origin: "config".into(),
    })
}

fn one_from_toml(event: Event, item: &toml::Value) -> Result<GateDef, String> {
    let at = event.name();
    match item {
        toml::Value::String(command) => {
            if command.trim().is_empty() {
                return Err(format!("[gates].{at}: an empty command"));
            }
            Ok(GateDef {
                name: name_from_command(command),
                event,
                command: command.clone(),
                builtin: None,
                options: toml::Table::new(),
                tools: Vec::new(),
                env: BTreeMap::new(),
                origin: "config".into(),
            })
        }
        toml::Value::Table(table) => {
            for key in table.keys() {
                if !["command", "name", "tool", "tools", "env"].contains(&key.as_str()) {
                    return Err(format!(
                        "[gates].{at}: unknown key `{key}` (command, name, tools, env)"
                    ));
                }
            }
            let command = table
                .get("command")
                .and_then(toml::Value::as_str)
                .filter(|command| !command.trim().is_empty())
                .ok_or_else(|| format!("[gates].{at}: a table needs a `command` string"))?;
            let name = match table.get("name") {
                Some(toml::Value::String(name)) if valid_name(name) => name.clone(),
                Some(_) => {
                    return Err(format!(
                        "[gates].{at}: `name` is letters, digits, `-`, `_`, `.`"
                    ));
                }
                None => name_from_command(command),
            };
            let mut tools = Vec::new();
            for key in ["tool", "tools"] {
                match table.get(key) {
                    None => {}
                    Some(toml::Value::String(tool)) => tools.push(tool.clone()),
                    Some(toml::Value::Array(list)) => {
                        for tool in list {
                            tools.push(
                                tool.as_str()
                                    .ok_or_else(|| format!("[gates].{at}: `{key}` holds strings"))?
                                    .to_string(),
                            );
                        }
                    }
                    Some(_) => return Err(format!("[gates].{at}: `{key}` is a string or a list")),
                }
            }
            if !tools.is_empty() && !matches!(event, Event::BeforeTool | Event::AfterTool) {
                return Err(format!(
                    "[gates].{at}: `tools` applies to before_tool/after_tool"
                ));
            }
            let mut env = BTreeMap::new();
            if let Some(value) = table.get("env") {
                let map = value
                    .as_table()
                    .ok_or_else(|| format!("[gates].{at}: `env` is a table of strings"))?;
                for (name, value) in map {
                    let value = value
                        .as_str()
                        .ok_or_else(|| format!("[gates].{at}: env `{name}` is not a string"))?;
                    if !valid_env_name(name) {
                        return Err(format!("[gates].{at}: `{name}` is not a variable name"));
                    }
                    env.insert(name.clone(), value.to_string());
                }
            }
            Ok(GateDef {
                name,
                event,
                command: command.to_string(),
                builtin: None,
                options: toml::Table::new(),
                tools,
                env,
                origin: "config".into(),
            })
        }
        _ => Err(format!(
            "[gates].{at}: a command string, a table or a list of them"
        )),
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
}

/// A name for a gate that did not give one: the stem of the last path-like
/// word of its command (`python gates/verify.py --x` is `verify`).
pub fn name_from_command(command: &str) -> String {
    let word = command
        .split_whitespace()
        .rev()
        .find(|word| !word.starts_with('-') && !word.contains('='))
        .or_else(|| command.split_whitespace().next())
        .unwrap_or("gate");
    let word = word.trim_matches(|c| c == '"' || c == '\'');
    let file = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let stem = file.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(file);
    let name: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() { "gate".into() } else { name }
}

/// Add a gate; a name already used gets `-2`, `-3`… so a `gate` event always
/// says which one ran.
pub fn push_unique(defs: &mut Vec<GateDef>, mut def: GateDef) {
    let base = def.name.clone();
    let mut n = 1;
    while defs.iter().any(|other| other.name == def.name) {
        n += 1;
        def.name = format!("{base}-{n}");
    }
    defs.push(def);
}

// -------------------------------------------------------------- skill gates

/// The key a skill's `metadata` uses to bind a script to events.
pub const SKILL_KEY: &str = "kelir-gate";

/// The `kelir-gate` value of a skill's metadata block (block or flow form).
pub fn skill_binding(metadata: &str) -> Option<String> {
    for line in metadata.lines() {
        let line = line.trim().trim_start_matches('{').trim_end_matches('}');
        for part in line.split(',') {
            let part = part.trim();
            if let Some(rest) = part.strip_prefix(SKILL_KEY) {
                let rest = rest.trim_start();
                if let Some(value) = rest.strip_prefix(':') {
                    let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Gates declared by the skills in `found`: a skill whose metadata says `kelir-gate:
/// <event>[=<script>][; <event>=<script>…]` binds a file under its own `scripts/` directory.
pub fn from_skills(found: &[crate::sources::Found], notes: &mut Vec<String>) -> Vec<GateDef> {
    let mut defs = Vec::new();
    for source in found {
        if source.builtin || source.kind != crate::sources::SourceKind::Skill {
            continue;
        }
        let Ok(skill) = crate::sources::parse_skill(&source.text) else {
            continue;
        };
        let Some(metadata) = skill.metadata.as_deref() else {
            continue;
        };
        let Some(binding) = skill_binding(metadata) else {
            continue;
        };
        let Some(dir) = source.path.parent() else {
            continue;
        };
        for part in binding.split(';') {
            let (event_name, script) = match part.split_once('=') {
                Some((event, script)) => (event.trim(), Some(script.trim())),
                None => (part.trim(), None),
            };
            let Some(event) = Event::parse(event_name) else {
                notes.push(format!(
                    "skill {}: kelir-gate `{event_name}` is not an event",
                    skill.name
                ));
                continue;
            };
            match resolve_script(dir, event, script) {
                Ok(path) => push_unique(
                    &mut defs,
                    GateDef {
                        name: format!("{}.{}", skill.name, event.name()),
                        event,
                        command: script_command(&path),
                        builtin: None,
                        options: toml::Table::new(),
                        tools: Vec::new(),
                        env: BTreeMap::new(),
                        origin: if source.layer == crate::sources::Layer::Project {
                            format!("project skill {}", skill.name)
                        } else {
                            format!("skill {}", skill.name)
                        },
                    },
                ),
                Err(why) => notes.push(format!("skill {}: kelir-gate: {why}", skill.name)),
            }
        }
    }
    defs
}

fn resolve_script(dir: &Path, event: Event, named: Option<&str>) -> Result<PathBuf, String> {
    let scripts = dir.join("scripts");
    if let Some(name) = named {
        let relative = Path::new(name);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(format!("`{name}` must stay inside the skill directory"));
        }
        let path = dir.join(relative);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!("`{name}` is not a file"))
        };
    }
    let files: Vec<PathBuf> = std::fs::read_dir(&scripts)
        .map_err(|_| "the skill has no scripts/ directory".to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    let stem = |path: &PathBuf| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default()
    };
    let wanted = event.name().replace('_', "-");
    for candidate in [event.name(), wanted.as_str(), "gate"] {
        if let Some(path) = files.iter().find(|path| stem(path) == candidate) {
            return Ok(path.clone());
        }
    }
    match files.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err("scripts/ is empty".into()),
        _ => Err(format!(
            "scripts/ holds several files and none is `{}.*` or `gate.*`: name one with `{}=scripts/<file>`",
            event.name(),
            event.name()
        )),
    }
}

/// A gate that came from the project's own files (its `.kelir/config.toml`, a skill under
/// `.kelir/skills` or `.claude/skills`): code the repository's author chose, not the person running
pub fn is_project_gate(def: &GateDef) -> bool {
    def.origin.starts_with("project ")
}

/// Whether the project at `cwd` may run its own gates. kelir has no prompt, so trust is declared:
/// `trusted_projects` in the machine config names a directory at or above `cwd`, or
pub fn project_trusted(cwd: &Path, trusted: &[PathBuf]) -> bool {
    if crate::envs::var("KELIR_TRUST_PROJECT").is_some_and(|v| !matches!(v.trim(), "" | "0")) {
        return true;
    }
    let canon = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let here = canon(cwd);
    trusted.iter().any(|dir| here.starts_with(canon(dir)))
}

/// Drop the project-layer gates of an untrusted project, saying so once per gate.
pub fn apply_trust(defs: Vec<GateDef>, trusted: bool, notes: &mut Vec<String>) -> Vec<GateDef> {
    if trusted {
        return defs;
    }
    defs.into_iter()
        .filter(|def| {
            let project = is_project_gate(def);
            if project {
                notes.push(format!(
                    "gate {} ({}) not started: this project is not trusted (add its directory to `trusted_projects` in the machine config, or set KELIR_TRUST_PROJECT=1)",
                    def.name, def.origin
                ));
            }
            !project
        })
        .collect()
}

pub fn script_command(path: &Path) -> String {
    let shown = crate::util::path_string(path).replace('\\', "/");
    let quoted = format!("\"{shown}\"");
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "py" => format!("{} {quoted}", python()),
        "sh" => format!("bash {quoted}"),
        "js" | "mjs" => format!("node {quoted}"),
        "ps1" => format!("powershell -NoProfile -ExecutionPolicy Bypass -File {quoted}"),
        "cmd" | "bat" => format!("cmd /C {quoted}"),
        _ => quoted,
    }
}

/// Adapt a `cmd /C …` command line to the shell that will run it.
pub fn adapt_for_shell(command: &str, shell_program: &str) -> String {
    let stem = Path::new(shell_program)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let posix = !matches!(stem.as_str(), "cmd" | "powershell" | "pwsh");
    if !cfg!(windows) || !posix {
        return command.to_string();
    }
    let trimmed = command.trim_start();
    for head in ["cmd /c ", "cmd.exe /c "] {
        if trimmed.len() >= head.len() && trimmed[..head.len()].eq_ignore_ascii_case(head) {
            let program = &trimmed[..head.len() - 3];
            return format!("{program}//C {}", &trimmed[head.len()..]);
        }
    }
    command.to_string()
}

/// `python3` where it exists, else `python` (on Windows the `python3` on PATH
/// is often a Store stub).
pub fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

// ---------------------------------------------------------------- decisions

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Allow {
        reason: String,
        context: Option<Context>,
        usage: Option<crate::stream::Usage>,
    },
    Deny {
        reason: String,
        context: Option<Context>,
        usage: Option<crate::stream::Usage>,
    },
    Rewrite {
        reason: String,
        input: Value,
        context: Option<Context>,
        usage: Option<crate::stream::Usage>,
    },
}

/// The longest `context` a gate may contribute, in characters.
pub const CONTEXT_MAX: usize = 32 * 1024;

/// The most the `context` field of a gate answer may add to the tail.
///
/// Extra context a gate contributes: appended as a `source` event at the tail,
/// after the user message, so the cached prefix is untouched.
#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub text: String,
    pub kind: String,
}

/// Take the shared fields out of a parsed decision so the runner can record
/// `context`/`usage` before it matches on the decision-specific fields.
fn split_answer(parsed: &mut Parsed) -> (Option<Context>, Option<crate::stream::Usage>) {
    match parsed {
        Parsed::Allow { context, usage, .. }
        | Parsed::Deny { context, usage, .. }
        | Parsed::Rewrite { context, usage, .. } => (context.take(), usage.take()),
    }
}

/// Parse the optional `context` field shared by external and built-in gates.
fn answer_context(object: &serde_json::Map<String, Value>) -> Result<Option<Context>, String> {
    let Some(value) = object.get("context").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .ok_or_else(|| "`context` is not a string".to_string())?;
    if text.is_empty() {
        return Ok(None);
    }
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .unwrap_or("gate");
    let kind = crate::util::truncate_chars(kind, 64);
    let text = crate::util::truncate_chars(text, CONTEXT_MAX);
    Ok(Some(Context { text, kind }))
}

/// Parse a gate's stdout. Empty is `allow` (a gate that only has side effects prints
/// nothing); anything else must be one JSON object with a known `decision`.
pub fn parse_decision(stdout: &str) -> Result<Parsed, String> {
    let text = stdout.trim();
    if text.is_empty() {
        return Ok(Parsed::Allow {
            reason: String::new(),
            context: None,
            usage: None,
        });
    }
    if text.len() > OUTPUT_MAX {
        return Err(format!("output over {OUTPUT_MAX} bytes"));
    }
    let value: Value = serde_json::from_str(text).map_err(|error| {
        format!(
            "not JSON ({error}): {}",
            crate::util::truncate_chars(text, 80)
        )
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| "not a JSON object".to_string())?;
    let decision = object
        .get("decision")
        .and_then(Value::as_str)
        .ok_or_else(|| "no `decision` string".to_string())?;
    let reason = match object.get("reason") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(reason)) => crate::util::truncate_chars(reason, REASON_MAX),
        Some(_) => return Err("`reason` is not a string".into()),
    };
    let context = answer_context(object)?;
    let usage = match object.get("usage").filter(|usage| !usage.is_null()) {
        Some(Value::Object(usage)) => {
            Some(crate::stream::parse_usage(&Value::Object(usage.clone())))
        }
        Some(_) => return Err("`usage` is not an object".into()),
        None => None,
    };
    match decision {
        "allow" => Ok(Parsed::Allow {
            reason,
            context,
            usage,
        }),
        "deny" => Ok(Parsed::Deny {
            reason,
            context,
            usage,
        }),
        "rewrite" => {
            let input = object
                .get("input")
                .filter(|input| input.is_object())
                .ok_or_else(|| "`rewrite` needs an `input` object".to_string())?;
            Ok(Parsed::Rewrite {
                reason,
                input: input.clone(),
                context,
                usage,
            })
        }
        other => Err(format!(
            "unknown decision `{}`",
            crate::util::truncate_chars(other, 20)
        )),
    }
}

/// One gate run, as the `gate` event records it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub gate: String,
    pub event: String,
    pub decision: String,
    pub reason: String,
    pub ms: u64,
    pub rewritten: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Allow,
    Deny {
        gate: String,
        reason: String,
    },
    /// The event's value after every rewrite: `{"text"}` for a prompt or a final answer, the tool
    /// input for `before_tool`, `{"result"}` for `after_tool`.
    Rewrite(Value),
}

/// Every run of one event, and what they add up to.
#[derive(Debug, Clone, PartialEq)]
pub struct Chain {
    pub runs: Vec<Run>,
    pub verdict: Verdict,
    /// `context` from every gate that answered with one, in declaration order.
    pub contexts: Vec<Context>,
    /// Token usage a built-in reported from its own provider call.
    pub usages: Vec<(String, crate::stream::Usage)>,
}

impl Chain {
    pub fn allow() -> Chain {
        Chain {
            runs: Vec::new(),
            verdict: Verdict::Allow,
            contexts: Vec::new(),
            usages: Vec::new(),
        }
    }
}

/// Apply a rewrite's `input` to the event's current payload, if its shape fits
/// the event. Returns the new payload and the rewritten value.
pub fn apply_rewrite(
    event: Event,
    gate: &str,
    payload: &mut Value,
    input: &Value,
) -> Result<Value, String> {
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("`rewrite` at {} needs input.{key} (a string)", event.name()))
    };
    match event {
        Event::PromptSubmit => {
            let new = text("text")?;
            payload["text"] = json!(new);
            Ok(json!({"text": new}))
        }
        Event::TurnEnd => {
            let new = text("text")?;
            payload["final_text"] = json!(new);
            Ok(json!({"text": new}))
        }
        Event::BeforeTool => {
            payload["input"] = input.clone();
            Ok(input.clone())
        }
        Event::AfterTool => {
            // `result` replaces what the model sees; `note` appends a line.
            let mut result = payload
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let mut changed = false;
            if let Some(new) = input.get("result").and_then(Value::as_str) {
                result = new.to_string();
                changed = true;
            }
            if let Some(note) = input.get("note").and_then(Value::as_str) {
                if !result.is_empty() && !result.ends_with('\n') {
                    result.push('\n');
                }
                result.push_str(&format!("[gate {gate}] {note}"));
                changed = true;
            }
            if !changed {
                return Err("`rewrite` at after_tool needs input.result or input.note".into());
            }
            payload["result"] = json!(result);
            Ok(json!({"result": result}))
        }
        Event::SessionStart | Event::SessionEnd => {
            Err(format!("{} has nothing to rewrite", event.name()))
        }
    }
}

/// The event document a gate sees. The JSON shape is the same for an external
/// gate reading stdin and a built-in reading its context: the six common keys
/// plus the event's own fields.
#[derive(Debug, Clone, PartialEq)]
pub struct GateInput(pub Value);

impl GateInput {
    pub fn new(
        event: Event,
        turn: u32,
        payload: &Value,
        session: &str,
        session_dir: &Path,
        cwd: &Path,
    ) -> GateInput {
        let mut object = serde_json::Map::new();
        object.insert("event".into(), json!(event.name()));
        object.insert("session".into(), json!(session));
        object.insert(
            "session_dir".into(),
            json!(crate::util::path_string(session_dir)),
        );
        object.insert(
            "log".into(),
            json!(crate::util::path_string(&session_dir.join("events.jsonl"))),
        );
        object.insert("cwd".into(), json!(crate::util::path_string(cwd)));
        object.insert("turn".into(), json!(turn));
        if let Some(fields) = payload.as_object() {
            for (key, value) in fields {
                object.insert(key.clone(), value.clone());
            }
        }
        GateInput(Value::Object(object))
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }

    pub fn text(&self) -> Option<&str> {
        self.0.get("text").and_then(Value::as_str)
    }

    pub fn from(&self) -> Option<&str> {
        self.0.get("from").and_then(Value::as_str)
    }

    pub fn event_name(&self) -> Option<&str> {
        self.0.get("event").and_then(Value::as_str)
    }
}

/// What a built-in may use that an external gate cannot: a provider client for
/// its own call, the log so far (read-only) and the session directory.
pub struct GateCtx<'a> {
    pub client: &'a crate::provider::Client,
    pub provider: &'a crate::config::ProviderConfig,
    pub config: &'a crate::config::Config,
    pub provider_name: &'a str,
    /// A line to record in the session log (a note from this built-in).
    pub note: &'a (dyn Fn(&str) + Sync),
    pub events: &'a [crate::event::LogEvent],
    pub dir: &'a Path,
    /// Where skills are discovered, from the session's config.
    pub skills_dirs: &'a [std::path::PathBuf],
    /// The machine config dir, for machine-level skills.
    pub config_dir: Option<&'a Path>,
    /// The session's cwd (also on `GateInput`, but typed here).
    pub cwd: &'a Path,
}

/// One built-in gate's answer. The same three fields the external runner parses
/// from JSON, plus a usage event the built-in's own provider call reported.
pub struct GateAnswer {
    pub reason: String,
    pub decision: &'static str,
    pub input: Option<Value>,
    pub context: Option<Context>,
    pub usage: Option<crate::stream::Usage>,
}

impl GateAnswer {
    pub fn allow() -> GateAnswer {
        GateAnswer {
            reason: String::new(),
            decision: "allow",
            input: None,
            context: None,
            usage: None,
        }
    }
}

/// A built-in implemented in Rust. `run` is bounded by the gate timeout by the
/// runner, just like an external gate.
pub trait BuiltinGate: Send + Sync {
    fn events(&self) -> &[Event];
    fn run<'a>(
        &'a mut self,
        input: &'a GateInput,
        ctx: &'a GateCtx<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<GateAnswer, String>> + Send + 'a>>;
}

// ------------------------------------------------------------------- runner

/// The session's gates and what a run needs to know about the session.
pub struct Runner {
    defs: Vec<GateDef>,
    /// One stateful instance per built-in gate, kept across events.
    builtins: tokio::sync::Mutex<Vec<(String, Box<dyn BuiltinGate>)>>,
    /// Notes a built-in wrote while the session was borrowed.
    notes: std::sync::Mutex<Vec<String>>,
    shell: Vec<String>,
    cwd: PathBuf,
    session_id: String,
    session_dir: PathBuf,
    timeout: Duration,
}

impl Runner {
    /// Take the notes a built-in left for the session to record.
    pub fn take_notes(&self) -> Vec<String> {
        std::mem::take(&mut self.notes.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn new(
        defs: Vec<GateDef>,
        shell: Vec<String>,
        cwd: PathBuf,
        session_id: String,
        session_dir: PathBuf,
    ) -> Runner {
        Runner {
            defs,
            builtins: tokio::sync::Mutex::new(Vec::new()),
            notes: std::sync::Mutex::new(Vec::new()),
            shell,
            cwd,
            session_id,
            session_dir,
            timeout: timeout(),
        }
    }

    /// A runner with another timeout (the unit tests; the env hook is for the
    /// binary).
    pub fn with_timeout(mut self, timeout: Duration) -> Runner {
        self.timeout = timeout;
        self
    }

    pub fn defs(&self) -> &[GateDef] {
        &self.defs
    }

    /// Whether any gate would run at `event` for `tool`.
    pub fn has(&self, event: Event, tool: Option<&str>) -> bool {
        self.defs
            .iter()
            .any(|def| def.event == event && def.matches(tool))
    }

    /// Run every gate bound to `event` (for `tool`), in declaration order. A rewrite is seen by
    /// the gates after it; the first `deny` ends the chain (where `deny` means something).
    /// External and built-in gates meet here: both produce the same decision object.
    pub async fn run_event(
        &self,
        event: Event,
        tool: Option<&str>,
        turn: u32,
        mut payload: Value,
        ctx: Option<&GateCtx<'_>>,
    ) -> Chain {
        let mut chain = Chain::allow();
        let mut rewritten: Option<Value> = None;
        for (index, def) in self.defs.iter().enumerate() {
            if def.event != event || !def.matches(tool) {
                continue;
            }
            let input = GateInput::new(
                event,
                turn,
                &payload,
                &self.session_id,
                &self.session_dir,
                &self.cwd,
            );
            let started = Instant::now();
            let outcome = if let Some(name) = def.builtin.as_deref() {
                let slot = match Slot::take() {
                    Ok(slot) => slot,
                    Err(error) => {
                        let mut run = Self::empty_run(def, event, started, 0);
                        run.error = Some(error);
                        chain.runs.push(run);
                        continue;
                    }
                };
                let answer = {
                    let mut slots = self.builtins.lock().await;
                    let index = if let Some(index) = slots.iter().position(|(n, _)| n == name) {
                        index
                    } else {
                        let gate: Box<dyn BuiltinGate> = match name {
                            "hint" => match builtin::hint::Hint::new(def) {
                                Ok(hint) => Box::new(hint),
                                Err(why) => {
                                    drop(slots);
                                    let mut run = Self::empty_run(def, event, started, 0);
                                    run.error = Some(why);
                                    chain.runs.push(run);
                                    continue;
                                }
                            },
                            _ => {
                                drop(slots);
                                let mut run = Self::empty_run(def, event, started, 0);
                                run.error = Some(format!("unknown built-in `{name}`"));
                                chain.runs.push(run);
                                continue;
                            }
                        };
                        slots.push((name.to_string(), gate));
                        slots.len() - 1
                    };
                    let gate = &mut slots[index].1;
                    let note_sink = &self.notes;
                    let note: &(dyn Fn(&str) + Sync) = &|message| {
                        note_sink
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(message.to_string());
                    };
                    let ctx_with_notes = ctx.map(|ctx| crate::gates::GateCtx { note, ..*ctx });
                    match ctx_with_notes.as_ref() {
                        Some(ctx) => {
                            tokio::time::timeout(self.timeout, gate.run(&input, ctx)).await
                        }
                        None => {
                            drop(slots);
                            drop(slot);
                            let mut run = Self::empty_run(def, event, started, 0);
                            run.error = Some("a built-in gate needs a session context".into());
                            chain.runs.push(run);
                            continue;
                        }
                    }
                };
                drop(slot);
                match answer {
                    Ok(Ok(answer)) => Ok((builtin::to_stdout(&answer), None)),
                    Ok(Err(why)) => Err((why, None)),
                    Err(_) => Err((
                        format!("timed out after {} ms", self.timeout.as_millis()),
                        None,
                    )),
                }
            } else {
                match self.execute(index, def, &input.0.to_string()).await {
                    Ok(stdout) => Ok((stdout, None)),
                    Err(error) => Err((error, None)),
                }
            };
            let ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            let mut run = Self::empty_run(def, event, started, ms);
            let (stdout, usage) = match outcome {
                Ok(value) => value,
                Err((error, usage)) => {
                    if let Some(usage) = usage {
                        chain.usages.push((def.name.clone(), usage));
                    }
                    run.error = Some(error);
                    chain.runs.push(run);
                    continue;
                }
            };
            let mut parsed = match parse_decision(&stdout) {
                Ok(parsed) => parsed,
                Err(error) => {
                    run.error = Some(error);
                    chain.runs.push(run);
                    continue;
                }
            };
            if let Some(usage) = usage {
                chain.usages.push((def.name.clone(), usage));
            }
            let (context, parsed_usage) = split_answer(&mut parsed);
            if let Some(context) = context {
                chain.contexts.push(context);
            }
            if let Some(usage) = parsed_usage {
                chain.usages.push((def.name.clone(), usage));
            }
            match parsed {
                Parsed::Allow { reason, .. } => run.reason = reason,
                Parsed::Deny { reason, .. } => {
                    run.decision = "deny".into();
                    run.reason = reason.clone();
                    if event.can_deny() {
                        chain.verdict = Verdict::Deny {
                            gate: def.name.clone(),
                            reason,
                        };
                        chain.runs.push(run);
                        return chain;
                    }
                    run.reason =
                        format!("{reason} (ignored: deny means nothing at {})", event.name());
                }
                Parsed::Rewrite { reason, input, .. } => {
                    run.reason = reason;
                    if !event.can_rewrite() {
                        run.error = Some(format!("rewrite means nothing at {}", event.name()));
                    } else {
                        match apply_rewrite(event, &def.name, &mut payload, &input) {
                            Ok(value) => {
                                run.decision = "rewrite".into();
                                run.rewritten = true;
                                rewritten = Some(value);
                            }
                            Err(error) => run.error = Some(error),
                        }
                    }
                }
            }
            chain.runs.push(run);
        }
        if let Some(value) = rewritten {
            chain.verdict = Verdict::Rewrite(value);
        }
        chain
    }

    fn empty_run(def: &GateDef, event: Event, _started: Instant, ms: u64) -> Run {
        Run {
            gate: def.name.clone(),
            event: event.name().into(),
            decision: "allow".into(),
            reason: String::new(),
            ms,
            rewritten: false,
            error: None,
        }
    }

    /// The JSON a gate reads on stdin: the session's fields, then the event's.
    pub fn stdin_for(&self, event: Event, turn: u32, payload: &Value) -> String {
        let mut object = serde_json::Map::new();
        object.insert("event".into(), json!(event.name()));
        object.insert("session".into(), json!(self.session_id));
        object.insert(
            "session_dir".into(),
            json!(crate::util::path_string(&self.session_dir)),
        );
        object.insert(
            "log".into(),
            json!(crate::util::path_string(
                &self.session_dir.join("events.jsonl")
            )),
        );
        object.insert("cwd".into(), json!(crate::util::path_string(&self.cwd)));
        object.insert("turn".into(), json!(turn));
        if let Some(fields) = payload.as_object() {
            for (key, value) in fields {
                object.insert(key.clone(), value.clone());
            }
        }
        Value::Object(object).to_string()
    }

    /// Start the gate, feed it `stdin`, and return its stdout, or why that
    /// failed (every `Err` is a fail-open).
    async fn execute(&self, index: usize, def: &GateDef, stdin: &str) -> Result<String, String> {
        let Some(program) = self.shell.first() else {
            return Err("no shell configured".into());
        };
        // Held until the gate and its tree are gone.
        let _slot = Slot::take()?;
        let shell_args: Vec<String> = self.shell.iter().skip(1).cloned().collect();
        let dir = self.session_dir.join("gates");
        let line = adapt_for_shell(&def.command, program);
        let input = crate::shellcmd::input(&line, &shell_args, &dir, &format!("g{index}"))?;
        let mut gate_args = shell_args.clone();
        match &input {
            crate::shellcmd::ShellInput::Argument(text) => {
                gate_args.push(text.clone());
            }
            crate::shellcmd::ShellInput::Script { path, keep_switch } => {
                let mut script_args =
                    crate::shellcmd::script_args(&shell_args, keep_switch.as_deref(), path);
                gate_args.append(&mut script_args);
            }
        }
        let token = run_token();
        let mut extra: BTreeMap<String, String> = BTreeMap::new();
        extra.insert(
            "KELIR_SESSION_DIR".into(),
            crate::util::path_string(&self.session_dir),
        );
        extra.insert("KELIR_GATE_EVENT".into(), def.event.name().into());
        extra.insert("KELIR_GATE_NAME".into(), def.name.clone());
        extra.insert(RUN_ENV.into(), token.clone());
        extra.extend(def.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        #[cfg(windows)]
        let job_name = {
            let n = JOBS.fetch_add(1, Ordering::SeqCst);
            Some(crate::bg::job_name(
                &self.session_id,
                &format!("gate-{}-{n}", std::process::id()),
            ))
        };
        #[cfg(not(windows))]
        let job_name = None;
        let spec = crate::sys::runner::Spec {
            cwd: Some(self.cwd.clone()),
            env: crate::sys::runner::Env::Scrubbed { extra },
            stdin: crate::sys::runner::Stdin::Bytes(stdin.as_bytes().to_vec()),
            timeout: Some(self.timeout),
            output_cap: OUTPUT_MAX,
            stderr_cap: 8 * 1024,
            no_window: true,
            job_name,
            unix_guard: None,
            ..crate::sys::runner::Spec::new(program, gate_args)
        };
        let (mut command, group) = crate::sys::runner::command(&spec)?;
        let mut child = crate::sys::runner::start(&mut command, &group)?;
        crate::sys::runner::feed_stdin(&spec, &mut child);
        #[cfg(unix)]
        let mut reaper = reap::Reaper::start(
            child.id().ok_or("the gate exited at once")?,
            &token,
            self.timeout + Duration::from_secs(15),
        );
        let stdout = child.stdout.take().ok_or("no stdout pipe")?;
        let stderr = child.stderr.take().ok_or("no stderr pipe")?;
        let out_task = tokio::spawn(crate::sys::runner::read_capped(stdout, OUTPUT_MAX));
        let err_task = tokio::spawn(crate::sys::runner::read_capped(stderr, 8 * 1024));

        let limit = self.timeout;
        let waited = tokio::time::timeout(limit, child.wait()).await;
        // The tree dies with the gate whatever happened: a gate that left a
        // background process behind leaves nothing.
        group.kill();
        // Whatever left the group (setsid, a double fork) goes too; the guard
        // that would have done it for a killed session is retired.
        #[cfg(unix)]
        reaper.reap(false);
        let status = match waited {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => return Err(format!("cannot wait for the gate: {error}")),
            Err(_) => {
                let _ = tokio::time::timeout(DRAIN, child.wait()).await;
                out_task.abort();
                err_task.abort();
                return Err(format!("timed out after {} ms", limit.as_millis()));
            }
        };
        let (out, overflow) = out_task.await.unwrap_or_default();
        let (err, _) = err_task.await.unwrap_or_default();
        if !status.success() {
            let tail = String::from_utf8_lossy(&err);
            let tail = tail.trim();
            let code = status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "a signal".into());
            return Err(if tail.is_empty() {
                format!("exited with {code}")
            } else {
                format!(
                    "exited with {code}: {}",
                    crate::util::truncate_chars(&tail.replace(['\r', '\n'], " "), STDERR_TAIL)
                )
            });
        }
        if overflow {
            return Err(format!("output over {OUTPUT_MAX} bytes"));
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

pub use crate::sys::runner::allowed_env;

// ----------------------------------------------------------------- describe

/// The effective gate configuration as `observe` prints it: one line per
/// gate, never an environment value (the `env` table is named by key only).
pub fn describe(defs: &[GateDef]) -> Vec<Value> {
    defs.iter()
        .map(|def| {
            json!({
                "name": def.name,
                "event": def.event.name(),
                "origin": def.origin,
                "builtin": def.builtin,
                "command": crate::util::truncate_chars(&def.command, 200),
                "tools": def.tools,
                "env": def.env.keys().collect::<Vec<_>>(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live counter is process-wide: tests that start gates take turns.
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Holds the turn and, when the test ends, asserts no gate slot leaked.
    struct Quiet(
        #[expect(dead_code, reason = "the guard is held for its Drop, never read")]
        tokio::sync::MutexGuard<'static, ()>,
    );

    impl Drop for Quiet {
        fn drop(&mut self) {
            if !std::thread::panicking() {
                assert_eq!(live(), 0, "a test left a gate process counted as alive");
            }
        }
    }

    fn def(event: Event, command: &str) -> GateDef {
        GateDef {
            name: name_from_command(command),
            event,
            command: command.to_string(),
            builtin: None,
            options: toml::Table::new(),
            tools: Vec::new(),
            env: BTreeMap::new(),
            origin: "test".into(),
        }
    }

    fn runner(dir: &Path, defs: Vec<GateDef>) -> Runner {
        std::fs::create_dir_all(dir).unwrap();
        Runner::new(
            defs,
            crate::config::default_shell(),
            dir.to_path_buf(),
            "ses_test".into(),
            dir.to_path_buf(),
        )
        .with_timeout(Duration::from_secs(20))
    }

    // ------------------------------------------------------- decision parsing

    #[test]
    fn a_decision_is_one_json_object() {
        assert_eq!(
            parse_decision(r#"{"decision":"allow"}"#),
            Ok(Parsed::Allow {
                reason: String::new(),
                context: None,
                usage: None,
            })
        );
        assert_eq!(
            parse_decision(r#" {"decision":"deny","reason":"no"} "#),
            Ok(Parsed::Deny {
                reason: "no".into(),
                context: None,
                usage: None,
            })
        );
        assert_eq!(
            parse_decision(r#"{"decision":"rewrite","input":{"text":"x"}}"#),
            Ok(Parsed::Rewrite {
                reason: String::new(),
                input: json!({"text": "x"}),
                context: None,
                usage: None,
            })
        );
        assert_eq!(
            parse_decision(r#"{"decision":"allow","context":"a note","kind":"hint"}"#),
            Ok(Parsed::Allow {
                reason: String::new(),
                context: Some(Context {
                    text: "a note".into(),
                    kind: "hint".into()
                }),
                usage: None,
            })
        );
    }

    #[test]
    fn empty_output_is_allow_and_everything_else_wrong_is_an_error() {
        assert!(matches!(parse_decision(""), Ok(Parsed::Allow { .. })));
        assert!(matches!(parse_decision("  \n"), Ok(Parsed::Allow { .. })));
        for bad in [
            "garbage",
            "[]",
            "42",
            r#"{"reason":"x"}"#,
            r#"{"decision":"maybe"}"#,
            r#"{"decision":7}"#,
            r#"{"decision":"rewrite"}"#,
            r#"{"decision":"rewrite","input":"text"}"#,
            r#"{"decision":"deny","reason":3}"#,
            r#"{"decision":"allow"} trailing"#,
        ] {
            assert!(parse_decision(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn a_huge_answer_is_an_error_and_a_long_reason_is_cut() {
        let huge = format!(
            r#"{{"decision":"deny","reason":"{}"}}"#,
            "x".repeat(OUTPUT_MAX)
        );
        assert!(parse_decision(&huge).is_err());
        let long = format!(r#"{{"decision":"deny","reason":"{}"}}"#, "y".repeat(10_000));
        match parse_decision(&long).unwrap() {
            Parsed::Deny { reason, .. } => assert!(reason.chars().count() <= REASON_MAX + 1),
            other => panic!("{other:?}"),
        }
    }

    // ----------------------------------------------------------- declarations

    #[test]
    fn the_gates_table_takes_strings_tables_and_lists() {
        let value: toml::Value = toml::from_str(
            r#"
turn_end = "python gates/verify.py"
before_tool = [ "sh a.sh", { name = "no-rm", command = "sh b.sh", tools = ["bash"], env = { A = "b" } } ]
session_end = { command = "git push" }
"#,
        )
        .unwrap();
        let defs = from_toml(&value).unwrap();
        assert_eq!(defs.len(), 4);
        let turn_end = defs.iter().find(|d| d.event == Event::TurnEnd).unwrap();
        assert_eq!(turn_end.name, "verify");
        let named = defs.iter().find(|d| d.name == "no-rm").unwrap();
        assert_eq!(named.tools, vec!["bash"]);
        assert_eq!(named.env.get("A").map(String::as_str), Some("b"));
    }

    #[test]
    fn a_builtin_gate_is_declared_by_table_with_its_own_options() {
        let value: toml::Value = toml::from_str(
            r#"
hint = { builtin = "hint", model = "cheap/model", timeout_ms = 400, max_names = 2 }
"#,
        )
        .unwrap();
        let defs = from_toml(&value).unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "hint");
        assert_eq!(defs[0].event, Event::PromptSubmit);
        assert_eq!(defs[0].builtin.as_deref(), Some("hint"));
        assert!(defs[0].command.is_empty());
        assert_eq!(defs[0].options["model"].as_str(), Some("cheap/model"));
    }

    #[test]
    fn builtin_integer_options_cannot_be_strings() {
        for (text, key) in [
            (
                "hint = { builtin = 'hint', model = 'm', max_names = \"3\" }",
                "max_names",
            ),
            (
                "hint = { builtin = 'hint', model = 'm', timeout_ms = \"400\" }",
                "timeout_ms",
            ),
            (
                "hint = { builtin = 'hint', model = 'm', timeout = 0 }",
                "timeout",
            ),
            (
                "hint = { builtin = 'hint', model = 'm', max_names = -1 }",
                "max_names",
            ),
        ] {
            let value: toml::Value = toml::from_str(text).unwrap();
            let error = from_toml(&value).unwrap_err();
            assert!(error.contains(key), "{text}: {error}");
            assert!(error.contains("positive integer"), "{text}: {error}");
        }
        let value: toml::Value =
            toml::from_str("hint = { builtin = 'hint', model = 'm', max_names = 10 }").unwrap();
        assert!(from_toml(&value).is_ok());
    }

    /// The note location is part of the event contract: keep the reference in
    /// step with the `gate` location every gate failure records.
    #[test]
    fn the_reference_documents_the_builtin_note_location() {
        let reference = include_str!("../docs/reference/sources-and-skills.md");
        assert!(
            reference.contains("(`gate` location"),
            "the hint failure note's `where` must be documented as `gate`"
        );
        assert!(
            !reference.contains("(`source` location"),
            "stale `source` note-location claim"
        );
    }

    #[test]
    fn a_bad_builtin_declaration_says_what_is_wrong() {
        for (text, needle) in [
            (
                "nope = { builtin = 'missing' }",
                "unknown builtin `missing`",
            ),
            ("hint = { builtin = 'hint' }", "needs a `model`"),
            (
                "hint = { builtin = 'hint', model = 'm', event = 'turn_end' }",
                "supports only prompt_submit",
            ),
            (
                "hint = { builtin = 'hint', model = 'm', bogus = 1 }",
                "unknown key `bogus`",
            ),
        ] {
            let value: toml::Value = toml::from_str(text).unwrap();
            let error = from_toml(&value).unwrap_err();
            assert!(error.contains(needle), "{text}: {error}");
        }
    }

    #[test]
    fn a_bad_gates_table_says_what_is_wrong() {
        for (text, needle) in [
            ("turn_start = \"x\"", "not an event"),
            ("turn_end = 3", "command string"),
            ("turn_end = \"\"", "empty command"),
            ("turn_end = { name = \"x\" }", "needs a `command`"),
            (
                "turn_end = { command = \"x\", tools = [\"bash\"] }",
                "before_tool",
            ),
            (
                "before_tool = { command = \"x\", bogus = 1 }",
                "unknown key",
            ),
            (
                "before_tool = { command = \"x\", env = { \"1A\" = \"b\" } }",
                "variable name",
            ),
            (
                "before_tool = { command = \"x\", name = \"a b\" }",
                "`name`",
            ),
        ] {
            let value: toml::Value = toml::from_str(text).unwrap();
            let error = from_toml(&value).unwrap_err();
            assert!(error.contains(needle), "{text}: {error}");
        }
    }

    #[test]
    fn two_gates_with_one_name_are_told_apart() {
        let mut defs = Vec::new();
        push_unique(&mut defs, def(Event::TurnEnd, "sh check.sh"));
        push_unique(&mut defs, def(Event::BeforeTool, "sh check.sh"));
        assert_eq!(defs[0].name, "check");
        assert_eq!(defs[1].name, "check-2");
    }

    #[test]
    fn names_come_from_the_last_path_of_the_command() {
        assert_eq!(name_from_command("python gates/verify.py"), "verify");
        assert_eq!(
            name_from_command("python \"C:/x/y/note_check.py\" --fast"),
            "note_check"
        );
        assert_eq!(name_from_command("true"), "true");
    }

    #[test]
    fn tool_patterns_match_a_name_a_server_or_a_prefix() {
        assert!(tool_matches("bash", "bash"));
        assert!(!tool_matches("bash", "edit"));
        assert!(tool_matches("mcp:notes", "mcp:notes.search"));
        assert!(!tool_matches("mcp:notes", "mcp:notesy.search"));
        assert!(tool_matches("mcp:notes.search", "mcp:notes.search"));
        assert!(tool_matches("mcp:*", "mcp:a.b"));
        let mut gate = def(Event::BeforeTool, "x");
        assert!(gate.matches(Some("anything")));
        gate.tools = vec!["edit".into()];
        assert!(gate.matches(Some("edit")) && !gate.matches(Some("bash")));
    }

    // ---------------------------------------------------------- skill binding

    #[test]
    fn a_skill_binds_its_scripts_by_metadata() {
        assert_eq!(
            skill_binding("kelir-gate: turn_end").as_deref(),
            Some("turn_end")
        );
        assert_eq!(
            skill_binding("{kelir-gate: turn_end}").as_deref(),
            Some("turn_end")
        );
        assert_eq!(
            skill_binding("owner: x, kelir-gate: \"after_tool=scripts/a.py\"").as_deref(),
            Some("after_tool=scripts/a.py")
        );
        assert_eq!(skill_binding("owner: x"), None);
    }

    fn skill_on_disk(root: &Path, front: &str, scripts: &[&str]) -> Vec<crate::sources::Found> {
        let dir = root.join("verify");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        for name in scripts {
            std::fs::write(dir.join("scripts").join(name), "print()").unwrap();
        }
        let text = format!("---\nname: verify\ndescription: d\n{front}\n---\nbody\n");
        std::fs::write(dir.join("SKILL.md"), &text).unwrap();
        vec![crate::sources::Found {
            kind: crate::sources::SourceKind::Skill,
            name: "verify".into(),
            path: dir.join("SKILL.md"),
            text,
            layer: crate::sources::Layer::Project,
            builtin: false,
        }]
    }

    #[test]
    fn a_skill_script_is_found_by_event_then_gate_then_the_only_file() {
        let temp = tempfile::tempdir().unwrap();
        let mut notes = Vec::new();
        let found = skill_on_disk(
            temp.path(),
            "metadata: {kelir-gate: turn_end}",
            &["other.py", "turn_end.py"],
        );
        let defs = from_skills(&found, &mut notes);
        assert_eq!(defs.len(), 1, "{notes:?}");
        assert_eq!(defs[0].event, Event::TurnEnd);
        assert_eq!(defs[0].name, "verify.turn_end");
        assert!(
            defs[0].command.contains("turn_end.py"),
            "{}",
            defs[0].command
        );

        let temp = tempfile::tempdir().unwrap();
        let found = skill_on_disk(
            temp.path(),
            "metadata:\n  kelir-gate: turn_end",
            &["only.sh"],
        );
        let defs = from_skills(&found, &mut notes);
        assert!(defs[0].command.starts_with("bash "), "{}", defs[0].command);
    }

    #[test]
    fn a_skill_binding_that_cannot_be_resolved_is_a_note_not_a_failure() {
        let temp = tempfile::tempdir().unwrap();
        let mut notes = Vec::new();
        let found = skill_on_disk(
            temp.path(),
            "metadata: {kelir-gate: turn_end}",
            &["a.py", "b.py"],
        );
        assert!(from_skills(&found, &mut notes).is_empty());
        assert!(notes[0].contains("name one"), "{notes:?}");
        let found = skill_on_disk(
            temp.path(),
            "metadata: {kelir-gate: \"turn_end=../../etc/x\"}",
            &["a.py"],
        );
        assert!(from_skills(&found, &mut notes).is_empty());
        assert!(
            notes.iter().any(|n| n.contains("inside the skill")),
            "{notes:?}"
        );
        let found = skill_on_disk(temp.path(), "metadata: {kelir-gate: sometimes}", &["a.py"]);
        assert!(from_skills(&found, &mut notes).is_empty());
    }

    // ------------------------------------------------------- the event JSON

    #[test]
    fn the_event_json_carries_the_session_and_the_events_own_fields() {
        let temp = tempfile::tempdir().unwrap();
        let runner = runner(temp.path(), vec![]);
        let text = runner.stdin_for(
            Event::BeforeTool,
            3,
            &json!({"tool":"bash","input":{"command":"ls"}}),
        );
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["event"], "before_tool");
        assert_eq!(value["session"], "ses_test");
        assert_eq!(value["turn"], 3);
        assert_eq!(value["tool"], "bash");
        assert_eq!(value["input"]["command"], "ls");
        for key in ["session_dir", "log", "cwd"] {
            assert!(value[key].as_str().is_some_and(|s| !s.is_empty()), "{key}");
        }
        assert!(value["log"].as_str().unwrap().ends_with("events.jsonl"));
        assert!(!text.contains("token"), "{text}");
    }

    #[test]
    fn rewrite_shapes_follow_the_event() {
        let mut payload = json!({"text": "a"});
        let value = apply_rewrite(
            Event::PromptSubmit,
            "g",
            &mut payload,
            &json!({"text": "b"}),
        )
        .unwrap();
        assert_eq!(value, json!({"text": "b"}));
        assert_eq!(payload["text"], "b");
        assert!(apply_rewrite(Event::PromptSubmit, "g", &mut payload, &json!({"x": 1})).is_err());

        let mut payload = json!({"final_text": "a"});
        apply_rewrite(Event::TurnEnd, "g", &mut payload, &json!({"text": "b"})).unwrap();
        assert_eq!(payload["final_text"], "b");

        let mut payload = json!({"input": {"command": "a"}});
        apply_rewrite(
            Event::BeforeTool,
            "g",
            &mut payload,
            &json!({"command": "b"}),
        )
        .unwrap();
        assert_eq!(payload["input"]["command"], "b");

        let mut payload = json!({"result": "out"});
        let value = apply_rewrite(
            Event::AfterTool,
            "chk",
            &mut payload,
            &json!({"note": "hm"}),
        )
        .unwrap();
        assert_eq!(value["result"], "out\n[gate chk] hm");
        assert!(apply_rewrite(Event::AfterTool, "g", &mut payload, &json!({})).is_err());
        assert!(apply_rewrite(Event::SessionEnd, "g", &mut payload, &json!({"text":"x"})).is_err());
    }

    // ------------------------------------------------------------ environment

    #[test]
    fn a_gate_never_inherits_a_secret() {
        for secret in [
            "KELIR_TOKEN",
            "KELIR_SOCK",
            "OPENROUTER_API_KEY",
            "KELIR_GO_API_KEY",
            "MY_CUSTOM_KEY_ENV",
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
        ] {
            assert!(!allowed_env(secret), "{secret} must not reach a gate");
        }
        for ordinary in [
            "PATH",
            "HOME",
            "TEMP",
            "SystemRoot",
            "SSH_AUTH_SOCK",
            "LC_ALL",
        ] {
            assert!(allowed_env(ordinary), "{ordinary}");
        }
    }

    // --------------------------------------------------- real gate processes

    fn answers(temp: &Path, name: &str, body: &str) -> String {
        let path = temp.join(name);
        std::fs::write(&path, body).unwrap();
        format!(
            "bash \"{}\"",
            crate::util::path_string(&path).replace('\\', "/")
        )
    }

    #[tokio::test]
    async fn a_chain_runs_in_order_and_a_deny_stops_it() {
        let _serial = Quiet(SERIAL.lock().await);
        let temp = tempfile::tempdir().unwrap();
        let rewrite = answers(
            temp.path(),
            "rewrite.sh",
            "cat > seen1.json\necho '{\"decision\":\"rewrite\",\"input\":{\"text\":\"second\"}}'\n",
        );
        let seen = answers(
            temp.path(),
            "seen.sh",
            "cat > seen2.json\necho '{\"decision\":\"deny\",\"reason\":\"stop here\"}'\n",
        );
        let never = answers(temp.path(), "never.sh", "echo ran > never.txt\n");
        let runner = runner(
            temp.path(),
            vec![
                def(Event::PromptSubmit, &rewrite),
                def(Event::PromptSubmit, &seen),
                def(Event::PromptSubmit, &never),
            ],
        );
        let chain = runner
            .run_event(Event::PromptSubmit, None, 1, json!({"text": "first"}), None)
            .await;
        assert_eq!(chain.runs.len(), 2, "{:?}", chain.runs);
        assert_eq!(chain.runs[0].decision, "rewrite");
        assert!(chain.runs[0].rewritten);
        assert_eq!(chain.runs[1].decision, "deny");
        assert_eq!(
            chain.verdict,
            Verdict::Deny {
                gate: "seen".into(),
                reason: "stop here".into()
            }
        );
        let second: Value =
            serde_json::from_str(&std::fs::read_to_string(temp.path().join("seen2.json")).unwrap())
                .unwrap();
        assert_eq!(second["text"], "second", "the next gate sees the rewrite");
        assert!(!temp.path().join("never.txt").exists());
    }

    #[tokio::test]
    async fn a_crash_garbage_and_silence_all_fail_open() {
        let _serial = Quiet(SERIAL.lock().await);
        let temp = tempfile::tempdir().unwrap();
        let crash = answers(temp.path(), "crash.sh", "echo boom >&2\nexit 3\n");
        let garbage = answers(temp.path(), "garbage.sh", "echo not json\n");
        let silent = answers(temp.path(), "silent.sh", "true\n");
        let missing = "definitely-not-a-command-kelir-test".to_string();
        let runner = runner(
            temp.path(),
            vec![
                def(Event::BeforeTool, &crash),
                def(Event::BeforeTool, &garbage),
                def(Event::BeforeTool, &silent),
                def(Event::BeforeTool, &missing),
            ],
        );
        let chain = runner
            .run_event(
                Event::BeforeTool,
                Some("bash"),
                1,
                json!({"tool":"bash","input":{}}),
                None,
            )
            .await;
        assert_eq!(chain.verdict, Verdict::Allow);
        assert_eq!(chain.runs.len(), 4);
        assert!(chain.runs.iter().all(|run| run.decision == "allow"));
        let errors: Vec<_> = chain.runs.iter().map(|run| run.error.clone()).collect();
        assert!(
            errors[0]
                .as_deref()
                .unwrap()
                .contains("exited with 3: boom"),
            "{errors:?}"
        );
        assert!(
            errors[1].as_deref().unwrap().contains("not JSON"),
            "{errors:?}"
        );
        assert_eq!(errors[2], None, "silence is not an error");
        assert!(
            errors[3].is_some(),
            "a missing command is a failure: {errors:?}"
        );
    }

    #[tokio::test]
    async fn deny_means_nothing_where_the_table_says_so() {
        let _serial = Quiet(SERIAL.lock().await);
        let temp = tempfile::tempdir().unwrap();
        let deny = answers(
            temp.path(),
            "deny.sh",
            "echo '{\"decision\":\"deny\",\"reason\":\"x\"}'\n",
        );
        let runner = runner(
            temp.path(),
            vec![def(Event::SessionEnd, &deny), def(Event::AfterTool, &deny)],
        );
        let chain = runner
            .run_event(Event::SessionEnd, None, 1, json!({}), None)
            .await;
        assert_eq!(chain.verdict, Verdict::Allow);
        assert!(
            chain.runs[0].reason.contains("ignored"),
            "{:?}",
            chain.runs[0]
        );
        let chain = runner
            .run_event(
                Event::AfterTool,
                Some("bash"),
                1,
                json!({"result": "r"}),
                None,
            )
            .await;
        assert_eq!(chain.verdict, Verdict::Allow);
    }

    #[tokio::test]
    async fn a_gate_that_hangs_is_killed_with_its_tree_and_allowed() {
        let _serial = Quiet(SERIAL.lock().await);
        let temp = tempfile::tempdir().unwrap();
        // A background child that would write a file in 3 s, and a hang. If the
        // tree is killed, the file never appears.
        let hang = answers(
            temp.path(),
            "hang.sh",
            "(sleep 3; echo alive > flag.txt) &\nsleep 60\n",
        );
        let runner = runner(temp.path(), vec![def(Event::TurnEnd, &hang)])
            .with_timeout(Duration::from_millis(800));
        let started = Instant::now();
        let chain = runner
            .run_event(Event::TurnEnd, None, 1, json!({"final_text": "x"}), None)
            .await;
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(chain.verdict, Verdict::Allow);
        assert!(
            chain.runs[0]
                .error
                .as_deref()
                .unwrap()
                .contains("timed out"),
            "{:?}",
            chain.runs[0]
        );
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            !temp.path().join("flag.txt").exists(),
            "the gate's tree outlived its timeout"
        );
    }

    #[tokio::test]
    async fn a_gate_that_exits_cleanly_leaves_no_background_process() {
        let _serial = Quiet(SERIAL.lock().await);
        let temp = tempfile::tempdir().unwrap();
        let leaves = answers(
            temp.path(),
            "leaves.sh",
            "(sleep 3; echo alive > flag.txt) >/dev/null 2>&1 &\necho '{\"decision\":\"allow\"}'\n",
        );
        let runner = runner(temp.path(), vec![def(Event::SessionEnd, &leaves)]);
        let chain = runner
            .run_event(Event::SessionEnd, None, 1, json!({}), None)
            .await;
        assert_eq!(chain.runs[0].error, None, "{:?}", chain.runs[0]);
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            !temp.path().join("flag.txt").exists(),
            "a leftover child survived the gate"
        );
    }

    #[tokio::test]
    async fn a_chatty_gate_cannot_wedge_the_pipe() {
        let _serial = Quiet(SERIAL.lock().await);
        let temp = tempfile::tempdir().unwrap();
        let chatty = answers(
            temp.path(),
            "chatty.sh",
            "i=0\nwhile [ $i -lt 40000 ]; do echo 0123456789012345678901234567890123456789; i=$((i+1)); done\n",
        );
        let runner = runner(temp.path(), vec![def(Event::SessionStart, &chatty)]);
        let chain = runner
            .run_event(Event::SessionStart, None, 1, json!({}), None)
            .await;
        assert!(
            chain.runs[0]
                .error
                .as_deref()
                .unwrap()
                .contains("output over"),
            "{:?}",
            chain.runs[0]
        );
        assert_eq!(chain.verdict, Verdict::Allow);
    }

    #[test]
    fn describe_names_environment_variables_but_never_their_values() {
        let mut gate = def(Event::TurnEnd, "sh x.sh");
        gate.env
            .insert("SECRET_THING".into(), "hunter2-canary".into());
        let text = serde_json::to_string(&describe(&[gate])).unwrap();
        assert!(text.contains("SECRET_THING"));
        assert!(!text.contains("hunter2-canary"), "{text}");
    }

    #[test]
    fn a_kelir_started_by_a_gate_runs_no_gates() {
        assert!(started_by_a_gate(Some("turn_end")));
        assert!(!started_by_a_gate(Some("  ")));
        assert!(!started_by_a_gate(None));
    }

    #[test]
    fn the_gate_processes_of_a_whole_family_are_bounded() {
        // The cap is per process; the family's total follows from the child caps.
        let live_children = crate::child::DEFAULT_MAX_LIVE as usize;
        assert_eq!(root_bound(live_children), 8 + 16 * 2);
        assert!(root_bound(live_children) <= 40);
        const { assert!(MAX_LIVE_CHILD >= 1 && MAX_LIVE_CHILD < MAX_LIVE) };
    }

    #[tokio::test]
    async fn the_live_cap_holds_when_many_runs_pile_up() {
        let _serial = Quiet(SERIAL.lock().await);
        let temp = tempfile::tempdir().unwrap();
        let slow = answers(
            temp.path(),
            "slow.sh",
            "sleep 2
",
        );
        let runner = std::sync::Arc::new(runner(temp.path(), vec![def(Event::SessionEnd, &slow)]));
        let mut peak = 0usize;
        let mut tasks = Vec::new();
        for _ in 0..(MAX_LIVE * 3) {
            let runner = runner.clone();
            tasks.push(tokio::spawn(async move {
                runner
                    .run_event(Event::SessionEnd, None, 1, json!({}), None)
                    .await
            }));
        }
        tokio::time::sleep(Duration::from_millis(1200)).await;
        peak = peak.max(live());
        let mut skipped = 0;
        for task in tasks {
            let chain = task.await.unwrap();
            if chain.runs[0]
                .error
                .as_deref()
                .is_some_and(|e| e.contains("already running"))
            {
                skipped += 1;
            }
            assert_eq!(chain.runs[0].decision, "allow");
        }
        assert!(peak <= MAX_LIVE, "{peak} gates were alive at once");
        assert!(skipped >= MAX_LIVE, "only {skipped} runs were skipped");
        assert_eq!(live(), 0, "a gate slot leaked");
    }

    #[tokio::test]
    async fn a_gate_that_spawns_itself_is_bounded_and_leaves_nothing() {
        let _serial = Quiet(SERIAL.lock().await);
        let temp = tempfile::tempdir().unwrap();
        // Each level starts two more, three levels deep at most (the script counts its own depth), and then
        // sleeps: 15 processes if nothing stopped it.
        let script = answers(
            temp.path(),
            "selfish.sh",
            "d=${DEPTH:-0}
if [ $d -lt 3 ]; then DEPTH=$((d+1)) bash \"$0\" & DEPTH=$((d+1)) bash \"$0\" & fi
(sleep 3; echo alive > flag.txt) &
sleep 1
",
        );
        let mut gate = def(Event::SessionEnd, &script);
        gate.env.insert("DEPTH".into(), "0".into());
        let runner = runner(temp.path(), vec![gate]).with_timeout(Duration::from_millis(600));
        let chain = runner
            .run_event(Event::SessionEnd, None, 1, json!({}), None)
            .await;
        assert_eq!(chain.runs.len(), 1);
        assert_eq!(live(), 0);
        // The script's own sleeps determine when a survivor would write `flag.txt`: 1 s in the gate and 3 s in
        // the detached child.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            !temp.path().join("flag.txt").exists(),
            "a descendant of the gate survived"
        );
    }

    // ------------------------------------------------ process sweep

    /// under a POSIX shell on Windows the `/C` of `cmd /C` must survive MSYS.
    #[test]
    fn a_cmd_line_is_adapted_to_a_posix_shell_on_windows_only() {
        let line = script_command(Path::new("C:/g/turn_end.cmd"));
        assert_eq!(line, "cmd /C \"C:/g/turn_end.cmd\"");
        let bash = adapt_for_shell(&line, "C:/Program Files/Git/bin/bash.exe");
        if cfg!(windows) {
            assert_eq!(bash, "cmd //C \"C:/g/turn_end.cmd\"");
            assert_eq!(
                adapt_for_shell("CMD.EXE /c x.bat", "sh"),
                "CMD.EXE //C x.bat"
            );
        } else {
            assert_eq!(bash, line);
        }
        // cmd and PowerShell take `/C` as is; other commands are never touched.
        assert_eq!(adapt_for_shell(&line, "C:/Windows/System32/cmd.exe"), line);
        assert_eq!(adapt_for_shell(&line, "pwsh"), line);
        assert_eq!(adapt_for_shell("python x.py /C", "bash"), "python x.py /C");
    }

    #[test]
    fn project_gates_are_recognised_and_dropped_when_untrusted() {
        let mut project = def(Event::TurnEnd, "echo a");
        project.origin = "project config".into();
        let mut skill = def(Event::TurnEnd, "echo b");
        skill.origin = "project skill watch".into();
        let machine = def(Event::TurnEnd, "echo c");
        assert!(is_project_gate(&project) && is_project_gate(&skill) && !is_project_gate(&machine));
        let all = vec![project, skill, machine];
        let mut notes = Vec::new();
        let kept = apply_trust(all.clone(), false, &mut notes);
        assert_eq!(kept.len(), 1);
        assert_eq!(notes.len(), 2, "{notes:?}");
        let mut none = Vec::new();
        assert_eq!(apply_trust(all, true, &mut none).len(), 3);
        assert!(none.is_empty());
    }

    #[test]
    fn a_trusted_directory_covers_what_is_under_it_and_nothing_beside_it() {
        let root = tempfile::tempdir().unwrap();
        let inside = root.path().join("a").join("b");
        std::fs::create_dir_all(&inside).unwrap();
        let beside = root.path().join("a-other");
        std::fs::create_dir_all(&beside).unwrap();
        let trusted = vec![root.path().join("a")];
        assert!(project_trusted(&inside, &trusted));
        assert!(project_trusted(&root.path().join("a"), &trusted));
        assert!(!project_trusted(&beside, &trusted));
        assert!(!project_trusted(&inside, &[]));
    }

    /// The macOS `ps e` parser matches only whole environment entries, never
    /// a prefix or a command word. No process is spawned.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_token_sweep_matches_only_exact_entries() {
        let ps = concat!(
            "  1 /sbin/launchd\n",
            " 42 bash gate.sh KELIR_GATE_RUN=token123 HOME=/x\n",
            " 43 sleep 3 KELIR_GATE_RUN=token1234\n",
            " 44 echo KELIR_GATE_RUN=token123 done\n",
            " 45 sh -c KELIR_GATE_RUN=token123 sleep\n",
        );
        let found = reap::parse_marked_pids(ps, "KELIR_GATE_RUN=token123");
        // Every whole field equal to the token counts, whether the `ps e` output carries it as a command word
        // or an environment entry.
        assert_eq!(found, [42, 44, 45].into_iter().collect());
        assert!(reap::parse_marked_pids(ps, "KELIR_GATE_RUN=zzz").is_empty());
    }

    /// Linux: the sweep finds a process that left the group by its token.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_sweep_kills_a_setsid_process_that_carries_the_token() {
        use std::os::unix::process::CommandExt as _;
        let token = run_token();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .env(RUN_ENV, &token)
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        // Not a descendant of the (unrelated) root, not in any group we signal.
        reap::kill_run(1_000_000, &token);
        let status = child.wait().unwrap();
        assert!(
            !status.success(),
            "the marked process survived the sweep ({pid})"
        );
    }
}
