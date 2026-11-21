//! Per-OS state and config directories.
//!
//! State: `%LOCALAPPDATA%\kelir` (Windows), `~/Library/Application Support/kelir`
//! (macOS), `$XDG_STATE_HOME/kelir` or `~/.local/state/kelir` (Linux).
//! Config: `%APPDATA%\kelir` (Windows), the same Application Support dir
//! (macOS), `$XDG_CONFIG_HOME/kelir` or `~/.config/kelir` (Linux).

use std::path::{Path, PathBuf};

fn env_path(var: &str) -> Option<PathBuf> {
    crate::envs::var_os(var)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// The home directory. `KELIR_HOME` wins; otherwise `std::env::home_dir` asks
/// the OS (`$HOME` on Unix, with a `getpwuid_r` fallback; `%USERPROFILE%` or
/// `%HOMEDRIVE%%HOMEPATH%` on Windows).
///
/// The override stays first so a run never reads the machine's own
/// `~/.claude/skills`: discovery must see the same inputs every time, and a
/// person's home is not a test fixture.
pub fn home() -> PathBuf {
    env_path("KELIR_HOME").unwrap_or_else(|| {
        // `std::env::home_dir` is un-deprecated (1.87) and has a Unix fallback
        // (1.90): it is the OS's answer, not a hand-rolled env lookup. The
        // override stays first so tests never read the machine's own home.
        std::env::home_dir().unwrap_or_else(|| PathBuf::from("."))
    })
}

/// Root of all kelir state (`sessions/<id>/…` lives below it).
pub fn state_dir() -> PathBuf {
    if let Some(p) = env_path("KELIR_STATE_DIR") {
        return p;
    }
    let home = home();
    if cfg!(windows) {
        env_path("LOCALAPPDATA")
            .unwrap_or_else(|| home.join("AppData").join("Local"))
            .join("kelir")
    } else if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("kelir")
    } else {
        env_path("XDG_STATE_HOME")
            .unwrap_or_else(|| home.join(".local").join("state"))
            .join("kelir")
    }
}

/// Directory holding the machine-level `config.toml` and `prompt.md`.
pub fn config_dir() -> PathBuf {
    if let Some(p) = env_path("KELIR_CONFIG_DIR") {
        return p;
    }
    let home = home();
    if cfg!(windows) {
        env_path("APPDATA")
            .unwrap_or_else(|| home.join("AppData").join("Roaming"))
            .join("kelir")
    } else if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("kelir")
    } else {
        env_path("XDG_CONFIG_HOME")
            .unwrap_or_else(|| home.join(".config"))
            .join("kelir")
    }
}

pub fn machine_config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn sessions_dir() -> PathBuf {
    state_dir().join("sessions")
}

/// `<state>/sessions/<id>`.
pub fn session_dir(id: &str) -> PathBuf {
    sessions_dir().join(id)
}

/// Create a directory only the user can reach: mode 0700 on Unix, where a
/// umask like `002` would otherwise make the session log and its socket
/// group-readable. Windows inherits the profile's ACL, which already denies
/// other users (M1 review N4).
pub fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        // `DirBuilder::mode` is filtered by the umask; an explicit chmod is not.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)?;
    Ok(())
}

/// Session ids on disk, newest first. ULIDs sort by time, so the directory
/// names sort into creation order.
pub fn sessions_newest_first() -> Vec<String> {
    sessions_newest_first_in(&sessions_dir())
}

pub fn sessions_newest_first_in(base: &Path) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("ses_"))
        .collect();
    ids.sort();
    ids.reverse();
    ids
}

/// Resolve `kelir ctl <session>`: an exact id, a unique prefix, or `latest`.
///
/// Returns the candidates in the order to try them: one for an exact id, the
/// matches for a prefix, and the newest session alone for `latest` (a verb sent
/// to `latest` must never land on an older session because the newest one is
/// dead).
pub fn session_candidates(spec: &str) -> Result<Vec<String>, String> {
    session_candidates_in(&sessions_dir(), spec)
}

pub fn session_candidates_in(base: &Path, spec: &str) -> Result<Vec<String>, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("no session given (an id, a unique prefix, or `latest`)".into());
    }
    let all = sessions_newest_first_in(base);
    if spec == "latest" {
        let Some(newest) = all.first() else {
            return Err(format!("no sessions yet in {}", base.display()));
        };
        // `latest` is the newest session and only that one: `latest end` on a
        // dead newest session reports it dead instead of killing an older one.
        return Ok(vec![newest.clone()]);
    }
    if all.iter().any(|id| id == spec) {
        return Ok(vec![spec.to_string()]);
    }
    let matches: Vec<String> = all
        .iter()
        .filter(|id| id.starts_with(spec))
        .cloned()
        .collect();
    match matches.len() {
        0 => Err(format!("no session matches `{spec}`")),
        _ => Ok(matches),
    }
}

/// Resolve `kelir resume <spec>`: an exact id, a unique prefix, or `latest`
/// (the newest session, dead or alive — resuming is how a dead one continues).
pub fn resolve_session(spec: &str) -> Result<String, String> {
    resolve_session_in(&sessions_dir(), spec)
}

pub fn resolve_session_in(base: &Path, spec: &str) -> Result<String, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("no session given (an id, a unique prefix, or `latest`)".into());
    }
    let all = sessions_newest_first_in(base);
    if spec == "latest" {
        return all
            .first()
            .cloned()
            .ok_or_else(|| format!("no sessions yet in {}", base.display()));
    }
    if all.iter().any(|id| id == spec) {
        return Ok(spec.to_string());
    }
    let matches: Vec<&String> = all.iter().filter(|id| id.starts_with(spec)).collect();
    match matches.len() {
        0 => Err(format!("no session matches `{spec}`")),
        1 => Ok(matches[0].clone()),
        count => Err(format!(
            "`{spec}` matches {count} sessions; give more of the id"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_sessions(name: &str, ids: &[&str]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kelir-paths-{}-{name}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&dir).unwrap();
        for id in ids {
            std::fs::create_dir_all(dir.join(id)).unwrap();
        }
        dir
    }

    #[test]
    fn state_dir_ends_with_kelir_or_override() {
        let dir = state_dir();
        assert!(dir.is_absolute() || dir.starts_with("."));
        // Every platform puts kelir in a directory of that name unless the
        // caller overrode it via the environment (tests do, at times).
        if crate::envs::var_os("KELIR_STATE_DIR").is_none() {
            assert_eq!(dir.file_name().unwrap(), "kelir");
        }
    }

    #[test]
    fn session_dir_is_under_sessions() {
        let dir = session_dir("ses_test");
        assert!(dir.ends_with("sessions/ses_test") || dir.ends_with("sessions\\ses_test"));
    }

    #[test]
    fn sessions_are_listed_newest_first() {
        let base = temp_sessions("order", &["ses_01a", "ses_03c", "ses_02b"]);
        assert_eq!(
            sessions_newest_first_in(&base),
            vec![
                "ses_03c".to_string(),
                "ses_02b".to_string(),
                "ses_01a".to_string()
            ]
        );
    }

    #[test]
    fn an_exact_id_wins_over_a_prefix() {
        let base = temp_sessions("exact", &["ses_01a", "ses_02b"]);
        assert_eq!(
            session_candidates_in(&base, "ses_01a").unwrap(),
            vec!["ses_01a".to_string()]
        );
    }

    #[test]
    fn a_prefix_resolves_to_its_matches_newest_first() {
        let base = temp_sessions("prefix", &["ses_aaa1", "ses_aaa2", "ses_bbb"]);
        assert_eq!(
            session_candidates_in(&base, "ses_aaa").unwrap(),
            vec!["ses_aaa2".to_string(), "ses_aaa1".to_string()]
        );
    }

    #[test]
    fn latest_is_the_newest_session_only() {
        let base = temp_sessions("latest", &["ses_01a", "ses_02b"]);
        assert_eq!(
            session_candidates_in(&base, "latest").unwrap(),
            vec!["ses_02b".to_string()],
            "a dead newest session must not fall through to an older one"
        );
    }

    #[test]
    fn resume_resolves_one_session_and_refuses_an_ambiguous_prefix() {
        let base = temp_sessions("resume", &["ses_aaa1", "ses_aaa2", "ses_bbb"]);
        assert_eq!(
            resolve_session_in(&base, "ses_bbb").unwrap(),
            "ses_bbb".to_string()
        );
        assert_eq!(
            resolve_session_in(&base, "ses_b").unwrap(),
            "ses_bbb".to_string()
        );
        assert_eq!(
            resolve_session_in(&base, "latest").unwrap(),
            "ses_bbb".to_string()
        );
        let error = resolve_session_in(&base, "ses_aaa").unwrap_err();
        assert!(error.contains("matches 2 sessions"), "{error}");
        assert!(resolve_session_in(&base, "ses_zzz").is_err());
        assert!(resolve_session_in(&base, " ").is_err());
        let empty = temp_sessions("resume-empty", &[]);
        assert!(resolve_session_in(&empty, "latest").is_err());
    }

    #[test]
    fn an_unknown_session_is_an_error() {
        let base = temp_sessions("missing", &["ses_01a"]);
        let error = session_candidates_in(&base, "ses_zzz").unwrap_err();
        assert!(error.contains("no session matches"), "{error}");
        let empty = temp_sessions("empty", &[]);
        assert!(session_candidates_in(&empty, "latest").is_err());
        assert!(session_candidates_in(&base, "  ").is_err());
    }
}
