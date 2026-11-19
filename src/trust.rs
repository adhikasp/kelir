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
//! * **A registry of the jobs this session created.** `bg`, `cancel` and
//!   `bg-spawn` name a job id; the session only answers for ids it started or
//!   registered (its own `commands/<id>/` output dirs, its own
//!   `bg/<id>.status` files, and what it registered in memory), and every job
//!   directory and status/out path in a request must resolve *inside* the
//!   session's own `bg/` (canonicalised: `..`, a symlink out of it and, on
//!   Windows, a UNC or another drive's path are all refused).
//!
//! [`check_job`] is that second half and runs in the session, where a refusal
//! becomes a `verb` event (the caller is authenticated, so the log should say
//! what it tried). [`Trust::accepts`] is the first half and runs at the socket,
//! where a refusal is just an answer.

use std::collections::HashSet;
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
/// The longest job id the session will look at. Ids are `bg_<ulid>` or a call's
/// output dir name, both far shorter.
pub const MAX_ID: usize = 128;

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
pub fn valid_job_id(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty()
        && id.len() <= MAX_ID
        && !id.starts_with('.')
        && !id.contains("..")
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
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

/// One session's trust state: its token and the jobs it will answer for.
#[derive(Debug)]
pub struct Trust {
    dir: PathBuf,
    token: String,
    /// Ids this process registered in memory: a call it started, a job it
    /// spawned. The `bg/` directory is the durable half of the registry.
    ids: Mutex<HashSet<String>>,
}

impl Trust {
    pub fn new(dir: PathBuf, token: String) -> Self {
        Self {
            dir,
            token,
            ids: Mutex::new(HashSet::new()),
        }
    }

    /// A trust state with a fresh token, written to the session dir.
    pub fn create(dir: PathBuf) -> Result<Self, String> {
        // The session's own `bg/` exists from here on, so a spawn check can
        // canonicalise it and a job's status file has a home even before the
        // first job (STEER-7).
        std::fs::create_dir_all(dir.join("bg"))
            .map_err(|error| format!("cannot create {}: {error}", dir.join("bg").display()))?;
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

    /// Remember a job this session started (a call's output dir name, or a
    /// `bg` job an explicit `kelir bg run` made inside one of its calls).
    pub fn register(&self, id: &str) {
        let id = id.trim();
        if !valid_job_id(id) {
            return;
        }
        if let Ok(mut ids) = self.ids.lock() {
            ids.insert(id.to_string());
        }
    }

    /// Is `id` a job of this session? The in-memory registry, a status file in
    /// its own `bg/`, or a call's output directory in its own `commands/`.
    pub fn owns(&self, id: &str) -> bool {
        if !valid_job_id(id) {
            return false;
        }
        if self.ids.lock().map(|ids| ids.contains(id)).unwrap_or(false) {
            return true;
        }
        if self.bg_dir().join(format!("{id}.status")).is_file() {
            return true;
        }
        self.dir.join("commands").join(id).is_dir()
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

    /// The `bg-spawn` half, which has its own verb (a spawn has no status file
    /// yet, so "is it mine" is a question about the directory it would live in).
    pub fn check_spawn(&self, request: &Request) -> Result<(), String> {
        if request.verb != "bg-spawn" {
            return Ok(());
        }
        let spawn: crate::bg::SpawnRequest = serde_json::from_value(request.args.clone())
            .map_err(|error| format!("bg-spawn: not a spawn request: {error}"))?;
        if !valid_job_id(&spawn.id) {
            return Err(format!(
                "bg-spawn: `{}` is not a job id (one path component: letters, digits, `.`, `_`, \
                 `-`)",
                spawn.id
            ));
        }
        // A new job id cannot be in the registry yet; what can be proved is
        // that the job would live in this session's own `bg/`.
        let ours = self.bg_dir();
        let base = std::fs::canonicalize(&ours)
            .map_err(|error| format!("cannot resolve {}: {error}", ours.display()))?;
        let real = match resolve_inside(&base, &spawn.dir) {
            Ok(real) => real,
            // The resolution already refused it (`..`, a symlink out, a UNC or
            // another drive); the answer says what the spawn asked for.
            Err(_) => {
                return Err(format!(
                    "bg-spawn: job `{}` would live in `{}`, which is not inside this session's \
                     {}",
                    spawn.id,
                    spawn.dir.trim(),
                    base.display()
                ));
            }
        };
        if real != base {
            return Err(format!(
                "bg-spawn: job `{}` would live in {}, not in this session's {}",
                spawn.id,
                real.display(),
                base.display()
            ));
        }
        if !spawn.out.trim().is_empty() {
            self.check_job_path(&spawn.out, "out")?;
        }
        Ok(())
    }

    /// The session half: `bg` and `cancel` name a job id, and the id (and the
    /// paths that come with it) must be this session's own.
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

#[cfg(test)]
mod tests {
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
        ] {
            assert!(!valid_job_id(bad), "{bad} was accepted");
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
            "..\\..\\elsewhere",
            "\\\\host\\share\\x",
            "//host/share/x",
        ] {
            assert!(
                resolve_inside(&dir.join("bg"), bad).is_err(),
                "{bad} escaped"
            );
        }
        #[cfg(windows)]
        {
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

    #[test]
    fn the_registry_is_the_sessions_own_bg_and_commands() {
        let dir = temp_dir("owns");
        std::fs::create_dir_all(dir.join("bg")).unwrap();
        std::fs::create_dir_all(dir.join("commands").join("12-3-2")).unwrap();
        let trust = Trust::new(dir.clone(), "t".into());
        assert!(trust.owns("12-3-2"), "a call's output dir is its own");
        assert!(!trust.owns("bg_other"));
        std::fs::write(dir.join("bg").join("bg_1.status"), "{}").unwrap();
        assert!(trust.owns("bg_1"), "a status file in bg/ is its own");
        trust.register("bg_2");
        assert!(trust.owns("bg_2"));
        trust.register("../etc/passwd");
        assert!(!trust.owns("../etc/passwd"));
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

        trust.register("bg_1");
        assert!(trust.check_job(&foreign).is_ok());

        let stolen = json!({
            "id": "bg_1", "state": "running", "started": "2026-01-01T00:00:00Z",
            "out": crate::util::path_string(&dir.join("..").join("stolen.out")), "kind": "job"
        });
        let error = trust
            .check_job(&Request::new("bg", stolen, "job"))
            .unwrap_err();
        assert!(error.contains("not inside this session's own"), "{error}");

        // A `..` id never becomes a status path.
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
    fn a_spawn_into_another_directory_is_refused() {
        let dir = temp_dir("spawn");
        let bg = dir.join("bg");
        std::fs::create_dir_all(&bg).unwrap();
        let other = temp_dir("spawn-other");
        let trust = Trust::new(dir.clone(), "t".into());
        let spawn = |job_dir: &Path, out: &Path| {
            json!({
                "dir": crate::util::path_string(job_dir),
                "id": "bg_1",
                "out": crate::util::path_string(out),
                "cmd": "true",
                "shell": ["bash"],
                "kind": "job",
            })
        };
        let ok = Request::new("bg-spawn", spawn(&bg, &bg.join("bg_1.out")), "job");
        assert!(
            trust.check_spawn(&ok).is_ok(),
            "{:?}",
            trust.check_spawn(&ok)
        );
        let foreign = Request::new(
            "bg-spawn",
            spawn(&other.join("bg"), &other.join("bg").join("bg_1.out")),
            "job",
        );
        let error = trust.check_spawn(&foreign).unwrap_err();
        assert!(error.contains("would live in"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&other);
    }
}
