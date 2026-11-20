//! The environment: every variable kelir reads or sets, in one table.
//!
//! `docs/reference/env.md` is generated from [`VARS`], so this module is the
//! only place a variable is *declared*: a name that is not here cannot be
//! documented, and [`var`]/[`var_os`] debug-assert that the name they are
//! asked for is known. The one exception is [`named`]: a `key_env` in the
//! config names *any* variable the person chose, so it cannot be a constant.
//!
//! Access goes through this module rather than `std::env` so that the drift
//! test can prove it: no `env::var` call may live outside this file
//! ([`tests::no_env_read_happens_outside_this_module`]).

use std::ffi::OsString;

/// Who reads or writes a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// kelir reads it (a knob, a path, a provider key).
    Read,
    /// kelir sets it for its children (a tool call, a background job).
    Set,
    /// Only a test sets it; the code path it arms is inert otherwise.
    TestHook,
}

impl Dir {
    pub fn name(self) -> &'static str {
        match self {
            Dir::Read => "read",
            Dir::Set => "set",
            Dir::TestHook => "test",
        }
    }
}

/// One variable: its name, who reads it, and the one line the docs table says.
#[derive(Debug, Clone, Copy)]
pub struct Var {
    pub name: &'static str,
    pub dir: Dir,
    /// What it does, in one sentence (the docs table's effect column).
    pub effect: &'static str,
}

const fn read(name: &'static str, effect: &'static str) -> Var {
    Var {
        name,
        dir: Dir::Read,
        effect,
    }
}

const fn set(name: &'static str, effect: &'static str) -> Var {
    Var {
        name,
        dir: Dir::Set,
        effect,
    }
}

const fn hook(name: &'static str, effect: &'static str) -> Var {
    Var {
        name,
        dir: Dir::TestHook,
        effect,
    }
}

/// Every variable kelir knows, sorted by name within each direction by the
/// docs renderer (the table is kept in reading order here: providers, paths,
/// knobs, then the ones kelir exports).
pub const VARS: &[Var] = &[
    // ---------------------------------------------------------- provider keys
    read(
        "OPENROUTER_API_KEY",
        "the OpenRouter provider's key (never printed: `kelir doctor` reports the source)",
    ),
    read(
        "KELIR_GO_API_KEY",
        "the OpenCode Go key, an override for opencode's own store",
    ),
    // ------------------------------------------------------------- locations
    read(
        "KELIR_CONFIG",
        "the config file to use instead of the machine/project search",
    ),
    read(
        "KELIR_CONFIG_DIR",
        "the machine config directory (`config.toml`, `prompt.md`, `skills/`)",
    ),
    read(
        "KELIR_STATE_DIR",
        "the state root (sessions, model caches, bg jobs, locks)",
    ),
    read(
        "KELIR_HOME",
        "replaces `$HOME`/`USERPROFILE` only (user skills, `~`); it does NOT move state or config, set `KELIR_STATE_DIR` and `KELIR_CONFIG_DIR` for that",
    ),
    read(
        "KELIR_DOCS_ROOT",
        "the `docs/reference` directory `kelir docs --generate` and `--check` read (a checkout, or a copy)",
    ),
    // ------------------------------------------------------------ session knobs
    read(
        "KELIR_BASE_URL",
        "overrides a provider's base URL (a proxy, or a test's mock server)",
    ),
    read(
        "KELIR_PROMOTE_AFTER",
        "seconds before a still-running bash call becomes a background job (overrides `promote_after`)",
    ),
    read(
        "KELIR_HERDR",
        "force Herdr status records on (`1`) or off (`0`)",
    ),
    read(
        "KELIR_ASCII",
        "non-empty: ASCII glyphs instead of Unicode in the TUI",
    ),
    read(
        "NO_COLOR",
        "non-empty (the convention): no colours; attributes and glyphs stay",
    ),
    read(
        "HC_TERMINAL_ID",
        "set by herdr-core in every pane: its presence makes kelir ask `hc get` once for the pane/space/tab line in the frozen system prompt",
    ),
    read("TERM", "`dumb` picks the ASCII glyph set"),
    read(
        "TERM_PROGRAM",
        "`herdr-telegram` marks a pane a Telegram bridge reads (a job reports `idle`, not `done`)",
    ),
    // ------------------------------------------------------ system locations
    read("HOME", "the home directory on Unix (`~`)"),
    read("USERPROFILE", "the home directory on Windows (`~`)"),
    read(
        "XDG_STATE_HOME",
        "Linux state root when `KELIR_STATE_DIR` is unset",
    ),
    read(
        "XDG_CONFIG_HOME",
        "Linux config root when `KELIR_CONFIG_DIR` is unset",
    ),
    read(
        "XDG_RUNTIME_DIR",
        "the socket directory on Unix, when a session has no directory yet",
    ),
    read(
        "APPDATA",
        "Windows config root when `KELIR_CONFIG_DIR` is unset",
    ),
    read(
        "LOCALAPPDATA",
        "Windows state root when `KELIR_STATE_DIR` is unset",
    ),
    read(
        "PATH",
        "where `bash`, `rg`, `opencode` and other programs are looked for",
    ),
    read(
        "COMSPEC",
        "the shell that runs a `.cmd`/`.bat` helper (`opencode`) on Windows",
    ),
    read(
        "SHELL",
        "the login shell, when `bash` is not on `PATH` (Unix)",
    ),
    // ------------------------------------------------------- set for children
    set(
        "KELIR_SOCK",
        "the session socket (a Unix socket path or a pipe name), so `kelir ctl`/`view`/`bg` reach the session",
    ),
    set(
        "KELIR_TOKEN",
        "the session's socket token: a tool child sends it with every request to the session",
    ),
    set("KELIR_SESSION_DIR", "the session's directory"),
    set("KELIR_CALL_ID", "the provider call id of the bash call"),
    set(
        "KELIR_BIN",
        "the kelir binary, so `kelir bg`/`kelir view` work off `PATH`",
    ),
    // ------------------------------------------------------------- test hooks
    hook(
        "KELIR_CRASH_AT",
        "abort at a named point (`before_append`, …): the crash tests",
    ),
    hook(
        "KELIR_TEST_PANIC",
        "panic once the TUI owns the terminal, to prove the restore path",
    ),
    hook(
        "KELIR_SHELL_RAW",
        "`cr`: send a POSIX script raw, keeping CRLF terminators",
    ),
];

/// The registry entry for `name`, if kelir declares it.
pub fn declared(name: &str) -> Option<&'static Var> {
    VARS.iter().find(|var| var.name == name)
}

/// Every declared variable with this direction, in table order.
pub fn with_dir(dir: Dir) -> impl Iterator<Item = &'static Var> {
    VARS.iter().filter(move |var| var.dir == dir)
}

/// Read a declared variable (UTF-8, `None` when unset or not UTF-8).
///
/// Debug builds assert the name is in [`VARS`]: a new knob that only exists in
/// a call site would be invisible to `docs/reference/env.md`.
pub fn var(name: &str) -> Option<String> {
    debug_assert!(
        declared(name).is_some(),
        "environment variable `{name}` is not in `envs::VARS`"
    );
    std::env::var(name).ok()
}

/// [`var`], as an OS string (a path may not be UTF-8).
pub fn var_os(name: &str) -> Option<OsString> {
    debug_assert!(
        declared(name).is_some(),
        "environment variable `{name}` is not in `envs::VARS`"
    );
    std::env::var_os(name)
}

/// The whole environment as (name, value) pairs, for a job that must run in
/// exactly its starter's environment (`kelir bg run` hands it to the session).
pub fn all() -> Vec<(String, String)> {
    std::env::vars().collect()
}

/// A variable someone else named: a provider's `key_env` from the config, which
/// can be any name kelir did not choose. Never documented, because it is not
/// kelir's to document.
pub fn named(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variable_is_declared_once_with_an_effect() {
        let mut names: Vec<&str> = VARS.iter().map(|var| var.name).collect();
        names.sort();
        let mut unique = names.clone();
        unique.dedup();
        assert_eq!(names, unique, "a variable is declared twice");
        for var in VARS {
            assert!(
                var.effect.len() > 10,
                "{}: the docs sentence is too short to be useful",
                var.name
            );
            assert!(
                !var.effect.contains('\n'),
                "{}: the docs table is one row, so the effect is one line",
                var.name
            );
            assert!(
                var.effect.ends_with(|c: char| !c.is_whitespace()),
                "{}: trailing space",
                var.name
            );
        }
        // Every direction has members: a docs table with no rows is a bug.
        for dir in [Dir::Read, Dir::Set, Dir::TestHook] {
            assert!(with_dir(dir).next().is_some(), "{:?} is empty", dir);
        }
    }

    /// The one rule this module exists for: `std::env` is read here and nowhere
    /// else, so a variable cannot exist in a call site but not in the docs.
    #[test]
    fn no_env_read_happens_outside_this_module() {
        let mut offenders = Vec::new();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        walk(&root, &mut offenders);
        assert!(
            offenders.is_empty(),
            "read the environment through `crate::envs` instead, so `docs/reference/env.md` \
             stays complete:\n{}",
            offenders.join("\n")
        );
    }

    fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if path.file_name().and_then(|n| n.to_str()) == Some("envs.rs") {
                continue;
            }
            for (number, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                if [
                    "env::var(",
                    "env::var_os(",
                    "env::vars(",
                    "env::vars_os(",
                    "env::var_os as",
                    "env::var as",
                    "env::{",
                ]
                .iter()
                .any(|p| code.contains(p))
                {
                    out.push(format!(
                        "{}:{}: {}",
                        path.display(),
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    /// Every name in the table is a real `KELIR_*`/`HC_*`-free name: the
    /// docs generation quotes them, so a typo would be a wrong instruction.
    #[test]
    fn declared_names_look_like_variables() {
        for var in VARS {
            assert!(
                var.name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "{}: not a variable name",
                var.name
            );
        }
        // The two provider keys keep their documented spelling; a rename here
        // breaks every machine's config.
        assert_eq!(
            declared("OPENROUTER_API_KEY").map(|v| v.dir),
            Some(Dir::Read)
        );
        assert_eq!(declared("KELIR_GO_API_KEY").map(|v| v.dir), Some(Dir::Read));
        // `named` is for variables kelir did not choose.
        assert!(declared("MY_OWN_KEY").is_none());
    }
}
