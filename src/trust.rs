//! The session socket's trust boundary: who may drive this session, and which
//! background jobs it will take reports for (STEER-7, retro M5-A F8).
//!
//! Reaching the socket used to be the whole of the authorisation: any process
//! of the same user that could connect could inject `bg` reports (during
//! dogfooding a test child inside a session impersonated the shim and put 43
//! "bg job was lost" messages into a turn) and could cancel a job that belonged
//! to somebody else. Two things close that:
//!
//! * **A per-session token.** 32 random bytes, hex, made at session start, held
//!   in this process's memory, written to `<session dir>/token` with owner-only
//!   permissions, and exported to the session's own tool children as
//!   `KELIR_TOKEN`. Every request that arrives *on the socket* must carry it
//!   (`Request::token`); a request without it is answered and dropped, before
//!   the session sees it (so it cannot write a `verb` event either). The token
//!   is never logged, never in an event, never in an argv — only the two
//!   processes that already share this session's memory read it.
//!
//!   **What this does not defend against, by design:** a process that can read
//!   the session's environment (`/proc/<pid>/environ`, a debugger) or its token
//!   file already runs as the user, in the session, and is trusted. The token
//!   stops a *different* session's child, a stray process, and a script that
//!   happened to find the pipe by name — not the user or the model's own tools.
//!
//! * **A registry of the jobs this session created, and a capability per job.**
//!   The session is the only issuer of job ids. A job id is minted here —
//!   `bg_<32 random hex>` for an explicit job, `call_<32 random hex>` for the
//!   `bg/` id of a bash call — and recorded in this registry *and* in the
//!   session's own log. `bg`, `cancel` and `bg-spawn` name a job id; the
//!   session answers only for ids in the registry, and a `bg` report for one of
//!   them must also carry **that job's capability** (a random per-job secret
//!   given to its shim and nobody else). Both are needed, because a tool child
//!   holds the session's token by design: the token says "this is somebody
//!   inside this session", the capability says "this is *that job's shim*".
//!
//!   **File existence is never evidence.** `commands/<id>/` and `bg/<id>.status`
//!   are written by whoever can write the session directory, a tool child
//!   included, so neither registers a job; the registry is this process's
//!   memory, rebuilt on resume from the log. Every job directory and status/out
//!   path in a request must still resolve *inside* the session's own `bg/`
//!   (canonicalised: `..`, a symlink out of it and, on Windows, a UNC or
//!   another drive's path are all refused).
//!
//! [`check_job`] is that second half and runs in the session, where a refusal
//! becomes a `verb` event (the caller is authenticated, so the log should say
//! what it tried; it is rate-limited so a child cannot flood the log with it).
//! [`Trust::accepts`] is the first half and runs at the socket, where a refusal
//! is just an answer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::ipc::Request;

/// The variable a session's own tool children read the token from.
pub const TOKEN_ENV: &str = "KELIR_TOKEN";
/// The file `kelir ctl` (a human's terminal) reads the token from.
pub const TOKEN_FILE: &str = "token";
/// Bytes of randomness in a token; 32 hex-encoded characters would be 128 bits,
/// this is 256.
pub const TOKEN_BYTES: usize = 32;
/// The longest job id the session will look at. Ids are `bg_<hex>` or
/// `call_<hex>` here, both far shorter.
pub const MAX_ID: usize = 128;
/// Bytes of randomness in a job id: 128 bits. A job id is not a secret (it is
/// in the log and in `bg ls`), but it must never be guessable, so a foreign
/// process cannot name a job that is about to exist.
pub const ID_BYTES: usize = 16;
/// Bytes of randomness in a job's capability: 256 bits, the same secret size as
/// the session token. Only that job's shim holds it.
pub const CAP_BYTES: usize = 32;
/// The variable a job's shim hands its capability to the command it runs, when
/// that command reports itself (`kelir bg run` inside a call). Never an argv.
pub const CAP_ENV: &str = "KELIR_JOB_CAP";
/// How many refused `bg`/`cancel`/`bg-spawn` verbs one session will record in
/// its log before it answers without logging: a token holder names jobs that
/// are not this session's one by one, and the log must not grow by one event
/// per try (STEER-7 review B1).
pub const REFUSAL_BUDGET: u32 = 32;
/// How many job ids one session will mint on request (`bg-claim`) and for its
/// calls. A tool child inside a call may ask for ids (that is how `kelir bg run`
/// gets one), so the number is bounded: an authenticated process cannot make
/// the session mint for ever. Past it, a claim is refused (and the refusal is
/// logged at most [`REFUSAL_BUDGET`] times).
pub const CLAIM_BUDGET: u32 = 256;

// ------------------------------------------------------------------ the token

/// A fresh token: 64 lowercase hex characters from the OS's randomness.
pub fn new_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    // A machine with no usable randomness must not fall back to a predictable
    // token: an empty environment variable costs the caller a clear refusal.
    if random_bytes(&mut bytes).is_err() {
        return String::new();
    }
    hex(&bytes)
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

fn random_bytes(buf: &mut [u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Read as _;
        let mut file = std::fs::File::open("/dev/urandom")
            .map_err(|error| format!("no /dev/urandom: {error}"))?;
        file.read_exact(buf)
            .map_err(|error| format!("cannot read /dev/urandom: {error}"))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Security::Cryptography::{
            BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
        };
        // SAFETY: the buffer is ours and its length is what is passed.
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status < 0 {
            return Err(format!("BCryptGenRandom failed ({status:#x})"));
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        // No OS randomness on this platform: say so rather than inventing one.
        let _ = buf;
        Err("this platform has no secure random source".to_string())
    }
}

/// 32 lowercase hex characters from the OS's randomness, or an empty string
/// when the platform has no secure source (`` is never a valid id).
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    if random_bytes(&mut buf).is_err() {
        return String::new();
    }
    hex(&buf)
}

/// Compare two secrets without telling a caller how far it got. Length is not a
/// secret (every token is the same length).
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
    restrict_to_owner(&path)?;
    let check = owner_only(&path)?;
    if !check {
        return Err(format!(
            "{} is readable by more than its owner; a session token must not be",
            path.display()
        ));
    }
    Ok(())
}

/// The token file's contents, when there is one.
pub fn read_token(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(TOKEN_FILE)).ok()?;
    let token = text.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// The token a session exported to this process, when this process is one of
/// its children.
pub fn token_from_env() -> Option<String> {
    std::env::var(TOKEN_ENV)
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

/// Make a directory owner-only, the same way [`restrict_to_owner`] makes a
/// file: `0700` on Unix, the owner-only DACL on Windows. The session directory
/// above `bg/` is already `0700`, so this is defence in depth (STEER-7 review
/// N2); a platform that cannot set it is not a reason to refuse the session.
///
/// Unix only sets the mode: on Windows a directory's DACL is inherited from the
/// state directory, which is already the user's, and there is no portable
/// `chmod 0700` that does what this means. The mode is set explicitly rather
/// than left to the umask, which under `umask 000` leaves `777`.
pub fn restrict_dir_to_owner(dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot restrict {}: {error}", dir.display()))
    }
    #[cfg(not(unix))]
    {
        // Nothing to set: the DACL is inherited from the owner-only state dir.
        let _ = dir;
        Ok(())
    }
}

/// Make a file's permissions owner-only: `0600` on Unix, and on Windows a
/// protected DACL with one `FILE_ALL_ACCESS` ACE for this process's user.
///
/// The state directory already sits under the user's profile, whose ACL denies
/// other users — but "the parent happens to be safe" is not a property this
/// code can check portably, and a token another user can read is not a token
/// (STEER-7). So the file itself gets the descriptor, and [`owner_only`] reads
/// it back.
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

/// Is this file readable only by its owner? On Windows the DACL is read back
/// the same way the session pipe's is: a protected DACL with one ACE for this
/// user, no `Everyone`.
pub fn owner_only(path: &Path) -> Result<bool, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let metadata = std::fs::metadata(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        Ok(metadata.permissions().mode() & 0o077 == 0)
    }
    #[cfg(windows)]
    {
        crate::perm::file_is_owner_only(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = std::fs::metadata(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        Ok(true)
    }
}

/// Does the session's own `dir` hold a token that became readable by others?
/// Checked once per session start: a token another user can read is not a
/// token.
pub fn token_file_is_private(dir: &Path) -> Result<(), String> {
    let path = dir.join(TOKEN_FILE);
    if path.exists() && !owner_only(&path)? {
        return Err(format!(
            "{} is readable by more than its owner; refusing to use it",
            path.display()
        ));
    }
    Ok(())
}

// --------------------------------------------------------------------- ids

/// A job id this session may be told about: one path component, no traversal,
/// nothing a status path could be built out of that lands elsewhere.
///
/// Windows resolves more names than it writes: a trailing `.` or space is
/// dropped (`bg_esc.` *is* `bg_esc`), and `nul`/`con`/`com1` are devices, not
/// files. Both are refused here rather than left to produce a status file the
/// shim thinks it wrote and the OS did not (STEER-7 review N1).
pub fn valid_job_id(id: &str) -> bool {
    // Whitespace is not trimmed away: an id with a trailing space *is* a
    // different name on Windows (`bg_esc ` resolves to `bg_esc`) and a
    // different path everywhere else, so it is refused rather than quietly
    // rewritten into one this session may own.
    if id.trim() != id {
        return false;
    }
    if id.is_empty()
        || id.len() > MAX_ID
        || id.starts_with('.')
        || id.contains("..")
        || id.ends_with('.')
        || id.ends_with(' ')
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return false;
    }
    !is_windows_device_name(id)
}

/// `CON`, `NUL`, `COM1`… and their `name.ext` spellings, which Windows resolves
/// to a device whatever the extension. Only the stem before the first `.`
/// counts: `nul.status` is the null device too.
fn is_windows_device_name(id: &str) -> bool {
    let stem = id.split('.').next().unwrap_or(id).trim_end();
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    // COM0/COM1..COM9 and LPT1..LPT9 (COM0 is a device on Windows too).
    if upper.len() == 4 {
        let (head, digit) = upper.split_at(3);
        if matches!(head, "COM" | "LPT") && digit.as_bytes()[0].is_ascii_digit() {
            return true;
        }
    }
    // The Unicode superscript digit forms (`COM¹`, `LPT²`) are devices as well.
    let chars: Vec<char> = upper.chars().collect();
    if chars.len() == 4
        && matches!(chars[..3], ['C', 'O', 'M'] | ['L', 'P', 'T'])
        && matches!(chars[3], '\u{b9}' | '\u{b2}' | '\u{b3}')
    {
        return true;
    }
    false
}

// ------------------------------------------------------------------- paths

/// Resolve `candidate` (absolute, or relative to `base`) and prove it lands
/// inside `base`.
///
/// Canonicalising is the whole point: `..`, a symlink that points out of the
/// session directory and, on Windows, a UNC path (`\\host\share`) or another
/// drive (`D:\…`) all resolve to somewhere else and are refused. A path whose
/// last components do not exist yet (a job's `out` file before the shim writes
/// it) is resolved through its nearest existing ancestor.
pub fn resolve_inside(base: &Path, candidate: &str) -> Result<PathBuf, String> {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return Err("an empty path".to_string());
    }
    // `\\host\share`, `//host/share` and `\\?\C:\…` are never inside a local
    // directory, whatever the rest of the string says.
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

/// One job this session minted: its capability and whose provider call (if
/// any) asked for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    /// The per-job secret: only that job's shim holds it. A `bg` report for the
    /// id must carry it.
    pub capability: String,
    /// The provider call id, for an explicit `kelir bg run` inside a call.
    pub call_id: Option<String>,
}

/// One session's trust state: its token and the jobs it will answer for.
#[derive(Debug)]
pub struct Trust {
    dir: PathBuf,
    token: String,
    /// The jobs this session minted, by id. This is the whole of "is it mine":
    /// a file in the session directory is not evidence, because a tool child
    /// can write one (STEER-7 review B1). It lives in this process's memory and
    /// nowhere else — a capability on disk would be a capability a tool child
    /// could read, which is the whole attack.
    jobs: Mutex<HashMap<String, Job>>,
    /// Refused-verb entries still allowed in the log (see
    /// [`Trust::may_record_refusal`]).
    refusals: Mutex<u32>,
    /// Ids this session may still mint on request (see [`CLAIM_BUDGET`]).
    claims: Mutex<u32>,
}

impl Trust {
    pub fn new(dir: PathBuf, token: String) -> Self {
        Self {
            dir,
            token,
            jobs: Mutex::new(HashMap::new()),
            refusals: Mutex::new(REFUSAL_BUDGET),
            claims: Mutex::new(CLAIM_BUDGET),
        }
    }

    /// A trust state with a fresh token, written to the session dir.
    pub fn create(dir: PathBuf) -> Result<Self, String> {
        // The session's own `bg/` exists from here on, so a spawn check can
        // canonicalise it and a job's status file has a home even before the
        // first job (STEER-7). It is owner-only like the token: the directory
        // above it is `0700`, but defence in depth is cheap (review N2).
        let bg = dir.join("bg");
        std::fs::create_dir_all(&bg)
            .map_err(|error| format!("cannot create {}: {error}", bg.display()))?;
        let _ = restrict_dir_to_owner(&bg);
        let token = new_token();
        if token.is_empty() {
            return Err(
                "cannot generate a session token (this machine has no secure random source)"
                    .to_string(),
            );
        }
        write_token(&dir, &token)?;
        Ok(Self::new(dir, token))
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// The session's own `bg/`, where its jobs' status files live.
    pub fn bg_dir(&self) -> PathBuf {
        self.dir.join("bg")
    }

    /// Drop the token file: the session is ending, and a token nobody serves is
    /// only something to find. `kelir ctl` on an exited session is refused
    /// anyway (there is no socket to answer it).
    pub fn retire(&self) {
        // The token goes: a token nobody serves is only something to find, and
        // it must not outlive the session that minted it (review S1). The
        // registry needs nothing here — it was never on disk, and a resumed
        // session adopts its jobs again from its log and its `bg/`.
        let _ = std::fs::remove_file(self.dir.join(TOKEN_FILE));
    }

    /// Mint a job's id **and** its capability: `bg_<128 bits>` for an explicit
    /// `kelir bg run`, `call_<128 bits>` for a bash call's background half. The
    /// session is the only issuer, so an id is never a directory name two
    /// sessions share (review S2) and never something a child can predict; the
    /// capability goes to that job's shim and to nobody else.
    pub fn mint(&self, prefix: &str) -> Result<(String, String), String> {
        self.mint_for(prefix, None)
    }

    /// Mint a job's id and capability, recording which provider call asked for
    /// it (an explicit job started by a call).
    pub fn mint_for(
        &self,
        prefix: &str,
        call_id: Option<&str>,
    ) -> Result<(String, String), String> {
        let suffix = random_hex(ID_BYTES);
        if suffix.is_empty() {
            return Err(
                "cannot mint a job id (this machine has no secure random source)".to_string(),
            );
        }
        let id = format!("{prefix}_{suffix}");
        let capability = random_hex(CAP_BYTES);
        if capability.is_empty() {
            return Err(
                "cannot mint a job capability (this machine has no secure random source)"
                    .to_string(),
            );
        }
        self.remember(
            &id,
            &capability,
            call_id.map(|call| call.trim().to_string()),
        )?;
        Ok((id, capability))
    }

    /// Register a bash call's own `bg/` id (kelir names the directory itself,
    /// `<seq>-<step>-<n>`, M1). A call gets **no capability**: its shim never
    /// sends a `bg` report — the status file it writes is folded in by the
    /// session's own watcher — so no report for a call's id is accepted, not
    /// even from the call's own child (STEER-7 review B1). Registering it is
    /// what makes `cancel <call id>` and the watcher's note legitimate.
    pub fn issue_call(&self, id: &str, call_id: &str) -> Result<(), String> {
        let id = id.trim();
        if !valid_job_id(id) {
            return Err(format!("`{id}` is not a job id"));
        }
        self.remember(id, "", Some(call_id.to_string()))
    }

    /// Record a job in the registry. Every id the session answers for goes
    /// through here; nothing is written to disk (a capability on disk is a
    /// capability a tool child can read).
    fn remember(&self, id: &str, capability: &str, call_id: Option<String>) -> Result<(), String> {
        let job = Job {
            capability: capability.trim().to_string(),
            call_id,
        };
        match self.jobs.lock() {
            Ok(mut jobs) => {
                jobs.insert(id.trim().to_string(), job);
            }
            // A poisoned registry is not a reason to answer for a job: the
            // caller gets a refusal instead of a capability nobody can check.
            Err(_) => return Err("the job registry is unusable".to_string()),
        }
        Ok(())
    }

    /// A job this session minted and no longer needs (a spawn that never
    /// started): drop the id the shim would have reported with.
    pub fn forget(&self, id: &str) {
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.remove(id.trim());
        }
    }

    /// Mint an id and a capability on request (`bg-claim`: `kelir bg run` asks
    /// the session for the id it may report with, instead of picking one). The
    /// caller holds the session token — a tool child is what asks — so the
    /// number of claims is bounded: an authenticated process cannot make the
    /// session mint for ever.
    pub fn claim(&self, call_id: Option<&str>) -> Result<(String, String), String> {
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

    /// Is `id` a job this session minted? The only ownership there is.
    pub fn owns(&self, id: &str) -> bool {
        self.jobs
            .lock()
            .map(|jobs| jobs.contains_key(id.trim()))
            .unwrap_or(false)
    }

    /// Adopt a job a previous process of this session minted (a resume rebuilds
    /// the registry from the log's `bg_started` events and from the jobs in its
    /// own `bg/` at startup). The capability is gone with the process that
    /// minted it, so an adopted job answers `cancel` but never a `bg` report:
    /// a report is what a shim sends, and a shim that outlives its session
    /// settles through its status file and the watcher instead.
    pub fn adopt(&self, id: &str) {
        let id = id.trim();
        if !valid_job_id(id) || self.owns(id) {
            return;
        }
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.insert(
                id.to_string(),
                Job {
                    capability: String::new(),
                    call_id: None,
                },
            );
        }
    }

    /// The capability a job's shim holds, for the session's own use (it hands
    /// it to the shim it spawns). `None` for an id this session never minted,
    /// and for one it only adopted from a previous process (see [`adopt`]: the
    /// secret died with the process that minted it).
    pub fn capability(&self, id: &str) -> Option<String> {
        self.jobs
            .lock()
            .ok()
            .and_then(|jobs| jobs.get(id.trim()).map(|job| job.capability.clone()))
            .filter(|capability| !capability.is_empty())
    }

    /// Every id this session owns, for a test or a listing.
    pub fn ids(&self) -> Vec<String> {
        self.jobs
            .lock()
            .map(|jobs| jobs.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Does `given` prove that the sender is the shim of job `id`? Two things
    /// are needed, and neither is enough alone: the id is in the registry, and
    /// the capability is *that* job's. A tool child holds the session token by
    /// design, so only the capability separates a shim from the child next to
    /// it (STEER-7 review B1). An id with no capability at all (a call, an
    /// adopted job) is proved by nothing.
    pub fn proves(&self, id: &str, given: Option<&str>) -> bool {
        let Some(given) = given.map(str::trim).filter(|given| !given.is_empty()) else {
            return false;
        };
        let jobs = match self.jobs.lock() {
            Ok(jobs) => jobs,
            Err(_) => return false,
        };
        match jobs.get(id.trim()) {
            Some(job) if !job.capability.is_empty() => same(given, &job.capability),
            _ => false,
        }
    }

    /// The refusal budget: a caller that is authenticated (it has the token) but
    /// names jobs that are not this session's must not be able to grow the log
    /// by one `verb` event per try. The first [`REFUSAL_BUDGET`] refusals are
    /// recorded; past it the refusal is answered and *not* logged, so a flood
    /// costs the session nothing it keeps.
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

    /// The socket half: a request must carry this session's token. A request
    /// that does not is answered and dropped here, before the session can act
    /// on it or log it — an unauthenticated caller does not get to write into
    /// the log.
    pub fn accepts(&self, request: &Request) -> Result<(), String> {
        if self.token.is_empty() {
            return Err(
                "the session has no token (no secure random source on this machine); the \
                 socket is closed to verbs"
                    .to_string(),
            );
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

    /// The `bg-spawn` half: the one thing a job cannot do itself (on Windows a
    /// `kelir bg run` inside a call sits in the call's Job Object, which forbids
    /// breakaway). The session starts the shim, so **the session mints the id**:
    /// a spawn that names an id is a re-spawn of a job it already owns, and one
    /// that names none gets a fresh id back. Either way the capability goes from
    /// the session to the shim, never through the caller.
    ///
    /// Paths are checked before ownership so the answer says the more specific
    /// thing: a spawn aimed outside this session's `bg/` is refused for that.
    pub fn check_spawn(&self, request: &Request) -> Result<(), String> {
        let spawn = spawn_request(request)?;
        // Where the job would live, which is this session's own `bg/`.
        self.check_spawn_dir(&spawn)?;
        if !spawn.out.trim().is_empty() {
            self.check_job_path(&spawn.out, "out")?;
        }
        if let Some(cwd) = spawn.cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()) {
            // The command runs in the caller's directory; it is not this
            // session's `bg/`, so it is not resolved against it, only required
            // to be a directory that exists.
            if !Path::new(cwd).is_dir() {
                return Err(format!("bg-spawn: `{}` is not a directory", cwd.trim()));
            }
        }
        match spawn
            .id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            // A named id: this session must already own the job (a re-spawn).
            Some(id) => self.check_owned(id, "bg-spawn"),
            // No id: the session mints one when it answers.
            None => Ok(()),
        }
    }

    /// The `bg/` a spawn names must be this session's own, canonicalised.
    fn check_spawn_dir(&self, spawn: &crate::bg::SpawnRequest) -> Result<(), String> {
        let ours = self.bg_dir();
        let base = std::fs::canonicalize(&ours)
            .map_err(|error| format!("cannot resolve {}: {error}", ours.display()))?;
        let real = match resolve_inside(&base, &spawn.dir) {
            Ok(real) => real,
            // The resolution already refused it (`..`, a symlink out, a UNC or
            // another drive); the answer says what the spawn asked for.
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

    /// The session half: `bg` and `cancel` name a job id, and the id (and the
    /// paths that come with it) must be a *job* this session issued. A `bg`
    /// report additionally has to carry **that job's capability**: the token
    /// alone proves only "somebody inside this session", which every tool child
    /// is.
    pub fn check_job(&self, request: &Request) -> Result<(), String> {
        match request.verb.as_str() {
            "bg" => {
                let status: crate::bg::Status = serde_json::from_value(request.args.clone())
                    .map_err(|error| format!("bg: not a job status: {error}"))?;
                // The id first: a report that names a job this session never
                // issued is refused for that, and its paths are never looked at
                // (no `canonicalize` on a stranger's say-so).
                self.check_owned(&status.id, "bg")?;
                // Then the paths: a report for one of ours that points out of
                // the session is refused for the path, which is what it tried.
                // Both are needed before the capability, because a refusal must
                // not depend on knowing a secret.
                if !status.out.trim().is_empty() {
                    self.check_job_path(&status.out, "out")?;
                }
                if let Some(path) = &status.cmd_path {
                    self.check_job_path(path, "cmd_path")?;
                }
                // A *job*: a call's own id is in the registry (its status file
                // reaches the log through the session's watcher), but no report
                // is ever accepted for it — the call's shim never sends one, so
                // a report with that id is a child quoting its own directory
                // name (STEER-7 review B1).
                if status.kind != crate::bg::KIND_JOB {
                    return Err(format!(
                        "bg: `{}` is not a job of this session (a call reports through its \
                         status file, never over the socket)",
                        status.id
                    ));
                }
                // The capability lives on the request, not in the status: a
                // status file (or a status-shaped `args`) is a thing a child can
                // write, the request's own field is not.
                if !self.proves(&status.id, request.capability.as_deref()) {
                    return Err(format!(
                        "bg: the report for job `{}` does not carry that job's capability, so it \
                         is not from its shim (the session token is not one: every tool child has \
                         it)",
                        status.id
                    ));
                }
                Ok(())
            }
            "cancel" => {
                let id = crate::verbs::cancel_id(&request.args)?;
                // A cancel names one of ours and is done by a token holder (a
                // person at `kelir ctl`, the TUI, the session's own children).
                // It needs no capability: it kills, it does not record a state a
                // caller could be lied to about.
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
             (reports, cancels and spawns are accepted only for the session's own jobs)"
        ))
    }

    fn check_job_path(&self, candidate: &str, field: &str) -> Result<(), String> {
        for base in [self.bg_dir(), self.dir.join("commands")] {
            if resolve_inside(&base, candidate).is_ok() {
                return Ok(());
            }
        }
        Err(format!(
            "{field}: `{}` is not inside this session's own bg/ or commands/ directories",
            candidate.trim()
        ))
    }
}

/// Parse a spawn request's arguments.
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
            ulid::Ulid::new().to_string().to_lowercase()
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
            // drops a trailing dot or space (STEER-7 review N1).
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
        // A backslash is a separator only on Windows; on Unix it is an ordinary
        // character, so `..\..\elsewhere` is one harmless filename there and
        // an escape here (STEER-7; the Linux gate caught the first version).
        #[cfg(windows)]
        {
            assert!(resolve_inside(&dir.join("bg"), r"..\..\elsewhere").is_err());
            assert!(resolve_inside(&dir, r"D:\elsewhere").is_err());
            assert!(resolve_inside(&dir, r"\\?\C:\Windows").is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A symlink is the escape that a string check cannot see.
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

    /// The same on every platform: the file gets the descriptor and the check
    /// reads it back. On Windows that is a DACL, not a mode (STEER-7).
    #[test]
    fn the_token_file_is_owner_only_everywhere() {
        let dir = temp_dir("token-file-all");
        let token = new_token();
        write_token(&dir, &token).unwrap();
        assert!(owner_only(&dir.join(TOKEN_FILE)).unwrap());
        assert!(token_file_is_private(&dir).is_ok());
        assert_eq!(read_token(&dir).as_deref(), Some(token.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_token_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir("token-file");
        let token = new_token();
        write_token(&dir, &token).unwrap();
        let mode = std::fs::metadata(dir.join(TOKEN_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "mode {mode:o}");
        assert_eq!(read_token(&dir).as_deref(), Some(token.as_str()));
        // A token file someone else widened is noticed.
        std::fs::set_permissions(dir.join(TOKEN_FILE), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(!owner_only(&dir.join(TOKEN_FILE)).unwrap());
        assert!(token_file_is_private(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

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
    /// file a child can create (STEER-7 review B1).
    #[test]
    fn the_registry_holds_only_ids_the_session_minted() {
        let dir = temp_dir("owns");
        std::fs::create_dir_all(dir.join("bg")).unwrap();
        std::fs::create_dir_all(dir.join("commands").join("12-3-2")).unwrap();
        let trust = Trust::new(dir.clone(), "t".into());
        // A call's output directory, and a status file dropped by a child, are
        // both *files*: neither makes a job.
        std::fs::write(dir.join("bg").join("bg_1.status"), "{}").unwrap();
        assert!(!trust.owns("12-3-2"), "a directory owned a job");
        assert!(!trust.owns("bg_1"), "a status file owned a job");

        let (id, capability) = trust.mint("bg").unwrap();
        assert!(id.starts_with("bg_"), "{id}");
        assert_eq!(id.len(), 3 + ID_BYTES * 2, "{id}");
        assert_eq!(capability.len(), CAP_BYTES * 2, "{capability}");
        assert!(trust.owns(&id));
        assert_eq!(trust.capability(&id).as_deref(), Some(capability.as_str()));
        assert!(!trust.owns("../etc/passwd"));
        trust.forget(&id);
        assert!(!trust.owns(&id), "a forgotten job is not answered for");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two sessions never mint the same id (review S2): the id is random, and
    /// a call's capability is not derived from anything a caller can pick.
    #[test]
    fn no_two_sessions_mint_the_same_id() {
        let first = Trust::new(temp_dir("mint-a"), "t".into());
        let second = Trust::new(temp_dir("mint-b"), "t".into());
        let mut seen = HashSet::new();
        for _ in 0..64 {
            for trust in [&first, &second] {
                let (id, capability) = trust.mint("bg").unwrap();
                assert!(seen.insert(id), "an id repeated");
                assert!(seen.insert(capability), "a capability repeated");
            }
        }
        first.issue_call("12-3-1", "call_1").unwrap();
        assert!(first.owns("12-3-1"), "a call's own id is registered");
        assert_eq!(
            first.capability("12-3-1"),
            None,
            "a call has no capability, so no report for it is ever accepted"
        );
        assert!(!first.proves("12-3-1", Some("anything")));
    }

    /// A resume rebuilds the registry from the log and from nothing else (a
    /// file in `bg/` is a file a tool child can write, and a capability on disk
    /// would be one it could read). An adopted job answers `cancel` but never a
    /// `bg` report — the secret died with the process that minted it.
    #[test]
    fn a_resumed_session_adopts_only_what_its_log_names() {
        let dir = temp_dir("adopt");
        std::fs::create_dir_all(dir.join("bg")).unwrap();
        let resumed = Trust::create(dir.clone()).unwrap();
        assert!(!resumed.owns("bg_from_the_log"));
        resumed.adopt("bg_from_the_log");
        assert!(resumed.owns("bg_from_the_log"));
        assert!(resumed.capability("bg_from_the_log").is_none());
        assert!(!resumed.proves("bg_from_the_log", Some("anything")));
        // Neither does an id that was never adopted, nor a path.
        resumed.adopt("../escape");
        assert!(!resumed.owns("../escape"));
        // The token of the resumed process is a new one.
        assert_ne!(resumed.token(), Trust::create(dir.clone()).unwrap().token());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The capability is what separates a shim from a tool child that holds the
    /// session's token: no capability, or another job's, proves nothing.
    #[test]
    fn only_that_jobs_capability_proves_a_report() {
        let dir = temp_dir("proves");
        let trust = Trust::new(dir.clone(), "t".into());
        let (_mine, my_cap) = trust.mint("bg").unwrap();
        let (theirs, their_cap) = trust.mint("bg").unwrap();
        assert!(trust.proves(&theirs, Some(&their_cap)));
        assert!(!trust.proves(&theirs, Some(&my_cap)));
        assert!(!trust.proves(&theirs, None));
        assert!(!trust.proves(&theirs, Some("")));
        assert!(!trust.proves("bg_never", Some(&their_cap)));
        assert!(!trust.proves(&theirs, Some(&their_cap[1..])), "a prefix");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The refusal budget: an authenticated caller that names foreign jobs one
    /// by one may put `REFUSAL_BUDGET` refusals in the log, not more.
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
        let (id, capability) = trust.claim(Some("call_9")).unwrap();
        assert!(id.starts_with("bg_"));
        assert!(trust.proves(&id, Some(&capability)));
        for _ in 1..CLAIM_BUDGET {
            trust.claim(None).unwrap();
        }
        let error = trust.claim(None).unwrap_err();
        assert!(error.contains("minted its"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_foreign_report_is_refused_and_a_path_out_of_the_session_is_refused() {
        let dir = temp_dir("check-job");
        std::fs::create_dir_all(dir.join("bg")).unwrap();
        let trust = Trust::new(dir.clone(), "t".into());
        let out = crate::util::path_string(&dir.join("bg").join("bg_1.out"));
        let report = json!({
            "id": "bg_1", "state": "running", "started": "2026-01-01T00:00:00Z",
            "out": out, "kind": "job"
        });
        let foreign = Request::new("bg", report.clone(), "job");
        let error = trust.check_job(&foreign).unwrap_err();
        assert!(error.contains("did not start job `bg_1`"), "{error}");

        // Minted: the id is the session's own, but a report without that job's
        // capability is still refused. The session token is what every tool
        // child has (STEER-7 review B1).
        let (id, capability) = trust.mint("bg").unwrap();
        let out = crate::util::path_string(&dir.join("bg").join(format!("{id}.out")));
        let report = json!({
            "id": id, "state": "running", "started": "2026-01-01T00:00:00Z",
            "out": out, "kind": "job"
        });
        let bare = Request::new("bg", report.clone(), "job").with_token(Some("t".into()));
        let error = trust.check_job(&bare).unwrap_err();
        assert!(
            error.contains("does not carry that job's capability"),
            "{error}"
        );
        assert!(
            !error.contains(&capability),
            "the refusal echoed the secret"
        );

        let wrong = bare
            .clone()
            .with_capability(Some("0".repeat(CAP_BYTES * 2)));
        assert!(trust.check_job(&wrong).is_err());

        let right = bare.with_capability(Some(capability.clone()));
        assert!(
            trust.check_job(&right).is_ok(),
            "{:?}",
            trust.check_job(&right)
        );

        let stolen = json!({
            "id": id, "state": "running", "started": "2026-01-01T00:00:00Z",
            "out": crate::util::path_string(&dir.join("..").join("stolen.out")), "kind": "job"
        });
        let error = trust
            .check_job(&Request::new("bg", stolen, "job").with_capability(Some(capability.clone())))
            .unwrap_err();
        assert!(error.contains("not inside this session's own"), "{error}");

        // A `..` id never becomes a status path.
        let traversal = json!({
            "id": "../bg_9", "state": "running", "started": "2026-01-01T00:00:00Z",
            "out": out, "kind": "job"
        });
        assert!(
            trust
                .check_job(&Request::new("bg", traversal, "job").with_capability(Some(capability)))
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_spawn_into_another_directory_is_refused() {
        let dir = temp_dir("spawn");
        let bg = dir.join("bg");
        std::fs::create_dir_all(&bg).unwrap();
        let other = temp_dir("spawn-other");
        let trust = Trust::new(dir.clone(), "t".into());
        let (id, _capability) = trust.mint("bg").unwrap();
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
            spawn(&bg, &bg.join(format!("{id}.out")), &id),
            "job",
        );
        assert!(
            trust.check_spawn(&ok).is_ok(),
            "{:?}",
            trust.check_spawn(&ok)
        );
        // An id the session never minted: a child can pick any name it likes,
        // and it is still not this session's job.
        let made_up = Request::new(
            "bg-spawn",
            spawn(&bg, &bg.join("bg_picked.out"), "bg_picked"),
            "job",
        );
        let error = trust.check_spawn(&made_up).unwrap_err();
        assert!(error.contains("did not start job"), "{error}");
        // No id at all: the session mints one when it answers.
        let mut args = spawn(&bg, &bg.join("bg_new.out"), &id);
        args.as_object_mut().unwrap().remove("id");
        assert!(
            trust
                .check_spawn(&Request::new("bg-spawn", args, "job"))
                .is_ok()
        );
        let foreign = Request::new(
            "bg-spawn",
            spawn(&other.join("bg"), &other.join("bg").join("bg_1.out"), &id),
            "job",
        );
        let error = trust.check_spawn(&foreign).unwrap_err();
        assert!(error.contains("would live in"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&other);
    }
}
