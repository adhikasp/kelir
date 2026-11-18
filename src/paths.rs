//! Per-OS state and config directories.
//!
//! State: `%LOCALAPPDATA%\kelir` (Windows), `~/Library/Application Support/kelir`
//! (macOS), `$XDG_STATE_HOME/kelir` or `~/.local/state/kelir` (Linux).
//! Config: `%APPDATA%\kelir` (Windows), the same Application Support dir
//! (macOS), `$XDG_CONFIG_HOME/kelir` or `~/.config/kelir` (Linux).

use std::path::PathBuf;

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

fn home() -> PathBuf {
    env_path("HOME")
        .or_else(|| env_path("USERPROFILE"))
        .unwrap_or_else(|| PathBuf::from("."))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_ends_with_kelir_or_override() {
        let dir = state_dir();
        assert!(dir.is_absolute() || dir.starts_with("."));
        // Every platform puts kelir in a directory of that name unless the
        // caller overrode it via the environment (tests do, at times).
        if std::env::var_os("KELIR_STATE_DIR").is_none() {
            assert_eq!(dir.file_name().unwrap(), "kelir");
        }
    }

    #[test]
    fn session_dir_is_under_sessions() {
        let dir = session_dir("ses_test");
        assert!(dir.ends_with("sessions/ses_test") || dir.ends_with("sessions\\ses_test"));
    }
}
