//! Named sessions: `names/<name>` -> a session id, and `kelir sessions`.
//! A name is a file in `<state>/names/` holding one small JSON object.
//! It is published create-or-fail ([`crate::atomic::publish_new`]) so two processes naming one
//! session at once cannot both win, and replaced with a rename so a reader never sees half of one.
//! A name whose file is unreadable is tolerated: it reads as "no such name" (with a warning) and

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atomic::{Published, publish_new, write_atomic};

pub const MAX_LEN: usize = 64;
pub const OWNER_FILE: &str = "owner";

/// Files in `names/` that belong to a scheduled run, not to a name binding.
/// Keeping them out of `validate()` stops a name from clobbering a STATUS record
/// (and a STATUS record from being listed as a phantom name).
fn is_run_file(name: &str) -> bool {
    name.ends_with(".status.json")
        || name.ends_with(".lock")
        || name.contains(".stale.")
        || name.ends_with(".lock.stale")
}

/// `[a-z0-9._-]{1,64}`, and nothing that would be a different file on another platform: it starts with
/// a letter or digit, does not end in a dot (Windows drops it), and is not a device name.
pub fn validate(name: &str) -> Result<(), String> {
    if is_run_file(name) {
        return Err(format!("`{name}` is reserved for scheduled-run files"));
    }
    if name.is_empty() || name.len() > MAX_LEN {
        return Err(format!(
            "a name is 1 to {MAX_LEN} characters of a-z, 0-9, `.`, `_` and `-` (got {} characters)",
            name.chars().count()
        ));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')))
    {
        return Err(format!(
            "`{name}`: a name may hold only a-z, 0-9, `.`, `_` and `-` (found {bad:?})"
        ));
    }
    if !name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit()) {
        return Err(format!("`{name}`: a name starts with a letter or a digit"));
    }
    if name.ends_with('.') {
        return Err(format!("`{name}`: a name does not end in a dot"));
    }
    let stem = name.split('.').next().unwrap_or(name);
    let reserved = matches!(stem, "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        return Err(format!("`{name}` is a reserved device name on Windows"));
    }
    Ok(())
}

/// A session id as kelir mints them; anything else read from disk is refused so
/// a tampered name file cannot point outside `sessions/`.
pub fn valid_session_id(id: &str) -> bool {
    id.len() > 4
        && id.len() <= 64
        && id.starts_with("ses_")
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

pub fn dir() -> PathBuf {
    crate::paths::state_dir().join("names")
}

fn path_of(name: &str) -> PathBuf {
    dir().join(name)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub session: String,
    pub created: String,
    pub updated: String,
}

impl Entry {
    fn new(session: &str) -> Entry {
        let now = crate::event::now_ts();
        Entry {
            session: session.to_string(),
            created: now.clone(),
            updated: now,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut text = serde_json::to_string(self).unwrap_or_default();
        text.push('\n');
        text.into_bytes()
    }
}

fn parse(text: &str) -> Result<Entry, String> {
    let entry: Entry = serde_json::from_str(text).map_err(|error| format!("not JSON: {error}"))?;
    if !valid_session_id(&entry.session) {
        return Err("it does not name a session".into());
    }
    Ok(entry)
}

/// The name's entry: `Ok(None)` when there is no such name, `Err` when the file
/// is there but unreadable.
pub fn read(name: &str) -> Result<Option<Entry>, String> {
    validate(name)?;
    match std::fs::read_to_string(path_of(name)) {
        Ok(text) => parse(&text)
            .map(Some)
            .map_err(|why| format!("names/{name} is unreadable ({why})")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read names/{name}: {error}")),
    }
}

/// [`read`], with an unreadable file reported on stderr and treated as absent.
pub fn read_lenient(name: &str) -> Result<Option<Entry>, String> {
    validate(name)?;
    match read(name) {
        Err(message) if message.contains("unreadable") => {
            eprintln!("kelir: warning: {message}; treating the name as free");
            Ok(None)
        }
        other => other,
    }
}

fn ensure_dir() -> Result<(), String> {
    crate::paths::create_private_dir(&dir()).map_err(|error| {
        format!(
            "cannot create {}: {error}",
            crate::util::path_string(&dir())
        )
    })
}

/// Bind `name` to `id` if the name is free. `Ok(false)` means somebody holds it.
pub fn bind_new(name: &str, id: &str) -> Result<bool, String> {
    validate(name)?;
    ensure_dir()?;
    let entry = Entry::new(id);
    match publish_new(&path_of(name), &entry.bytes()) {
        Ok(Published::Created) => Ok(true),
        Ok(Published::Exists) => Ok(false),
        Err(error) => Err(format!("cannot write names/{name}: {error}")),
    }
}

/// Point `name` at `id`, replacing what it pointed at (a job that starts a
/// fresh session). `created` is kept when the name already existed.
pub fn set(name: &str, id: &str) -> Result<(), String> {
    validate(name)?;
    ensure_dir()?;
    let mut entry = Entry::new(id);
    if let Ok(Some(old)) = read(name) {
        entry.created = old.created;
    }
    write_atomic(&path_of(name), &entry.bytes())
        .map_err(|error| format!("cannot write names/{name}: {error}"))
}

/// `sessions rename`: the new name must be free.
pub fn rename(old: &str, new: &str) -> Result<(), String> {
    validate(old)?;
    validate(new)?;
    let Some(entry) = read(old)? else {
        return Err(format!("no session is named `{old}`"));
    };
    if old == new {
        return Ok(());
    }
    let moved = Entry {
        updated: crate::event::now_ts(),
        ..entry
    };
    match publish_new(&path_of(new), &moved.bytes()) {
        Ok(Published::Created) => {}
        Ok(Published::Exists) => return Err(format!("the name `{new}` is already taken")),
        Err(error) => return Err(format!("cannot write names/{new}: {error}")),
    }
    std::fs::remove_file(path_of(old))
        .map_err(|error| format!("`{new}` is bound, but names/{old} could not be removed: {error}"))
}

/// `sessions forget`: drop the name, keep the session. `Ok(false)` when there
/// was no such name.
pub fn forget(name: &str) -> Result<bool, String> {
    validate(name)?;
    match std::fs::remove_file(path_of(name)) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("cannot remove names/{name}: {error}")),
    }
}

#[derive(Debug, Clone)]
pub struct Listed {
    pub name: String,
    pub entry: Result<Entry, String>,
}

/// Every name, sorted; an unreadable file is a row with its reason, never a
/// reason to stop listing.
pub fn list() -> Vec<Listed> {
    let mut rows: Vec<Listed> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|item| item.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.') && !is_run_file(name) && validate(name).is_ok())
        .map(|name| {
            let entry = match std::fs::read_to_string(path_of(&name)) {
                Ok(text) => parse(&text),
                Err(error) => Err(error.to_string()),
            };
            Listed { name, entry }
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

// ------------------------------------------------------------- the owner file

/// Record that this process owns the session in `dir` (pid and start time), so a refusal can
/// say who holds it. Best effort: the real exclusion is the session's lock and socket.
pub fn write_owner(dir: &Path) {
    let pid = std::process::id();
    let body = serde_json::json!({
        "pid": pid,
        "start": crate::child::start_ms(pid),
    });
    let _ = write_atomic(&dir.join(OWNER_FILE), body.to_string().as_bytes());
}

pub fn release_owner(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(OWNER_FILE));
}

/// The pid of the live process that owns the session in `dir`, if any. A pid
/// whose start time no longer matches (reuse) is not an owner.
pub fn holder(dir: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(dir.join(OWNER_FILE)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let pid = u32::try_from(value.get("pid")?.as_u64()?).ok()?;
    let start = value.get("start").and_then(|start| start.as_u64());
    crate::child::process_alive(pid, start).then_some(pid)
}

// ------------------------------------------------------- run --name / resolve

/// What `--name` / `session = "…"` resolved to.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// A session already holds the name and has a log: resume it.
    Resume(String),
    /// The name is free or points at a session with no log yet: start a session
    /// with exactly this id (the name is bound to it already).
    New(String),
}

/// Resolve a name to a session, binding a free name to a freshly minted id.
/// Refuses (with the holder's pid) a name whose session is running.
pub fn resolve_for_run(name: &str) -> Result<Target, String> {
    validate(name)?;
    for _ in 0..3 {
        match read_lenient(name)? {
            Some(entry) => return existing(name, entry),
            None => {
                let id = new_session_id();
                // An unreadable file reads as free; replace it rather than loop.
                let bound = if path_of(name).exists() {
                    set(name, &id).map(|()| true)?
                } else {
                    bind_new(name, &id)?
                };
                if bound {
                    return Ok(Target::New(id));
                }
                // Lost the race to another process binding the same name:
                // read again and follow what it bound.
            }
        }
    }
    Err(format!(
        "cannot settle the name `{name}`: it keeps changing"
    ))
}

fn existing(name: &str, entry: Entry) -> Result<Target, String> {
    let dir = crate::paths::session_dir(&entry.session);
    if let Some(pid) = holder(&dir) {
        return Err(format!(
            "the name `{name}` belongs to session {} which is running (pid {pid}); \
             wait for it, `kelir ctl` it, or pick another name",
            entry.session
        ));
    }
    if dir.join("events.jsonl").is_file() {
        Ok(Target::Resume(entry.session))
    } else {
        Ok(Target::New(entry.session))
    }
}

pub fn new_session_id() -> String {
    format!("ses_{}", ulid::Ulid::generate().to_string().to_lowercase())
}

// ------------------------------------------------------------- kelir sessions

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub session: String,
    pub name: Option<String>,
    pub state: String,
    pub model: String,
    pub started: String,
    pub last_activity: String,
    pub steps: usize,
    pub tokens: u64,
    pub cache_hit_ratio: Option<f64>,
    pub pid: Option<u32>,
    pub parent: Option<String>,
    /// Outcome of the last single-flight run bound to this name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// Summarise one session from its log, or `None` when it has none.
pub fn summarize(id: &str, names: &std::collections::HashMap<String, String>) -> Option<Summary> {
    let dir = crate::paths::session_dir(id);
    let read = crate::event::read_events(&dir.join("events.jsonl")).ok()?;
    let events = read.events;
    let first = events.first()?;
    let (mut model, mut parent) = (String::new(), None);
    for event in &events {
        match &event.body {
            crate::event::EventBody::SessionStarted {
                model: m,
                parent: p,
                ..
            } => {
                model = m.clone();
                parent = p.clone();
            }
            crate::event::EventBody::StepStarted { model: m, .. } => model = m.clone(),
            _ => {}
        }
    }
    let tally = crate::usage::Tally::from_events(&events, crate::usage::Warn::default());
    let pid = holder(&dir);
    let state = if pid.is_some() {
        "running".to_string()
    } else if matches!(
        events.last().map(|event| &event.body),
        Some(crate::event::EventBody::SessionEnded { .. })
    ) && tally.state == "running"
    {
        "stopped".to_string()
    } else if tally.state == "running" {
        // The log ends mid-turn and nothing owns it: a crash.
        "crashed".to_string()
    } else {
        tally.state.clone()
    };
    Some(Summary {
        session: id.to_string(),
        name: names.get(id).cloned(),
        state,
        model,
        started: first.ts.clone(),
        last_activity: events.last().map(|e| e.ts.clone()).unwrap_or_default(),
        steps: events
            .iter()
            .filter(|e| matches!(e.body, crate::event::EventBody::StepStarted { .. }))
            .count(),
        tokens: tally.prompt + tally.completion,
        cache_hit_ratio: (tally.prompt > 0).then(|| tally.cached as f64 / tally.prompt as f64),
        pid,
        parent,
        outcome: names
            .iter()
            .find(|(session, _)| session.as_str() == id)
            .and_then(|(_, name)| crate::namedrun::read_status(name).ok().flatten())
            .map(|status| status.outcome),
    })
}

fn name_index() -> std::collections::HashMap<String, String> {
    list()
        .into_iter()
        .filter_map(|row| row.entry.ok().map(|entry| (entry.session, row.name)))
        .collect()
}

/// How many unnamed root sessions `sessions ls` adds to the named ones.
const RECENT: usize = 20;

pub fn sessions_ls(args: &crate::cli::SessionsLsArgs) -> u8 {
    let index = name_index();
    let mut rows: Vec<Summary> = Vec::new();
    let mut unnamed = 0usize;
    for id in crate::paths::sessions_newest_first() {
        let named = index.contains_key(&id);
        if args.named && !named {
            continue;
        }
        if !args.all && !named && unnamed >= RECENT {
            continue;
        }
        let Some(summary) = summarize(&id, &index) else {
            continue;
        };
        if !args.all && summary.parent.is_some() {
            continue;
        }
        if !named {
            unnamed += 1;
        }
        rows.push(summary);
    }
    // A name whose session has no log yet (bound, never started) still shows.
    let listed: std::collections::HashSet<&str> =
        rows.iter().map(|row| row.session.as_str()).collect();
    let mut dangling: Vec<Summary> = Vec::new();
    for row in list() {
        match row.entry {
            Ok(entry) if !listed.contains(entry.session.as_str()) => {
                if !crate::paths::session_dir(&entry.session)
                    .join("events.jsonl")
                    .is_file()
                {
                    dangling.push(Summary {
                        session: entry.session,
                        name: Some(row.name.clone()),
                        state: "new".into(),
                        model: String::new(),
                        started: entry.created.clone(),
                        last_activity: entry.updated,
                        steps: 0,
                        tokens: 0,
                        cache_hit_ratio: None,
                        pid: None,
                        parent: None,
                        outcome: crate::namedrun::read_status(&row.name)
                            .ok()
                            .flatten()
                            .map(|status| status.outcome),
                    });
                }
            }
            Err(why) => eprintln!("kelir: warning: names/{}: {why}", row.name),
            _ => {}
        }
    }
    rows.extend(dangling);
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into())
        );
        return 0;
    }
    if rows.is_empty() {
        println!("no sessions");
        return 0;
    }
    println!(
        "{:<20} {:<30} {:<9} {:<20} {:>9} {:>6} {:<9}",
        "NAME", "SESSION", "STATE", "LAST ACTIVITY", "TOKENS", "CACHE", "OUTCOME"
    );
    for row in &rows {
        println!(
            "{:<20} {:<30} {:<9} {:<20} {:>9} {:>6} {:<9}",
            row.name.as_deref().unwrap_or("-"),
            row.session,
            row.state,
            row.last_activity.chars().take(19).collect::<String>(),
            row.tokens,
            row.cache_hit_ratio
                .map_or("-".to_string(), |ratio| format!("{:.0}%", ratio * 100.0)),
            row.outcome.as_deref().unwrap_or("-")
        );
    }
    0
}

pub fn sessions_rename(args: &crate::cli::SessionsRenameArgs) -> u8 {
    match rename(&args.old, &args.new) {
        Ok(()) => {
            println!("renamed `{}` to `{}`", args.old, args.new);
            0
        }
        Err(message) => {
            eprintln!("kelir: {message}");
            1
        }
    }
}

pub fn sessions_forget(args: &crate::cli::SessionsForgetArgs) -> u8 {
    match forget(&args.name) {
        Ok(true) => {
            println!("forgot `{}` (the session and its log are kept)", args.name);
            0
        }
        Ok(false) => {
            eprintln!("kelir: no session is named `{}`", args.name);
            1
        }
        Err(message) => {
            eprintln!("kelir: {message}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A job releases its session owner before its lock, so a waiter on that
    /// lock never sees a still-alive owner marker.
    #[test]
    fn releasing_the_owner_makes_the_session_resumable() {
        let dir = std::env::temp_dir().join(format!(
            "kelir-owner-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        write_owner(&dir);
        assert_eq!(holder(&dir), Some(std::process::id()));
        release_owner(&dir);
        assert_eq!(holder(&dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn names_are_lowercase_words_of_at_most_64() {
        for good in ["a", "nightly", "a.b_c-d", "0day", &"x".repeat(64)] {
            assert!(validate(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "Nightly",
            "has space",
            "a/b",
            "a\\b",
            "..",
            ".hidden",
            "-x",
            "trailing.",
            "naïve",
            "con",
            "nul.txt",
            "com1",
            "lpt9",
            "nightly.status.json",
            "nightly.lock",
            "nightly.stale.123.01abc.456",
            "nightly.lock.stale.deadbeef",
            &"x".repeat(65),
        ] {
            assert!(validate(bad).is_err(), "{bad:?} should be refused");
        }
        // `com10` is an ordinary name; only com0-9 are devices.
        assert!(validate("com10").is_ok());
    }

    #[test]
    fn a_tampered_session_id_is_not_a_session() {
        assert!(valid_session_id("ses_01hzzz"));
        for bad in ["", "ses_", "../x", "ses_a/b", "ses_A", "xses_1", "ses_a\\b"] {
            assert!(!valid_session_id(bad), "{bad}");
        }
        assert!(parse(r#"{"session":"../../etc","created":"x","updated":"x"}"#).is_err());
        assert!(parse("not json").is_err());
        assert!(parse(r#"{"session":"ses_ok1","created":"a","updated":"b"}"#).is_ok());
    }
}
