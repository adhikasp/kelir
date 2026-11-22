//! The session socket's trust boundary: who may drive this session, and which jobs it will take
//! reports for. A per-session token and a registry of owned jobs close it; file presence and
//! content are never evidence of ownership. Same-user processes are trusted by design — the model
//! gets a real shell as that user — so these checks keep *other* users out and stop a tool child
//! posing as a job shim.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::ipc::Request;

/// The variable a session's own tool children read the token from.
pub const TOKEN_ENV: &str = "KELIR_TOKEN";
/// The file `kelir ctl` (a human's terminal) reads the token from.
pub const TOKEN_FILE: &str = "token";
/// Bytes of randomness in a token; 256 bits.
pub const TOKEN_BYTES: usize = 32;
/// The longest job id the session will look at. Ids are `bg_<hex>` or `call_<hex>` here, both far
/// shorter.
pub const MAX_ID: usize = 128;
/// Bytes of randomness in a job id: 128 bits.
pub const ID_BYTES: usize = 16;
/// How many refused `bg`/`cancel`/`bg-spawn` verbs one session records in its log before it answers
/// without logging.
pub const REFUSAL_BUDGET: u32 = 32;
/// How many job ids one session may still mint on request (`bg-claim`).
pub const CLAIM_BUDGET: u32 = 256;
/// How many acknowledgement ids `bg-claim` may hand out.
pub const ACK_BUDGET: u32 = 16;

// ------------------------------------------------------------------ the token

/// A fresh token: 64 lowercase hex characters from the OS's randomness, or empty
/// when the OS has no entropy (the caller then refuses to start).
pub fn new_token() -> String {
    random_bytes::<TOKEN_BYTES>()
        .map(|bytes| hex(&bytes))
        .unwrap_or_default()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// OS randomness. `SystemRng::fill_bytes` panics when the OS has no entropy; catch it so the
/// callers' emptiness checks can refuse to start or mint rather than abort the session. An empty
/// result is never used as a token or an id (no predictable fallback).
fn random_bytes<const N: usize>() -> Option<[u8; N]> {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::random::{Rng as _, SystemRng};
    catch_unwind(AssertUnwindSafe(|| {
        let mut bytes = [0u8; N];
        SystemRng.fill_bytes(&mut bytes);
        bytes
    }))
    .ok()
}

/// Random hex of `bytes` bytes, used for job and child ids. Empty when the OS has no entropy.
pub fn random_hex(bytes: usize) -> String {
    random_bytes_vec(bytes)
        .map(|buf| hex(&buf))
        .unwrap_or_default()
}

fn random_bytes_vec(len: usize) -> Option<Vec<u8>> {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::random::{Rng as _, SystemRng};
    catch_unwind(AssertUnwindSafe(|| {
        let mut buf = vec![0u8; len];
        SystemRng.fill_bytes(&mut buf);
        buf
    }))
    .ok()
}

/// Compare two secrets without telling a caller how far it got. Length is not a secret (every
/// token is the same length).
pub fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in a.iter().zip(b) {
        diff |= left ^ right;
    }
    diff == 0
}

/// Write the token where `kelir ctl` looks for it, owner-only.
pub fn write_token(dir: &Path, token: &str) -> Result<(), String> {
    let path = dir.join(TOKEN_FILE);
    std::fs::write(&path, token.as_bytes())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    restrict_to_owner(&path)
}

/// The token file's contents, when there is one.
pub fn read_token(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(TOKEN_FILE)).ok()?;
    let token = text.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// The token a session exported to this process, when this process is one of its children.
pub fn token_from_env() -> Option<String> {
    crate::envs::var(TOKEN_ENV)
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

/// Make a directory owner-only, the same way [`restrict_to_owner`] makes a file: `0700` on Unix and
/// the owner-only DACL on Windows.
pub fn restrict_dir_to_owner(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot restrict {}: {error}", dir.display()))
    }
    #[cfg(windows)]
    {
        crate::perm::restrict_file_to_owner(dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Make a file's permissions owner-only: `0600` on Unix, and on Windows a protected DACL with
/// one `FILE_ALL_ACCESS` ACE for this process's user.
pub fn restrict_to_owner(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("cannot restrict {}: {error}", path.display()))
    }
    #[cfg(windows)]
    {
        crate::perm::restrict_file_to_owner(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}

// --------------------------------------------------------------------- ids

/// A job id this session may be told about: one path component, no traversal, nothing a status
/// path could be built out of that lands elsewhere.
pub fn valid_job_id(id: &str) -> bool {
    if id.trim() != id {
        return false;
    }
    if id.is_empty()
        || id.len() > MAX_ID
        || id.starts_with('.')
        || id.contains("..")
        || id.ends_with('.')
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return false;
    }
    !is_windows_device_name(id)
}

/// `CON`, `NUL`, `COM1`… and their `name.ext` spellings, which Windows resolves to a device
/// whatever the extension.
fn is_windows_device_name(id: &str) -> bool {
    let stem = id.split('.').next().unwrap_or(id).trim_end();
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    if upper.len() == 4 {
        let (head, digit) = upper.split_at(3);
        if matches!(head, "COM" | "LPT") && digit.as_bytes()[0].is_ascii_digit() {
            return true;
        }
    }
    let chars: Vec<char> = upper.chars().collect();
    chars.len() == 4
        && matches!(chars[..3], ['C', 'O', 'M'] | ['L', 'P', 'T'])
        && matches!(chars[3], '\u{b9}' | '\u{b2}' | '\u{b3}')
}

// ------------------------------------------------------------------- paths

/// Resolve `candidate` (absolute, or relative to `base`) and prove it lands inside `base`.
pub fn resolve_inside(base: &Path, candidate: &str) -> Result<PathBuf, String> {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return Err("an empty path".to_string());
    }
    if candidate.starts_with("\\\\") || candidate.starts_with("//") {
        return Err(format!("`{candidate}` is a UNC or device path"));
    }
    let base_real = std::fs::canonicalize(base)
        .map_err(|error| format!("cannot resolve {}: {error}", base.display()))?;
    let joined = {
        let path = Path::new(candidate);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            base_real.join(path)
        }
    };
    let real = resolve_existing(&joined)?;
    if !real.starts_with(&base_real) {
        return Err(format!(
            "`{candidate}` resolves to {}, which is outside {}",
            real.display(),
            base_real.display()
        ));
    }
    Ok(real)
}

fn resolve_existing(path: &Path) -> Result<PathBuf, String> {
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        match std::fs::canonicalize(&current) {
            Ok(mut real) => {
                for part in missing.iter().rev() {
                    real.push(part);
                }
                return Ok(real);
            }
            Err(_) => {
                let Some(name) = current.file_name().map(|name| name.to_os_string()) else {
                    return Err(format!("cannot resolve {}", path.display()));
                };
                missing.push(name);
                if !current.pop() {
                    return Err(format!("cannot resolve {}", path.display()));
                }
            }
        }
    }
}

// --------------------------------------------------------------- the session

/// One job this session minted, and the provider call (if any) that asked for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    /// The provider call id, for an explicit `kelir bg run` inside a call.
    pub call_id: Option<String>,
}

/// One session's trust state: its token and the jobs it will answer for.
#[derive(Debug)]
pub struct Trust {
    dir: PathBuf,
    token: String,
    /// The jobs this session minted, by id. This is the whole of "is it mine": a file in the
    /// session directory is not evidence, because a tool child can write one.
    jobs: Mutex<HashMap<String, Job>>,
    /// Refused-verb entries still allowed in the log (see [`Trust::may_record_refusal`]).
    refusals: Mutex<u32>,
    /// Ids this session may still mint on request (see [`CLAIM_BUDGET`]).
    claims: Mutex<u32>,
    /// What `bg-claim` may still acknowledge (see [`ACK_BUDGET`]).
    acks: Mutex<u32>,
}

impl Trust {
    pub fn new(dir: PathBuf, token: String) -> Self {
        Self {
            dir,
            token,
            jobs: Mutex::new(HashMap::new()),
            refusals: Mutex::new(REFUSAL_BUDGET),
            claims: Mutex::new(CLAIM_BUDGET),
            acks: Mutex::new(ACK_BUDGET),
        }
    }

    /// A trust state with a fresh token, written to the session dir.
    pub fn create(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
        let token = new_token();
        if token.is_empty() {
            return Err("cannot generate a session token".to_string());
        }
        write_token(&dir, &token)?;
        Ok(Self::new(dir, token))
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// The one root for this session's jobs: `calls/`. Each job owns a directory below it.
    pub fn bg_dir(&self) -> PathBuf {
        crate::paths::calls_dir(&self.dir)
    }

    /// Drop the token file: the session is ending, and a token nobody serves is only something to
    /// find.
    pub fn retire(&self) {
        let _ = std::fs::remove_file(self.dir.join(TOKEN_FILE));
    }

    /// Mint a job's id: `bg_<128 bits>` for an explicit `kelir bg run`, `call_<128 bits>` for a
    /// bash call's background half.
    pub fn mint(&self, prefix: &str) -> Result<String, String> {
        self.mint_for(prefix, None)
    }

    /// Mint a job id, recording which provider call asked for it.
    pub fn mint_for(&self, prefix: &str, call_id: Option<&str>) -> Result<String, String> {
        let suffix = random_hex(ID_BYTES);
        if suffix.is_empty() {
            return Err("cannot mint a job id".to_string());
        }
        let id = format!("{prefix}_{suffix}");
        self.remember(&id, call_id.map(|call| call.trim().to_string()))?;
        Ok(id)
    }

    /// Register a bash call's own `bg/` id (kelir names the directory itself, `<seq>-<step>-<n>`).
    pub fn issue_call(&self, id: &str, call_id: &str) -> Result<(), String> {
        let id = id.trim();
        if !valid_job_id(id) {
            return Err(format!("`{id}` is not a job id"));
        }
        self.remember(id, Some(call_id.to_string()))
    }

    /// Record a job in the registry. Every id the session answers for goes through here; nothing
    /// is written to disk.
    fn remember(&self, id: &str, call_id: Option<String>) -> Result<(), String> {
        match self.jobs.lock() {
            Ok(mut jobs) => {
                jobs.insert(id.trim().to_string(), Job { call_id });
                Ok(())
            }
            Err(_) => Err("the job registry is unusable".to_string()),
        }
    }

    /// A job this session minted and no longer needs (a spawn that never started): drop the id.
    pub fn forget(&self, id: &str) {
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.remove(id.trim());
        }
    }

    /// Mint an id on request (`bg-claim`: `kelir bg run` asks the session for the id it may report
    /// with, instead of picking one).
    pub fn claim(&self, call_id: Option<&str>) -> Result<String, String> {
        match self.claims.lock() {
            Ok(mut left) => {
                if *left == 0 {
                    return Err(format!(
                        "this session has minted its {CLAIM_BUDGET} jobs for this run; \
                         `kelir bg run` cannot be granted another id"
                    ));
                }
                *left -= 1;
            }
            Err(_) => return Err("the job registry is unusable".to_string()),
        }
        self.mint_for("bg", call_id.map(str::trim).filter(|call| !call.is_empty()))
    }

    /// `bg-claim`, the retired verb: an id for an acknowledgement, from a budget of its own so it
    /// can never starve [`Trust::claim`].
    pub fn claim_ack(&self) -> Result<String, String> {
        match self.acks.lock() {
            Ok(mut left) if *left > 0 => *left -= 1,
            Ok(_) => return Err("bg-claim: retired, and its small allowance is spent".to_string()),
            Err(_) => return Err("the job registry is unusable".to_string()),
        }
        self.mint_for("bg", None)
    }

    /// Is `id` a job this session minted? The only ownership there is.
    pub fn owns(&self, id: &str) -> bool {
        self.jobs
            .lock()
            .map(|jobs| jobs.contains_key(id.trim()))
            .unwrap_or(false)
    }

    /// Adopt a job a previous process of this session minted (a resume rebuilds the registry from
    pub fn adopt(&self, id: &str) {
        let id = id.trim();
        if !valid_job_id(id) || self.owns(id) {
            return;
        }
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.insert(id.to_string(), Job { call_id: None });
        }
    }

    /// Every id this session owns, for a test or a listing.
    pub fn ids(&self) -> Vec<String> {
        self.jobs
            .lock()
            .map(|jobs| jobs.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// The refusal budget: a caller that is authenticated (it has the token) but names jobs that
    /// are not this session's must not be able to grow the log by one `verb` event per try.
    pub fn may_record_refusal(&self) -> bool {
        let mut budget = match self.refusals.lock() {
            Ok(budget) => budget,
            Err(_) => return false,
        };
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        true
    }

    /// The socket half: a request must carry this session's token.
    pub fn accepts(&self, request: &Request) -> Result<(), String> {
        if self.token.is_empty() {
            return Err("the session has no token; the socket is closed to verbs".to_string());
        }
        match request.token.as_deref() {
            Some(given) if same(given.trim(), &self.token) => Ok(()),
            Some(_) => Err(format!(
                "the session socket refused this request: the token is not this session's \
                 (read it from <session dir>/{TOKEN_FILE}, or use `kelir ctl`)"
            )),
            None => Err(format!(
                "the session socket refused this request: it carries no token. Every verb \
                 needs this session's token ({TOKEN_ENV} in a tool child; `kelir ctl` reads \
                 <session dir>/{TOKEN_FILE} itself)"
            )),
        }
    }

    /// The `bg-spawn` half: the one thing a job cannot do itself (on Windows a `kelir bg run` inside
    /// a call sits in the call's Job Object, which forbids breakaway).
    pub fn check_spawn(&self, request: &Request) -> Result<(), String> {
        let spawn = spawn_request(request)?;
        self.check_spawn_dir(&spawn)?;
        if !spawn.out.trim().is_empty() {
            self.check_job_path(&spawn.out, "out")?;
        }
        if let Some(cwd) = spawn.cwd.as_deref().filter(|cwd| !cwd.trim().is_empty())
            && !Path::new(cwd).is_dir()
        {
            return Err(format!("bg-spawn: `{}` is not a directory", cwd.trim()));
        }
        match spawn
            .id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            Some(id) => Err(format!(
                "bg-spawn: a spawn cannot name a job id (`{id}`); the session mints them"
            )),
            None => Ok(()),
        }
    }

    /// The call root a spawn names must be this session's own, canonicalised.
    fn check_spawn_dir(&self, spawn: &crate::bg::SpawnRequest) -> Result<(), String> {
        let ours = self.bg_dir();
        let base = std::fs::canonicalize(&ours)
            .map_err(|error| format!("cannot resolve {}: {error}", ours.display()))?;
        let real = match resolve_inside(&base, &spawn.dir) {
            Ok(real) => real,
            Err(_) => {
                return Err(format!(
                    "bg-spawn: would live in `{}`, which is not inside this session's {}",
                    spawn.dir.trim(),
                    base.display()
                ));
            }
        };
        if real != base {
            return Err(format!(
                "bg-spawn: would live in {}, not in this session's {}",
                real.display(),
                base.display()
            ));
        }
        Ok(())
    }

    /// The session half: `bg` and `cancel` name a job id, and the id (and the paths that come with
    /// it) must be a *job* this session issued.
    pub fn check_job(&self, request: &Request) -> Result<(), String> {
        match request.verb.as_str() {
            "bg" => {
                let status: crate::bg::Status = serde_json::from_value(request.args.clone())
                    .map_err(|error| format!("bg: not a job status: {error}"))?;
                self.check_owned(&status.id, "bg")?;
                if !status.out.trim().is_empty() {
                    self.check_job_path(&status.out, "out")?;
                }
                if let Some(path) = &status.cmd_path {
                    self.check_job_path(path, "cmd_path")?;
                }
                if status.kind != crate::bg::KIND_JOB {
                    return Err(format!(
                        "bg: `{}` is not a job of this session (a call reports through its \
                         status file, never over the socket)",
                        status.id
                    ));
                }
                Ok(())
            }
            "cancel" => {
                let id = crate::verbs::cancel_id(&request.args)?;
                self.check_owned(&id, "cancel")
            }
            _ => Ok(()),
        }
    }

    fn check_owned(&self, id: &str, verb: &str) -> Result<(), String> {
        if !valid_job_id(id) {
            return Err(format!(
                "{verb}: `{id}` is not a job id (one path component: letters, digits, `.`, `_`, \
                 `-`)"
            ));
        }
        if self.owns(id) {
            return Ok(());
        }
        Err(format!(
            "{verb}: this session did not start job `{id}`, so it will not answer for it \
             (reports and cancels are accepted only for the session's own jobs)"
        ))
    }

    /// Is `candidate` a path inside this session's `bg/` or `calls/`?
    pub fn inside_bg(&self, candidate: &str) -> bool {
        self.check_job_path(candidate, "out").is_ok()
    }

    fn check_job_path(&self, candidate: &str, field: &str) -> Result<(), String> {
        let base = self.bg_dir();
        if resolve_inside(&base, candidate).is_ok() {
            Ok(())
        } else {
            Err(format!(
                "{field}: `{}` is not inside this session's own calls/ directory",
                candidate.trim()
            ))
        }
    }
}

pub fn spawn_request(request: &Request) -> Result<crate::bg::SpawnRequest, String> {
    serde_json::from_value(request.args.clone())
        .map_err(|error| format!("not a spawn request: {error}"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use serde_json::json;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kelir-trust-{name}-{}",
            ulid::Ulid::generate().to_string().to_lowercase()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_token_is_256_bits_of_hex_and_no_two_are_alike() {
        let mut seen = HashSet::new();
        for _ in 0..500 {
            let token = new_token();
            assert_eq!(token.len(), TOKEN_BYTES * 2, "{token}");
            assert!(
                token
                    .chars()
                    .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()),
                "{token}"
            );
            assert!(seen.insert(token), "a token repeated");
        }
    }

    #[test]
    fn a_comparison_accepts_only_the_exact_token() {
        let token = "0123456789abcdef0123456789abcdef";
        assert!(same(token, token));
        assert!(!same(token, &token[1..]), "a prefix matched");
        assert!(!same(token, "0123456789abcdef0123456789abcdee"));
        assert!(!same(token, ""));
        assert!(!same("", ""), "two empty strings are not a match");
    }

    #[test]
    fn a_job_id_is_one_plain_path_component() {
        for good in ["bg_01j8", "12-3-2", "bg_01J8.X", "a_b-c.d"] {
            assert!(valid_job_id(good), "{good}");
        }
        for bad in [
            "",
            " ",
            "..",
            ".",
            ".hidden",
            "a/b",
            "a\\b",
            "a..b",
            "a:b",
            "C:\\x",
            "\\\\host\\s",
            "../up",
            "a b",
            "café",
            "x/../../y",
            // Windows resolves these to devices, whatever the extension, and
            // drops a trailing dot or space.
            "nul",
            "NUL",
            "nul.status",
            "con",
            "COM1",
            "com0",
            "lpt9.json",
            "aux",
            "prn",
            "COM\u{b9}",
            "bg_esc.",
            "bg_esc ",
        ] {
            assert!(!valid_job_id(bad), "{bad} was accepted");
        }
        // A name that merely starts with a device name is a normal file.
        for good in ["console", "nullable", "bg_com1", "com10"] {
            assert!(valid_job_id(good), "{good}");
        }
        assert!(!valid_job_id(&"x".repeat(MAX_ID + 1)));
        assert!(valid_job_id(&"x".repeat(MAX_ID)));
    }

    #[test]
    fn a_path_inside_the_base_is_accepted_even_before_it_exists() {
        let dir = temp_dir("inside");
        let inside = dir.join("job.out");
        let resolved = resolve_inside(&dir, &crate::util::path_string(&inside)).unwrap();
        assert!(resolved.starts_with(std::fs::canonicalize(&dir).unwrap()));
        let relative = resolve_inside(&dir, "job.out").unwrap();
        assert_eq!(relative, resolved);
        // A name whose parent does not exist yet either.
        let deep = resolve_inside(&dir, "a/b/c.out").unwrap();
        assert!(deep.ends_with(std::path::Path::new("a").join("b").join("c.out")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_that_escapes_the_base_is_refused() {
        let dir = temp_dir("escape");
        std::fs::create_dir_all(dir.join("bg")).unwrap();
        for bad in [
            "../elsewhere",
            "bg/../../elsewhere",
            "..",
            "\\\\host\\share\\x",
            "//host/share/x",
        ] {
            assert!(
                resolve_inside(&dir.join("bg"), bad).is_err(),
                "{bad} escaped"
            );
        }
        // A backslash is a separator only on Windows; on Unix it is an ordinary character, so
        // `..\..\elsewhere` is one harmless filename there and an escape here (; the Linux gate caught the
        #[cfg(windows)]
        {
            assert!(resolve_inside(&dir.join("bg"), r"..\..\elsewhere").is_err());
            assert!(resolve_inside(&dir, r"D:\elsewhere").is_err());
            assert!(resolve_inside(&dir, r"\\?\C:\Windows").is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_base_is_refused() {
        let dir = temp_dir("symlink");
        let bg = dir.join("bg");
        std::fs::create_dir_all(&bg).unwrap();
        let outside = temp_dir("symlink-target");
        std::os::unix::fs::symlink(&outside, bg.join("link")).unwrap();
        assert!(
            resolve_inside(&bg, "link/steal").is_err(),
            "a symlink out of bg/ was followed"
        );
        // A link that stays inside is fine.
        std::fs::write(bg.join("real.out"), b"x").unwrap();
        std::os::unix::fs::symlink(bg.join("real.out"), bg.join("alias.out")).unwrap();
        assert!(resolve_inside(&bg, "alias.out").is_ok());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// A request without the token is refused on every platform.
    #[cfg(unix)]
    #[test]
    fn a_request_without_the_token_is_refused_by_the_socket_half() {
        let dir = temp_dir("accepts");
        let trust = Trust::new(dir.clone(), "ses-token".to_string());
        let bare = Request::new("say", json!({"text": "hi"}), "ctl");
        let error = trust.accepts(&bare).unwrap_err();
        assert!(error.contains("no token"), "{error}");
        assert!(
            !error.contains("ses-token"),
            "the refusal echoed the token: {error}"
        );
        let wrong = bare.clone().with_token(Some("other".into()));
        let error = trust.accepts(&wrong).unwrap_err();
        assert!(error.contains("not this session's"), "{error}");
        let right = bare.with_token(Some("ses-token".into()));
        assert!(trust.accepts(&right).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The registry is this process's memory and its persisted half — never a
    /// file a child can create.
    #[test]
    fn the_registry_holds_only_ids_the_session_minted() {
        let dir = temp_dir("owns");
        std::fs::create_dir_all(dir.join("bg")).unwrap();
        std::fs::create_dir_all(dir.join("calls").join("12-3-2")).unwrap();
        let trust = Trust::new(dir.clone(), "t".into());
        // A call's output directory, and a status file dropped by a child, are
        // both *files*: neither makes a job.
        std::fs::write(dir.join("bg").join("bg_1.status"), "{}").unwrap();
        assert!(!trust.owns("12-3-2"), "a directory owned a job");
        assert!(!trust.owns("bg_1"), "a status file owned a job");

        let id = trust.mint("bg").unwrap();
        assert!(id.starts_with("bg_"), "{id}");
        assert_eq!(id.len(), 3 + ID_BYTES * 2, "{id}");
        assert!(trust.owns(&id));
        assert!(!trust.owns("../etc/passwd"));
        trust.forget(&id);
        assert!(!trust.owns(&id), "a forgotten job is not answered for");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two sessions never mint the same id: the id is random, and
    /// the id is random, not derived from anything a caller can pick.
    #[test]
    fn no_two_sessions_mint_the_same_id() {
        let first = Trust::new(temp_dir("mint-a"), "t".into());
        let second = Trust::new(temp_dir("mint-b"), "t".into());
        let mut seen = HashSet::new();
        for _ in 0..64 {
            for trust in [&first, &second] {
                let id = trust.mint("bg").unwrap();
                assert!(seen.insert(id), "an id repeated");
            }
        }
        first.issue_call("12-3-1", "call_1").unwrap();
        assert!(first.owns("12-3-1"), "a call's own id is registered");
        assert!(!first.owns("bg_never"), "an id nobody minted is not owned");
    }

    /// A resume rebuilds the registry from the log and from nothing else (a file in `bg/` is a file a tool
    /// child can write).
    #[test]
    fn a_resumed_session_adopts_only_what_its_log_names() {
        let dir = temp_dir("adopt");
        std::fs::create_dir_all(dir.join("bg")).unwrap();
        let resumed = Trust::create(dir.clone()).unwrap();
        assert!(!resumed.owns("bg_from_the_log"));
        resumed.adopt("bg_from_the_log");
        assert!(resumed.owns("bg_from_the_log"));
        // Neither does an id that was never adopted, nor a path.
        resumed.adopt("../escape");
        assert!(!resumed.owns("../escape"));
        // The token of the resumed process is a new one.
        assert_ne!(resumed.token(), Trust::create(dir.clone()).unwrap().token());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refusals_are_rate_limited() {
        let dir = temp_dir("refusals");
        let trust = Trust::new(dir.clone(), "t".into());
        for _ in 0..REFUSAL_BUDGET {
            assert!(trust.may_record_refusal());
        }
        for _ in 0..5 {
            assert!(!trust.may_record_refusal(), "the budget refilled");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `bg-claim` is bounded: a child inside a call may ask the session for an
    /// id (that is how `kelir bg run` gets one), but not for ever.
    #[test]
    fn claims_are_bounded() {
        let dir = temp_dir("claims");
        let trust = Trust::new(dir.clone(), "t".into());
        let id = trust.claim(Some("call_9")).unwrap();
        assert!(id.starts_with("bg_"));
        assert!(trust.owns(&id));
        for _ in 1..CLAIM_BUDGET {
            trust.claim(None).unwrap();
        }
        let error = trust.claim(None).unwrap_err();
        assert!(error.contains("minted its"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_spawn_into_another_directory_is_refused() {
        let dir = temp_dir("spawn");
        let bg = crate::paths::calls_dir(&dir);
        std::fs::create_dir_all(&bg).unwrap();
        let other = temp_dir("spawn-other");
        let trust = Trust::new(dir.clone(), "t".into());
        let id = trust.mint("bg").unwrap();
        let spawn = |job_dir: &Path, out: &Path, id: &str| {
            json!({
                "dir": crate::util::path_string(job_dir),
                "id": id,
                "out": crate::util::path_string(out),
                "cmd": "true",
                "shell": ["bash"],
                "kind": "job",
            })
        };
        let ok = Request::new(
            "bg-spawn",
            spawn(&bg, &crate::bg::out_path(&bg, &id), &id),
            "job",
        );
        // A spawn may never name a job the session owns: a token holder must not
        // gain a running job's secrets.
        let error = trust.check_spawn(&ok).unwrap_err();
        assert!(error.contains("cannot name a job id"), "{error}");
        // An id the session never minted: a child can pick any name it likes,
        // and it is still not this session's job.
        let made_up = Request::new(
            "bg-spawn",
            spawn(&bg, &crate::bg::out_path(&bg, "bg_picked"), "bg_picked"),
            "job",
        );
        let error = trust.check_spawn(&made_up).unwrap_err();
        assert!(error.contains("cannot name a job id"), "{error}");
        // No id at all: the session mints one when it answers.
        let mut args = spawn(&bg, &crate::bg::out_path(&bg, "bg_new"), &id);
        args.as_object_mut().unwrap().remove("id");
        assert!(
            trust
                .check_spawn(&Request::new("bg-spawn", args, "job"))
                .is_ok()
        );
        let foreign = Request::new(
            "bg-spawn",
            spawn(
                &crate::paths::calls_dir(&other),
                &crate::bg::out_path(&crate::paths::calls_dir(&other), &id),
                &id,
            ),
            "job",
        );
        let error = trust.check_spawn(&foreign).unwrap_err();
        assert!(error.contains("would live in"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn a_foreign_report_is_refused_and_a_path_out_of_the_session_is_refused() {
        let dir = temp_dir("check-job");
        let calls = crate::paths::calls_dir(&dir);
        std::fs::create_dir_all(&calls).unwrap();
        let trust = Trust::new(dir.clone(), "t".into());
        let out = crate::util::path_string(&crate::bg::out_path(&calls, "bg_1"));
        let report = json!({
            "id": "bg_1", "state": "running", "started": "2026-01-01T00:00:00Z",
            "out": out, "kind": "job"
        });
        let foreign = Request::new("bg", report.clone(), "job");
        let error = trust.check_job(&foreign).unwrap_err();
        assert!(error.contains("did not start job `bg_1`"), "{error}");

        // The id is the session's own: the token is enough, since every tool child has it.
        let id = trust.mint("bg").unwrap();
        let out = crate::util::path_string(&crate::bg::out_path(&calls, &id));
        let report = json!({
            "id": id, "state": "running", "started": "2026-01-01T00:00:00Z",
            "out": out.clone(), "kind": "job"
        });
        let mine = Request::new("bg", report, "job").with_token(Some("t".into()));
        assert!(
            trust.check_job(&mine).is_ok(),
            "{:?}",
            trust.check_job(&mine)
        );

        let stolen = json!({
            "id": id, "state": "running", "started": "2026-01-01T00:00:00Z",
            "out": crate::util::path_string(&dir.join("..").join("stolen.out")), "kind": "job"
        });
        let error = trust
            .check_job(&Request::new("bg", stolen, "job"))
            .unwrap_err();
        assert!(error.contains("not inside this session's own"), "{error}");

        let traversal = json!({
            "id": "../bg_9", "state": "running", "started": "2026-01-01T00:00:00Z",
            "out": out, "kind": "job"
        });
        assert!(
            trust
                .check_job(&Request::new("bg", traversal, "job"))
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_ownership_is_the_registry_not_the_file() {
        let dir = temp_dir("status-owns");
        let calls = crate::paths::calls_dir(&dir);
        std::fs::create_dir_all(&calls).unwrap();
        let trust = Trust::new(dir.clone(), "t".into());
        let id = trust.mint("bg").unwrap();
        // A status file with the right id is still not evidence of ownership.
        let path = crate::bg::status_path(&calls, &id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{}").unwrap();
        let fresh = Trust::new(dir.clone(), "other".into());
        assert!(!fresh.owns(&id));
        assert!(trust.owns(&id));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod ack_budget_tests {
    use super::*;

    #[test]
    fn the_retired_claim_verb_cannot_spend_the_spawn_budget() {
        let dir = std::env::temp_dir().join(format!("kelir-ack-{}", ulid::Ulid::generate()));
        let _ = std::fs::create_dir_all(&dir);
        let trust = Trust::new(dir.clone(), "t".into());
        let mut granted = 0;
        for _ in 0..(CLAIM_BUDGET * 2) {
            if trust.claim_ack().is_ok() {
                granted += 1;
            }
        }
        assert_eq!(granted, ACK_BUDGET);
        assert!(trust.claim(None).is_ok(), "a spawn can still mint an id");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
