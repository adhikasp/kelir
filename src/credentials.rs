//! Where each provider's key comes from (STEER-2).
//!
//! Exactly two providers, and each one has its own first-match-wins list:
//!
//! * **OpenRouter** — the variable `key_env` names (`OPENROUTER_API_KEY` by
//!   default), then `key_cmd` (a command whose stdout is the key).
//! * **OpenCode Go** — the same two as an *explicit override*, then opencode's
//!   own credential store, read through the supported `opencode auth export
//!   opencode` CLI. No SQLite, no reading `opencode.db`, and no OAuth refresh of
//!   our own: opencode rotates the refresh token, so a second refresher would
//!   break the account it shares with us.
//!
//! A key is read once at session start and lives in memory only. It is never
//! written to the log, a status file, an error message or this process's
//! command line: the only thing that ever leaves this module besides the key
//! itself is [`Origin::describe`], which names the *source*, never the secret.
//!
//! The read is bounded ([`AUTH_TIMEOUT`]) and silent (no window on Windows, no
//! inherited console, output never printed) — it runs on someone's desktop.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::config::{ProviderConfig, ProviderKind};

/// How long `opencode auth export` (or a `key_cmd`) may take before the whole
/// process tree is killed. It talks to opencode's background service, so it is
/// not instantaneous; 5 s is a bound, not an expectation (the dev box measures
/// ~0.34 s).
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(5);

/// A credential this close to its expiry is worth re-reading before it is used:
/// opencode refreshes its own token when it runs, so a second read may already
/// carry the new one.
pub const EXPIRY_MARGIN: Duration = Duration::from_secs(120);

/// The environment variable OpenRouter's key is read from when `key_env` says
/// nothing (the machine's usual one).
pub const OPENROUTER_ENV: &str = "OPENROUTER_API_KEY";

/// The explicit override for OpenCode Go's credential: set it and it wins over
/// opencode's own store.
pub const GO_ENV: &str = "KELIR_GO_API_KEY";

/// Where a key came from. Recorded and printed; the key itself is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The named environment variable.
    Env(String),
    /// A command's trimmed stdout.
    KeyCmd(Vec<String>),
    /// opencode's own credential store, through `opencode auth export opencode`.
    OpencodeStore,
}

impl Origin {
    /// A sentence for `kelir doctor` and the log: the source, never the secret.
    pub fn describe(&self) -> String {
        match self {
            Origin::Env(name) => format!("env {name}"),
            Origin::KeyCmd(argv) => format!("key_cmd {}", program_name(argv)),
            Origin::OpencodeStore => "opencode auth export opencode".to_string(),
        }
    }
}

/// A resolved key and everything about it that is safe to show.
#[derive(Clone)]
pub struct Credential {
    key: String,
    pub origin: Origin,
    /// Epoch milliseconds, when the source publishes an expiry (opencode's
    /// store does; an env var does not).
    pub expires_at_ms: Option<u64>,
    /// How long the read took. `kelir doctor` prints it; the session records it
    /// nowhere (it is not part of what the provider was asked for).
    pub read_ms: u64,
}

impl Credential {
    fn new(key: String, origin: Origin) -> Self {
        Self {
            key,
            origin,
            expires_at_ms: None,
            read_ms: 0,
        }
    }

    /// The secret. Callers put it in one place — the provider config the client
    /// holds — and nowhere else.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// A short, secret-free description: `env OPENROUTER_API_KEY`, say.
    pub fn describe(&self) -> String {
        self.origin.describe()
    }

    /// Has this credential already expired (`now_ms` is epoch milliseconds)?
    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.expires_at_ms
            .map(|expires| expires <= now_ms)
            .unwrap_or(false)
    }

    /// Expires soon enough that a re-read is worth doing before a request.
    pub fn expires_within_margin(&self, now_ms: u64) -> bool {
        self.expires_at_ms
            .map(|expires| expires.saturating_sub(now_ms) < EXPIRY_MARGIN.as_millis() as u64)
            .unwrap_or(false)
    }

    /// `expires 2026-11-24T09:51:54Z`, or `no expiry`.
    pub fn expiry_text(&self) -> String {
        match self.expires_at_ms {
            Some(ms) => format!("expires {}", format_ms(ms)),
            None => "no expiry".to_string(),
        }
    }
}

/// A key never comes out of a `{:?}`: a struct that holds one can be printed in
/// a panic message, and a panic message can land in a log.
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("origin", &self.origin.describe())
            .field("key", &"[redacted]")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

/// `kelir doctor` prints an `expires` and the fix for a stale one: both need a
/// readable UTC timestamp (epoch milliseconds, as opencode writes it).
pub fn format_ms(ms: u64) -> String {
    let nanos = (ms as i128) * 1_000_000;
    match time::OffsetDateTime::from_unix_timestamp_nanos(nanos) {
        Ok(at) => at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| ms.to_string()),
        Err(_) => ms.to_string(),
    }
}

/// The fix a person can actually apply when opencode's credential is stale.
pub const REFRESH_FIX: &str =
    "run any `opencode` command once so it refreshes its credential, then retry";

/// Resolve the key for one provider and store it on the config (in memory).
///
/// Blocking on purpose: it is a process read (opencode's own CLI), so an async
/// caller wraps it in `spawn_blocking` and a sync caller (a `model` verb that
/// swaps provider) may simply pay the bounded wait.
pub fn resolve(provider: &mut ProviderConfig) -> Result<Credential, String> {
    let env = |name: &str| std::env::var(name).ok();
    let run = |argv: &[String]| run_command(argv, AUTH_TIMEOUT);
    let credential = resolve_with(provider, &env, &run)?;
    // The one place the secret lands: memory, on the config the client holds.
    // Everything else records the *source* only.
    provider.key = Some(credential.key.clone());
    provider.key_origin = Some(credential.describe());
    provider.key_expires_at_ms = credential.expires_at_ms;
    Ok(credential)
}

/// The resolution with the environment and the command runner injected, so the
/// precedence rules are unit-testable without touching this process's env or
/// spawning anything.
///
/// First match wins. For OpenRouter that is the env var, then `key_cmd`. For
/// OpenCode Go an explicit env var or `key_cmd` is an override and comes first,
/// and opencode's own store is the default last resort — which is the point of
/// STEER-2: nobody has to copy opencode's token into a variable that goes
/// stale.
pub fn resolve_with(
    provider: &ProviderConfig,
    env: &dyn Fn(&str) -> Option<String>,
    run: &dyn Fn(&[String]) -> Result<String, String>,
) -> Result<Credential, String> {
    let default_env = match provider.kind {
        ProviderKind::OpenRouter => OPENROUTER_ENV,
        ProviderKind::OpencodeGo => GO_ENV,
    };
    let env_name = provider
        .key_env
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| default_env.to_string());
    if let Some(value) = env(&env_name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(Credential::new(value, Origin::Env(env_name)));
    }
    if let Some(argv) = provider.key_cmd.as_ref().filter(|argv| !argv.is_empty()) {
        let started = Instant::now();
        let stdout = run(argv).map_err(|error| {
            format!(
                "provider `{}`: key_cmd {}: {error}",
                provider.kind.name(),
                program_name(argv)
            )
        })?;
        let key = stdout.trim().to_string();
        if key.is_empty() {
            return Err(format!(
                "provider `{}`: key_cmd {} printed nothing (the key goes to stdout)",
                provider.kind.name(),
                program_name(argv)
            ));
        }
        let mut credential = Credential::new(key, Origin::KeyCmd(argv.clone()));
        credential.read_ms = started.elapsed().as_millis() as u64;
        return Ok(credential);
    }
    match provider.kind {
        ProviderKind::OpencodeGo => store_credential(run),
        ProviderKind::OpenRouter => Err(format!(
            "provider `openrouter`: missing API key: set {OPENROUTER_ENV} (or `key_env`), or `[providers.openrouter] key_cmd = [\"…\"]`"
        )),
    }
}

/// opencode's own credential, read through its supported CLI and re-read once
/// when it is already about to expire (opencode refreshes its token when it
/// runs, so the second read may be the fresh one).
fn store_credential(
    run: &dyn Fn(&[String]) -> Result<String, String>,
) -> Result<Credential, String> {
    let started = Instant::now();
    let mut credential = read_store(run)?;
    let now = crate::models::now_ms();
    if credential.expires_within_margin(now)
        && let Ok(fresh) = read_store(run)
        && !fresh.expires_within_margin(now)
        && fresh.expires_at_ms != credential.expires_at_ms
    {
        credential = fresh;
    }
    credential.read_ms = started.elapsed().as_millis() as u64;
    Ok(credential)
}

/// One `opencode auth export opencode` and its parse.
fn read_store(run: &dyn Fn(&[String]) -> Result<String, String>) -> Result<Credential, String> {
    let argv: Vec<String> = ["opencode", "auth", "export", "opencode"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let stdout = run(&argv).map_err(|error| {
        // The fix belongs in the message: a machine without opencode has two
        // answers, install it or name the key explicitly (STEER-2).
        format!(
            "provider `opencode-go`: opencode's credential store: {error}\
             (install opencode and run `opencode auth login`, or set {GO_ENV}\
             / `[providers.opencode-go] key_cmd`)"
        )
    })?;
    let token = parse_auth_export(&stdout, crate::models::now_ms())
        .map_err(|error| format!("provider `opencode-go`: opencode's credential store: {error}"))?;
    let key = token.access;
    Ok(Credential {
        key,
        origin: Origin::OpencodeStore,
        expires_at_ms: token.expires_at_ms,
        read_ms: 0,
    })
}

/// The one entry of `opencode auth export`'s JSON that is opencode's own,
/// active credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreToken {
    pub access: String,
    pub expires_at_ms: Option<u64>,
}

/// Parse `opencode auth export opencode` (a JSON array; the entry whose
/// `integrationID` is `opencode` and whose `active` is true; the token is
/// `value.access` and `value.expires` is epoch milliseconds).
///
/// An expired entry is an error that says when it expired and what a person can
/// do about it — never a request with a token that cannot work.
pub fn parse_auth_export(json: &str, now_ms: u64) -> Result<StoreToken, String> {
    let value: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|error| format!("auth export is not JSON ({error})"))?;
    let entries = value
        .as_array()
        .ok_or_else(|| "auth export is not a JSON array".to_string())?;
    let entry = entries
        .iter()
        .find(|entry| {
            entry["integrationID"].as_str() == Some("opencode")
                && entry["active"].as_bool() == Some(true)
        })
        .ok_or_else(|| {
            "no active `opencode` entry (log in with `opencode auth login`)".to_string()
        })?;
    let value = &entry["value"];
    let access = ["access", "key"]
        .into_iter()
        .find_map(|field| value[field].as_str())
        .map(str::trim)
        .filter(|access| !access.is_empty())
        .ok_or_else(|| {
            "the active `opencode` entry carries no `value.access` (log in again with `opencode auth login`)"
                .to_string()
        })?;
    // `expires` is epoch milliseconds. A string of digits is accepted too: the
    // CLI's JSON is stable, but the store is opencode's to change, and a
    // numeric string must not become "no expiry" (that would be a key held long
    // past its life).
    let expires_at_ms = value["expires"].as_u64().or_else(|| {
        value["expires"]
            .as_str()
            .and_then(|text| text.trim().parse().ok())
    });
    if let Some(expires) = expires_at_ms
        && expires <= now_ms
    {
        return Err(format!(
            "the opencode credential expired at {}: {REFRESH_FIX}",
            format_ms(expires)
        ));
    }
    Ok(StoreToken {
        access: access.to_string(),
        expires_at_ms,
    })
}

/// The first element of an argv, for a message that names the command but not
/// what it printed. Only its file name: `key_cmd ["C:/Users/someone/bin/getkey"]`
/// must not put a private path into the log (`session_started.key_origin`).
fn program_name(argv: &[String]) -> String {
    let program = argv.first().map(String::as_str).unwrap_or("<empty>").trim();
    Path::new(program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| program.to_string())
}

// ------------------------------------------------------------- running it

/// Run one helper command and return its trimmed stdout.
///
/// Bounded, silent and reaping:
///
/// * the child is in a process group of its own (Unix `setpgid`, Windows
///   `CREATE_NEW_PROCESS_GROUP`), and on Windows it never gets a console
///   window ([`crate::proc::win::CREATE_NO_WINDOW`]) even when kelir has none —
///   a console-subsystem child of a console-less parent would otherwise flash
///   one up;
/// * no stdin (the helper must never inherit a pipe a person is typing into);
/// * past `timeout` the **tree** is killed by pid/group and reaped, so a hung
///   helper cannot outlive the read (AGENTS.md: no leaked processes);
/// * a failure is a one-line sentence: the helper's own output is never echoed,
///   because a credential manager prints credentials.
pub fn run_command(argv: &[String], timeout: Duration) -> Result<String, String> {
    let program = argv
        .first()
        .ok_or_else(|| "empty command".to_string())?
        .clone();
    let args: Vec<String> = argv[1..].to_vec();
    let path =
        find_program(&program).ok_or_else(|| format!("`{program}` was not found on PATH"))?;
    let mut command = process_command(&path);
    command.args(&args);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = crate::proc::win::CREATE_NO_WINDOW;
        const CREATE_NEW_PROCESS_GROUP: u32 = crate::proc::win::CREATE_NEW_PROCESS_GROUP;
        command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    let started = Instant::now();
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start `{program}`: {error}"))?;
    let pid = child.id();
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = std::io::Read::read_to_string(pipe, &mut text);
        }
        text
    });
    let err_reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = std::io::Read::read_to_string(pipe, &mut text);
        }
        text
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(error) => {
                kill_tree(pid);
                let _ = child.wait();
                return Err(format!("`{program}`: cannot read its status: {error}"));
            }
        }
        if started.elapsed() >= timeout {
            kill_tree(pid);
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = out_reader.join().unwrap_or_default();
    let _ = err_reader.join();
    match status {
        Some(status) if status.success() => Ok(stdout.trim().to_string()),
        Some(status) => Err(match status.code() {
            Some(code) => format!("`{program}` exited with status {code}"),
            None => format!("`{program}` was killed before it finished"),
        }),
        None => Err(format!(
            "`{program}` did not answer within {} s and was stopped",
            timeout.as_secs()
        )),
    }
}

/// The `Command` for a program: a `.cmd`/`.bat` shim on Windows is not an
/// executable, so it runs through `cmd.exe /C` (the npm-style `opencode.cmd`
/// that most installs have); everything else starts directly.
fn process_command(path: &Path) -> std::process::Command {
    #[cfg(windows)]
    {
        let extension = path
            .extension()
            .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if extension == "cmd" || extension == "bat" {
            use std::os::windows::process::CommandExt as _;
            let shell = std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into());
            let mut command = std::process::Command::new(shell);
            command.arg("/C");
            // `raw_arg` so the program's own quoting survives: `cmd /C` treats
            // a quoted first word as the whole command line, so the doubled
            // quotes are the documented spelling for a path with a space in it.
            command.raw_arg(format!("\"{}\"", path.display()));
            return command;
        }
    }
    std::process::Command::new(path)
}

/// Find a program the way a shell would: `<name>` on Unix (executable bit) and
/// `<name>.exe`/`.cmd`/`.bat`/`.com` on Windows. A bare, extensionless file on
/// Windows is skipped: it is a sh script (npm ships one), and `CreateProcess`
/// cannot run it.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 {
        // An explicit path: use it as given (the `.cmd` wrapping still applies).
        return path.is_file().then(|| path.to_path_buf());
    }
    let dirs = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&dirs) {
        for candidate in candidates(name) {
            let full = dir.join(candidate);
            if is_executable(&full) {
                return Some(full);
            }
        }
    }
    None
}

fn candidates(name: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        ["exe", "cmd", "bat", "com"]
            .into_iter()
            .map(|extension| format!("{name}.{extension}"))
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec![name.to_string()]
    }
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Kill a tree this process started, by pid/group and never by image name.
fn kill_tree(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    {
        // Safe: the child leads the group this read created (`process_group(0)`),
        // so the signal reaches only it and its own children. Never group 0.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        crate::proc::terminate_tree(pid);
        crate::proc::win::terminate(pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A `ProviderConfig` with nothing but the fields the resolver reads.
    fn provider(
        kind: ProviderKind,
        key_env: Option<&str>,
        key_cmd: Option<&[&str]>,
    ) -> ProviderConfig {
        ProviderConfig {
            kind,
            base_url: "https://example.invalid/v1".into(),
            key_env: key_env.map(str::to_string),
            key_cmd: key_cmd.map(|argv| argv.iter().map(|part| part.to_string()).collect()),
            key: None,
            key_origin: None,
            key_expires_at_ms: None,
            extra_headers: BTreeMap::new(),
            reasoning_field: None,
            reasoning_effort: None,
            reasoning_effort_explicit: false,
            session_header: None,
            max_concurrent_requests: None,
        }
    }

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    fn no_commands(argv: &[String]) -> Result<String, String> {
        Err(format!("unexpected command {argv:?}"))
    }

    // ------------------------------------------------------- JSON shapes

    /// The export shape opencode writes today: an array, one entry per
    /// integration, `value.access` the token and `value.expires` epoch ms.
    const EXPORT: &str = r#"[
      {"id":"id-1","integrationID":"github","label":"gh","active":true,
       "value":{"type":"oauth","access":"gho_not_ours","expires":9999999999999}},
      {"id":"id-2","integrationID":"opencode","label":"opencode","active":true,
       "value":{"type":"oauth","methodID":"m","refresh":"r","access":"oc_sk_live",
                "expires":1795513914282,"metadata":{}}}
    ]"#;

    #[test]
    fn the_active_opencode_entry_is_the_one_that_counts() {
        let token = parse_auth_export(EXPORT, 1_700_000_000_000).unwrap();
        assert_eq!(token.access, "oc_sk_live");
        assert_eq!(token.expires_at_ms, Some(1_795_513_914_282));
    }

    #[test]
    fn an_inactive_or_missing_entry_is_an_error_that_names_the_fix() {
        // Inactive: the same shape with `active: false`.
        let inactive = EXPORT.replace("\"active\":true", "\"active\":false");
        let error = parse_auth_export(&inactive, 1).unwrap_err();
        assert!(error.contains("no active `opencode` entry"), "{error}");
        assert!(error.contains("opencode auth login"), "{error}");
        // Another integration only: also no entry.
        let other = r#"[{"integrationID":"github","active":true,"value":{"access":"gho_x"}}]"#;
        assert!(parse_auth_export(other, 1).is_err());
        // An empty store (fresh install).
        assert!(parse_auth_export("[]", 1).is_err());
    }

    #[test]
    fn an_expired_credential_says_when_it_expired_and_what_to_do() {
        // `expires` in the past: refusing a request with it is the whole point
        // (STEER-2: never send a credential that cannot work).
        let now = 1_800_000_000_000;
        let error = parse_auth_export(EXPORT, now).unwrap_err();
        assert!(error.contains("expired at"), "{error}");
        assert!(error.contains(REFRESH_FIX), "{error}");
        // And at the exact instant: expired, not "still good".
        assert!(
            parse_auth_export(EXPORT, 1_795_513_914_282)
                .unwrap_err()
                .contains("expired")
        );
        assert!(parse_auth_export(EXPORT, 1_795_513_914_281).is_ok());
    }

    #[test]
    fn malformed_json_and_a_missing_access_are_errors_not_panics() {
        for bad in ["", "not json", "{}", "{\"a\":1}", "[1,2,3]"] {
            let error = parse_auth_export(bad, 1).unwrap_err();
            assert!(!error.is_empty(), "{bad:?}");
        }
        // The right entry, no token.
        let no_access = r#"[{"integrationID":"opencode","active":true,"value":{"expires":1}}]"#;
        let error = parse_auth_export(no_access, 1).unwrap_err();
        assert!(error.contains("value.access"), "{error}");
        // The right entry, an empty token.
        let empty =
            r#"[{"integrationID":"opencode","active":true,"value":{"access":"  ","expires":1}}]"#;
        assert!(parse_auth_export(empty, 1).is_err());
    }

    #[test]
    fn extra_integrations_and_extra_fields_are_ignored() {
        let extra = r#"[
          {"integrationID":"opencode","active":false,"value":{"access":"stale"}},
          {"integrationID":"openrouter","active":true,"value":{"access":"sk_x"}},
          {"integrationID":"opencode","active":true,"value":{"access":"oc_sk_new","expires":"1795513914282","future":true}},
          {"integrationID":"opencode","active":true,"value":{"access":"oc_sk_second"}}
        ]"#;
        let token = parse_auth_export(extra, 1_700_000_000_000).unwrap();
        assert_eq!(token.access, "oc_sk_new", "the first active entry wins");
        // A numeric string is still an expiry, not "no expiry".
        assert_eq!(token.expires_at_ms, Some(1_795_513_914_282));
    }

    #[test]
    fn a_key_without_an_expiry_is_read_as_no_expiry() {
        let entry =
            r#"[{"integrationID":"opencode","active":true,"value":{"access":"oc_sk_forever"}}]"#;
        let token = parse_auth_export(entry, 1).unwrap();
        assert_eq!(token.access, "oc_sk_forever");
        assert_eq!(token.expires_at_ms, None);
    }

    // ------------------------------------------------------ precedence

    #[test]
    fn openrouter_takes_the_env_var_then_key_cmd() {
        let cfg = provider(ProviderKind::OpenRouter, None, Some(&["pass", "show"]));
        let credential = resolve_with(&cfg, &env_of(&[("OPENROUTER_API_KEY", "sk-env")]), &|_| {
            Ok("sk-cmd\n".into())
        })
        .unwrap();
        assert_eq!(credential.key(), "sk-env");
        assert_eq!(credential.origin.describe(), "env OPENROUTER_API_KEY");

        // No env: `key_cmd`'s trimmed stdout.
        let credential = resolve_with(&cfg, &env_of(&[]), &|argv| {
            assert_eq!(argv.first().map(String::as_str), Some("pass"));
            Ok("  sk-cmd \r\n".into())
        })
        .unwrap();
        assert_eq!(credential.key(), "sk-cmd");
        assert_eq!(credential.origin.describe(), "key_cmd pass");
        // An env var that is empty or blank is not a key.
        assert_eq!(
            resolve_with(&cfg, &env_of(&[("OPENROUTER_API_KEY", "   ")]), &|_| Ok(
                "sk-cmd".into()
            ))
            .unwrap()
            .key(),
            "sk-cmd"
        );
    }

    #[test]
    fn openrouter_without_a_source_says_what_to_set() {
        let cfg = provider(ProviderKind::OpenRouter, None, None);
        let error = resolve_with(&cfg, &env_of(&[]), &no_commands).unwrap_err();
        assert!(error.contains(OPENROUTER_ENV), "{error}");
        assert!(error.contains("key_cmd"), "{error}");
    }

    #[test]
    fn opencode_go_defaults_to_the_store_and_lets_an_explicit_source_win() {
        let cfg = provider(ProviderKind::OpencodeGo, None, None);
        let credential = resolve_with(&cfg, &env_of(&[]), &|argv| {
            assert_eq!(
                argv,
                ["opencode", "auth", "export", "opencode"]
                    .map(str::to_string)
                    .as_slice()
            );
            Ok(EXPORT.into())
        })
        .unwrap();
        assert_eq!(credential.key(), "oc_sk_live");
        assert_eq!(credential.origin, Origin::OpencodeStore);
        assert_eq!(
            credential.origin.describe(),
            "opencode auth export opencode",
            "a description names the source, never the secret"
        );

        // `KELIR_GO_API_KEY` (the default `key_env`) overrides the store, and
        // the store is never even read.
        let credential = resolve_with(&cfg, &env_of(&[(GO_ENV, "oc_sk_override")]), &|_| {
            panic!("the store must not be read when the override is set")
        })
        .unwrap();
        assert_eq!(credential.key(), "oc_sk_override");
        assert_eq!(credential.origin, Origin::Env(GO_ENV.to_string()));

        // `key_env` names another variable, and that wins too.
        let named = provider(ProviderKind::OpencodeGo, Some("MY_GO_KEY"), None);
        let credential = resolve_with(
            &named,
            &env_of(&[("MY_GO_KEY", "oc_sk_named"), (GO_ENV, "ignored")]),
            &|_| Ok(EXPORT.into()),
        )
        .unwrap();
        assert_eq!(credential.key(), "oc_sk_named");

        // `key_cmd` beats the store as well.
        let cmd = provider(ProviderKind::OpencodeGo, None, Some(&["go-key"]));
        let credential =
            resolve_with(&cmd, &env_of(&[]), &|_| Ok("oc_sk_from_cmd".into())).unwrap();
        assert_eq!(credential.key(), "oc_sk_from_cmd");
    }

    #[test]
    fn a_key_cmd_that_fails_or_prints_nothing_names_itself() {
        let cfg = provider(ProviderKind::OpenRouter, None, Some(&["pass", "show"]));
        let error =
            resolve_with(&cfg, &env_of(&[]), &|_| Err("exited with status 2".into())).unwrap_err();
        assert!(error.contains("key_cmd pass"), "{error}");
        assert!(!error.contains("status 2\n"), "{error}");
        let error = resolve_with(&cfg, &env_of(&[]), &|_| Ok("\n  \n".into())).unwrap_err();
        assert!(error.contains("printed nothing"), "{error}");
    }

    /// A `key_cmd` is named by its file name: a message and the log must not
    /// carry a private path (STEER-2).
    #[test]
    fn a_command_is_named_without_its_directory() {
        let argv =
            |parts: &[&str]| -> Vec<String> { parts.iter().map(|part| part.to_string()).collect() };
        assert_eq!(program_name(&argv(&["pass", "show"])), "pass");
        assert_eq!(
            program_name(&argv(&["/home/someone/bin/getkey", "--flag"])),
            "getkey"
        );
        assert_eq!(
            program_name(&argv(&[r"C:\Users\someone\bin\getkey.cmd"])),
            "getkey.cmd"
        );
        assert_eq!(program_name(&[]), "<empty>");
    }

    // -------------------------------------------------------- freshness

    fn credential_at(expires: Option<u64>) -> Credential {
        Credential {
            key: "oc_sk_x".into(),
            origin: Origin::OpencodeStore,
            expires_at_ms: expires,
            read_ms: 0,
        }
    }

    #[test]
    fn expiry_and_its_two_minute_margin() {
        let now = 1_000_000_000_000;
        let plain = credential_at(None);
        assert!(!plain.is_expired(now));
        assert!(!plain.expires_within_margin(now), "no expiry, no re-read");
        assert_eq!(plain.expiry_text(), "no expiry");

        let margin = EXPIRY_MARGIN.as_millis() as u64;
        assert!(!credential_at(Some(now + margin + 1)).expires_within_margin(now));
        assert!(credential_at(Some(now + margin - 1)).expires_within_margin(now));
        assert!(credential_at(Some(now)).expires_within_margin(now));
        assert!(credential_at(Some(now)).is_expired(now));
        assert!(credential_at(Some(now - 1)).is_expired(now));
        assert!(!credential_at(Some(now + 1)).is_expired(now));
    }

    #[test]
    fn a_credential_inside_the_margin_is_read_again_once() {
        let now = crate::models::now_ms();
        let soon = now + 60_000;
        let later = now + 3_600_000;
        let reads = std::cell::Cell::new(0);
        let credential = store_credential(&|_| {
            reads.set(reads.get() + 1);
            let expires = if reads.get() == 1 { soon } else { later };
            Ok(format!(
                r#"[{{"integrationID":"opencode","active":true,"value":{{"access":"oc_sk_{}","expires":{expires}}}}}]"#,
                reads.get()
            ))
        })
        .unwrap();
        assert_eq!(reads.get(), 2, "one re-read");
        assert_eq!(credential.key(), "oc_sk_2", "the fresher read wins");
        assert_eq!(credential.expires_at_ms, Some(later));

        // A re-read that hands back the same expiry is kept as it was (no
        // spinning, and no second request with a token that is about to die).
        let reads = std::cell::Cell::new(0);
        let credential = store_credential(&|_| {
            reads.set(reads.get() + 1);
            Ok(format!(
                r#"[{{"integrationID":"opencode","active":true,"value":{{"access":"oc_sk_same","expires":{soon}}}}}]"#
            ))
        })
        .unwrap();
        assert_eq!(reads.get(), 2);
        assert_eq!(credential.expires_at_ms, Some(soon));

        // Far from its expiry: one read, no second one.
        let reads = std::cell::Cell::new(0);
        let credential = store_credential(&|_| {
            reads.set(reads.get() + 1);
            Ok(format!(
                r#"[{{"integrationID":"opencode","active":true,"value":{{"access":"oc_sk_ok","expires":{later}}}}}]"#
            ))
        })
        .unwrap();
        assert_eq!(reads.get(), 1);
        assert_eq!(credential.key(), "oc_sk_ok");
    }

    // -------------------------------------------------------- redaction

    #[test]
    fn nothing_that_describes_a_key_can_print_it() {
        let credential = Credential {
            key: "oc_sk_super_secret_value".into(),
            origin: Origin::OpencodeStore,
            expires_at_ms: Some(1_795_513_914_282),
            read_ms: 12,
        };
        let shown = format!("{credential:?} {}", credential.describe());
        assert!(!shown.contains("super_secret"), "{shown}");
        assert!(shown.contains("[redacted]"), "{shown}");
        assert!(shown.contains("opencode auth export opencode"), "{shown}");
        // The expiry is rendered as an RFC 3339 instant, not a raw number.
        assert!(
            credential.expiry_text().contains('T'),
            "{:?}",
            credential.expiry_text()
        );
    }

    // --------------------------------------------------------- running

    #[test]
    fn a_program_is_found_on_path_and_not_invented() {
        assert!(find_program("definitely-not-a-real-program-42").is_none());
        // `cmd` exists on Windows only; on Unix try `sh`, always there.
        #[cfg(windows)]
        assert!(find_program("cmd").is_some(), "the .exe/.cmd search");
        #[cfg(unix)]
        assert!(find_program("sh").is_some());
    }

    /// The read's three hard rules, driven for real: a timeout kills the tree,
    /// a failure says why without echoing output, and stdout is trimmed.
    #[test]
    fn a_helper_is_bounded_and_silent() {
        #[cfg(unix)]
        {
            let out = run_command(
                &["sh".into(), "-c".into(), "printf '  oc_sk_x \\n'".into()],
                Duration::from_secs(5),
            )
            .unwrap();
            assert_eq!(out, "oc_sk_x");
            // A failure carries the exit status, never the helper's stderr: a
            // credential manager prints credentials.
            let error = run_command(
                &[
                    "sh".into(),
                    "-c".into(),
                    "echo oc_sk_leaked >&2; exit 3".into(),
                ],
                Duration::from_secs(5),
            )
            .unwrap_err();
            assert!(error.contains("status 3"), "{error}");
            assert!(!error.contains("leaked"), "{error}");
            // A helper that never answers is killed, not waited for.
            let started = Instant::now();
            let crew = ["sleep".to_string(), "30".to_string()];
            let error = run_command(&crew, Duration::from_millis(300)).unwrap_err();
            assert!(error.contains("did not answer"), "{error}");
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "{:?}",
                started.elapsed()
            );
        }
        #[cfg(windows)]
        {
            let out = run_command(
                &["cmd".into(), "/C".into(), "echo oc_sk_x".into()],
                Duration::from_secs(5),
            )
            .unwrap();
            assert_eq!(out, "oc_sk_x");
            let error = run_command(
                &[
                    "cmd".into(),
                    "/C".into(),
                    "echo oc_sk_leaked 1>&2 & exit 3".into(),
                ],
                Duration::from_secs(5),
            )
            .unwrap_err();
            assert!(error.contains("status 3"), "{error}");
            assert!(!error.contains("leaked"), "{error}");
            let started = Instant::now();
            let crew = [
                "cmd".into(),
                "/C".into(),
                "ping -n 30 127.0.0.1 > NUL".into(),
            ];
            let error = run_command(&crew, Duration::from_millis(300)).unwrap_err();
            assert!(error.contains("did not answer"), "{error}");
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "{:?}",
                started.elapsed()
            );
        }
    }
}
