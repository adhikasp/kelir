//! Sources: the base prompt, instruction files and skills directories.
//! The system prompt is rendered **once** at `session_started` and then frozen: a source
//! edited on disk mid-session arrives as a `source` event (a message at the tail), never as
//! a rewrite of the prefix, because prompt caching is the point of rendering once.

use std::path::{Path, PathBuf};

use super::*;

pub const SKILLS_PLACEHOLDER: &str = "{{skills_dirs}}";

pub const MAX_INSTRUCTION_BYTES: usize = 64 * 1024;

pub const MAX_INSTRUCTIONS_TOTAL: usize = 256 * 1024;

/// The most one skill's `SKILL.md` contributes to a `source` message.
pub const MAX_SKILL_BYTES: usize = 64 * 1024;

/// The rendered system prompt: the text, its hash, and everything that went in.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub text: String,
    pub hash: String,
    pub sources: Vec<SourceInfo>,
    /// What a reader should hear about (a non-UTF-8 file, a truncation, a
    /// malformed skill), recorded as `error` events right after the start.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// The base prompt.
    Base,
    /// `AGENTS.md`, `CLAUDE.md`: the instruction files.
    Instruction,
    /// A skill's `SKILL.md`.
    Skill,
    /// A file of the configured memory directory (`memory_dir`): rendered into the frozen system at the
    /// start, announced as a `source` message when it changes (the model writes memory with `edit`).
    Memory,
    /// The one Herdr line (`src/herdr_ctx.rs`): a fact of the pane, rendered
    /// into the frozen system; it has no file, so it is never watched.
    Herdr,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Base => "prompt",
            SourceKind::Instruction => "instruction",
            SourceKind::Skill => "skill",
            SourceKind::Memory => "memory",
            SourceKind::Herdr => "herdr",
        }
    }

    /// The `source` event's `kind`. The spec's list has no `prompt`, so the base prompt's own
    /// edit arrives as an `instruction` (its `name` is `prompt.md`, which tells them apart).
    pub fn event_kind(self) -> &'static str {
        match self {
            SourceKind::Base | SourceKind::Instruction => "instruction",
            SourceKind::Skill => "skill",
            SourceKind::Memory => "memory",
            SourceKind::Herdr => "herdr",
        }
    }

    /// The `source` event's kind, read back, or a `sources[]` entry's `kind`.
    pub fn parse(kind: &str) -> SourceKind {
        match kind {
            "prompt" => SourceKind::Base,
            "skill" => SourceKind::Skill,
            "memory" => SourceKind::Memory,
            "herdr" => SourceKind::Herdr,
            _ => SourceKind::Instruction,
        }
    }
}

/// Where a source's text comes from. The embedded default is a *typed* absence
/// of a path, never a sentinel string the watcher could try to read as a file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SourcePath {
    /// The base prompt compiled into the binary (`src/prompt.md`): no file.
    #[default]
    Embedded,
    /// A skill compiled into the binary (`src/docs/skill-kelir.md`): no file, so
    /// it is never watched, re-read or announced as changed.
    Builtin,
    File(PathBuf),
}

impl SourcePath {
    /// The `sources[]` path: empty for the embedded default, a real path
    /// otherwise.
    pub fn log_path(&self) -> String {
        match self {
            SourcePath::Embedded | SourcePath::Builtin => String::new(),
            SourcePath::File(path) => crate::util::path_string(path),
        }
    }

    /// The path a `sources[]` entry (or a `source` event) names.
    pub fn from_log(kind: SourceKind, path: &str) -> SourcePath {
        if path.is_empty() || (kind == SourceKind::Base && path == "embedded") {
            return SourcePath::Embedded;
        }
        SourcePath::File(PathBuf::from(
            path.replace('/', std::path::MAIN_SEPARATOR_STR),
        ))
    }

    pub fn file(&self) -> Option<&Path> {
        match self {
            SourcePath::Embedded | SourcePath::Builtin => None,
            SourcePath::File(path) => Some(path),
        }
    }

    /// What to call it in a note: a reader can open a real path, and cannot
    /// open a name.
    pub fn describe(&self) -> String {
        match self {
            SourcePath::Embedded => "the embedded base prompt".to_string(),
            SourcePath::Builtin => "a skill embedded in the kelir binary".to_string(),
            SourcePath::File(path) => crate::util::path_string(path),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Machine,
    Project,
}

/// One discovered source, before it is rendered.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub kind: SourceKind,
    /// The name a clash is decided on: the file name, or the skill directory's.
    pub name: String,
    pub path: PathBuf,
    pub text: String,
    pub layer: Layer,
    /// Compiled into the binary (`src/docs/skill-kelir.md`) rather than read
    /// from `path`: there is nothing on disk to watch, re-read or announce.
    pub builtin: bool,
}

/// Where sources come from: every directory a session looks in.
#[derive(Debug, Clone, Default)]
pub struct Places {
    /// The machine config dir (`prompt.md`, `skills/`).
    pub config_dir: Option<PathBuf>,
    /// `~/.claude` (`skills/`): the machine layer the ecosystem uses.
    pub machine_claude: Option<PathBuf>,
    /// The config's `skills_dirs`, machine first.
    pub skills_dirs: Vec<PathBuf>,
    /// The session cwd: instruction files are found by walking up from here.
    pub cwd: PathBuf,
    /// Where the walk up stops (the home directory): a file above it is not the
    /// project's business.
    pub home: Option<PathBuf>,
    /// Where the walk up stops (the repository root): a directory holding
    /// `.git`. Set by the caller that knows the session's cwd.
    pub repo_root: Option<PathBuf>,
    /// The configured memory directory (`memory_dir`), already resolved against
    /// the cwd; `None` is memory off.
    pub memory_dir: Option<PathBuf>,
}

/// The paths this process looks in, built from the config and the environment.
pub fn places(cwd: &Path, skills_dirs: &[PathBuf], config_dir: Option<PathBuf>) -> Places {
    let home = crate::paths::home();
    Places {
        config_dir,
        machine_claude: Some(home.join(".claude")),
        skills_dirs: skills_dirs.to_vec(),
        cwd: cwd.to_path_buf(),
        home: Some(home),
        repo_root: repo_root_above(cwd),
        memory_dir: None,
    }
}

/// The nearest ancestor of `cwd` (itself included) that holds a `.git` entry:
/// the directory the walk up stops at.
pub(crate) fn repo_root_above(cwd: &Path) -> Option<PathBuf> {
    let mut dir = Some(cwd);
    while let Some(current) = dir {
        if current.join(".git").exists() {
            return Some(current.to_path_buf());
        }
        dir = current.parent();
    }
    None
}

impl Places {
    /// The directories skills are discovered in, machine layer first, without
    /// duplicates.
    pub fn skills_directories(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        let mut push = |path: PathBuf| {
            if !out.contains(&path) {
                out.push(path);
            }
        };
        if let Some(claude) = &self.machine_claude {
            push(claude.join("skills"));
        }
        if let Some(config) = &self.config_dir {
            push(config.join("skills"));
        }
        for dir in &self.skills_dirs {
            push(dir.clone());
        }
        push(self.cwd.join(".claude").join("skills"));
        push(self.cwd.join(".kelir").join("skills"));
        out
    }

    /// Which layer a directory belongs to: the machine ones come first in
    /// [`Places::skills_directories`], the project ones (`.claude`, `.kelir`) are last.
    pub(crate) fn skills_layer(&self, directory: &Path) -> Layer {
        let cwd_skills = [
            self.cwd.join(".claude").join("skills"),
            self.cwd.join(".kelir").join("skills"),
        ];
        if cwd_skills.iter().any(|path| path == directory) {
            Layer::Project
        } else {
            Layer::Machine
        }
    }
}
