//! Sources: the base prompt, instruction files and skills directories.
//!
//! The system prompt is rendered **once** at `session_started` and then frozen:
//! a source edited on disk mid-session arrives as a `source` event (a message
//! at the tail), never as a rewrite of the prefix, because prompt caching is
//! the point of rendering once.
//!
//! Two layers, machine then project, and the project wins on a name clash:
//!
//! * the base prompt: the embedded default (at most 1 KB), overridden by an
//!   explicit `prompt` file, by `<config>/prompt.md` and by `<cwd>/.kelir/prompt.md`;
//! * instruction files: every `AGENTS.md` and `CLAUDE.md` found walking up from
//!   the session cwd (nearest first), in the machine config dir too;
//! * skills directories: the config's `skills_dirs`, `<config>/skills`,
//!   `~/.claude/skills` for the machine layer, and `.claude/skills`,
//!   `.kelir/skills` for the project one, read-only.
//!
//! Skills are **pull**: the base prompt names the directories and nothing else.
//! A skill is a directory holding a `SKILL.md` with Agent Skills front matter
//! (`name`, `description`, optional `metadata`); reading it is the model's job
//! (with `bash`), so its body never costs a token until the task needs it.
//!
//! One rule governs every source text, whoever reads it ([`read_source`]): the
//! BOM is dropped, invalid UTF-8 is decoded lossily and said so, and the text is
//! capped with a visible truncation notice. The render, the watcher and the
//! `skills` verb all go through it, so a file hashes the same everywhere and is
//! never announced twice.

use std::path::{Path, PathBuf};

use crate::event::{LogEvent, SourceInfo};

/// The placeholder in the base prompt where the skills directories go.
pub const SKILLS_PLACEHOLDER: &str = "{{skills_dirs}}";

/// The most one instruction file contributes to the prompt.
pub const MAX_INSTRUCTION_BYTES: usize = 64 * 1024;
/// The most all instruction files together contribute.
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
}

impl SourceKind {
    /// The render name kelir uses for the source.
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Base => "prompt",
            SourceKind::Instruction => "instruction",
            SourceKind::Skill => "skill",
        }
    }

    /// The `source` event's `kind`. The spec's list has no `prompt`, so the
    /// base prompt's own edit arrives as an `instruction` (its `name` is
    /// `prompt.md`, which tells them apart).
    pub fn event_kind(self) -> &'static str {
        match self {
            SourceKind::Base | SourceKind::Instruction => "instruction",
            SourceKind::Skill => "skill",
        }
    }

    /// The `source` event's kind, read back, or a `sources[]` entry's `kind`.
    pub fn parse(kind: &str) -> SourceKind {
        match kind {
            "prompt" => SourceKind::Base,
            "skill" => SourceKind::Skill,
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
    File(PathBuf),
}

impl SourcePath {
    /// The `sources[]` path: empty for the embedded default, a real path
    /// otherwise.
    pub fn log_path(&self) -> String {
        match self {
            SourcePath::Embedded => String::new(),
            SourcePath::File(path) => crate::util::path_string(path),
        }
    }

    /// The path a `sources[]` entry (or a `source` event) names.
    ///
    /// An old log wrote the literal `embedded` for the base prompt; that
    /// sentinel is read back as [`SourcePath::Embedded`] for the base prompt
    /// only, so an unrelated file called `embedded` is a file like any other.
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
            SourcePath::Embedded => None,
            SourcePath::File(path) => Some(path),
        }
    }

    /// What to call it in a note: a reader can open a real path, and cannot
    /// open a name.
    pub fn describe(&self) -> String {
        match self {
            SourcePath::Embedded => "the embedded base prompt".to_string(),
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
    }
}

/// The nearest ancestor of `cwd` (itself included) that holds a `.git` entry:
/// the directory the walk up stops at.
fn repo_root_above(cwd: &Path) -> Option<PathBuf> {
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
    /// [`Places::skills_directories`], the project ones (`.claude`, `.kelir`)
    /// are last.
    fn skills_layer(&self, directory: &Path) -> Layer {
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

/// The instruction file names, in the order they render within one directory.
const INSTRUCTION_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// Discover every machine and project source for `places`, dropping the notes.
pub fn discover(places: &Places) -> Vec<Found> {
    let mut notes = Vec::new();
    discover_with_notes(places, &mut notes)
}

/// Discover every machine and project source for `places`.
///
/// The order is the render order: machine layer, then project layer (nearest
/// instruction file first). A project source of a name replaces a machine one;
/// within a layer every file is kept, so a monorepo's root rules and a
/// subdirectory's rules both reach the model (F4).
pub fn discover_with_notes(places: &Places, notes: &mut Vec<String>) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    let mut budget = MAX_INSTRUCTIONS_TOTAL;
    // Machine instruction files: the config dir's own AGENTS.md/CLAUDE.md.
    if let Some(config) = &places.config_dir {
        for name in INSTRUCTION_FILES {
            let path = config.join(name);
            let Some(text) = read_file(&path, MAX_INSTRUCTION_BYTES, &mut budget, notes) else {
                continue;
            };
            push_source(
                &mut found,
                SourceKind::Instruction,
                name,
                path,
                text,
                Layer::Machine,
            );
        }
    }
    // Project instruction files: every one found walking up from the cwd,
    // nearest first. The walk stops at the repository root and at the home
    // directory (N3), so a file planted above them is not picked up.
    for current in ancestors(places) {
        for name in INSTRUCTION_FILES {
            let path = current.join(name);
            let Some(text) = read_file(&path, MAX_INSTRUCTION_BYTES, &mut budget, notes) else {
                continue;
            };
            push_source(
                &mut found,
                SourceKind::Instruction,
                name,
                path,
                text,
                Layer::Project,
            );
        }
    }
    // Skills directories, machine layer then project. A skill's name is its
    // directory's name; a project skill of that name wins.
    for directory in places.skills_directories() {
        let layer = places.skills_layer(&directory);
        for skill in skills_in(&directory, notes) {
            push_source(
                &mut found,
                SourceKind::Skill,
                &skill.name,
                skill.path,
                skill.text,
                layer,
            );
        }
    }
    found
}

/// The cwd and its ancestors, nearest first, stopping at the repository root
/// (a directory that holds `.git`) and at the home directory (inclusive: a file
/// *in* home still belongs to the project, one above it does not).
fn ancestors(places: &Places) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dir = Some(places.cwd.clone());
    while let Some(current) = dir {
        let stop = places.repo_root.as_deref() == Some(current.as_path())
            || places
                .home
                .as_deref()
                .filter(|home| !home.as_os_str().is_empty())
                == Some(current.as_path());
        out.push(current.clone());
        if stop {
            break;
        }
        dir = current.parent().map(Path::to_path_buf);
    }
    out
}

/// Add a source, resolving a name clash the way the two layers demand: a
/// project source wins over a machine one of the same name. Within one layer
/// nothing is dropped (F4): a monorepo's root `AGENTS.md` and a subdirectory's
/// `AGENTS.md` are two files, and both are the model's business.
fn push_source(
    found: &mut Vec<Found>,
    kind: SourceKind,
    name: &str,
    path: PathBuf,
    text: String,
    layer: Layer,
) {
    let item = Found {
        kind,
        name: name.to_string(),
        path,
        text,
        layer,
    };
    if layer == Layer::Project {
        // A project file replaces the machine one of its name, wherever the
        // machine one sits in the list.
        while let Some(index) = found.iter().position(|other| {
            other.kind == kind && other.name == item.name && other.layer == Layer::Machine
        }) {
            found.remove(index);
        }
        found.push(item);
        return;
    }
    if found
        .iter()
        .any(|other| other.kind == kind && other.name == item.name)
    {
        return;
    }
    found.push(item);
}

/// Every skill in one directory, sorted by name so the render is stable.
///
/// A directory whose `SKILL.md` is missing, unreadable or not valid Agent
/// Skills front matter is not a skill: it is skipped with a note, never listed
/// in `sources[]` (F6).
fn skills_in(directory: &Path, notes: &mut Vec<String>) -> Vec<SkillFile> {
    let mut out: Vec<SkillFile> = Vec::new();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let file = path.join("SKILL.md");
        let mut budget = MAX_SKILL_BYTES;
        let mut inner = Vec::new();
        let Some(text) = read_file(&file, MAX_SKILL_BYTES, &mut budget, &mut inner) else {
            continue;
        };
        if let Err(reason) = parse_skill(&text) {
            notes.push(format!(
                "source: {} is not a skill ({reason}); it was skipped",
                crate::util::path_string(&file)
            ));
            continue;
        }
        notes.append(&mut inner);
        out.push(SkillFile {
            name: name.to_string(),
            path: file,
            text,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// One skill found in a skills directory.
#[derive(Debug)]
struct SkillFile {
    /// The skill's name: its directory's name.
    name: String,
    path: PathBuf,
    text: String,
}

/// Read a source file: BOM dropped, invalid UTF-8 decoded lossily, length
/// capped, and each of those stated in `notes`.
///
/// `budget` is the room left in the total cap for this kind of source; it is
/// reduced by this file's bytes. `None` means the file is missing, is not a
/// file, could not be read, or had no room left at all.
fn read_file(
    path: &Path,
    limit: usize,
    budget: &mut usize,
    notes: &mut Vec<String>,
) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            notes.push(format!(
                "source: cannot read {}: {error}",
                crate::util::path_string(path)
            ));
            return None;
        }
    };
    if *budget == 0 {
        notes.push(format!(
            "source: {} not read: the {MAX_INSTRUCTIONS_TOTAL}-byte instruction budget is spent",
            crate::util::path_string(path)
        ));
        return None;
    }
    // The total budget skips a file rather than truncating it, so every file
    // that does go in is read the same way the watcher re-reads it later (a
    // truncation here that the watcher could not reproduce would announce the
    // file again at the first boundary). A single file over its own cap is
    // truncated by `decode` instead: the user can see why.
    let room = limit.min(*budget);
    if bytes.len() > room && room < limit {
        notes.push(format!(
            "source: {} not read: {} bytes left of the {MAX_INSTRUCTIONS_TOTAL}-byte \
             instruction budget, the file is {} bytes",
            crate::util::path_string(path),
            *budget,
            bytes.len()
        ));
        return None;
    }
    let mut messages = Vec::new();
    let text = decode(&bytes, room, &mut messages);
    for message in messages {
        notes.push(format!(
            "source: {} {message}",
            crate::util::path_string(path)
        ));
    }
    *budget = budget.saturating_sub(bytes.len().min(room));
    Some(text)
}

/// The one decode rule every source text goes through: a UTF-8 BOM is dropped,
/// invalid UTF-8 becomes U+FFFD (and is said so), and text over `limit` bytes is
/// cut on a char boundary with a visible truncation notice (F7, N6).
fn decode(bytes: &[u8], limit: usize, notes: &mut Vec<String>) -> String {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let mut text = match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            notes.push(format!(
                "is not valid UTF-8 ({} bytes); invalid bytes read as U+FFFD",
                bytes.len()
            ));
            String::from_utf8_lossy(bytes).into_owned()
        }
    };
    if text.len() > limit {
        let total = text.len();
        text = crate::util::truncate_chars(&text, limit);
        text.push_str(&format!(
            "\n\n[kelir: truncated; {total} bytes on disk, the first {limit} shown]"
        ));
        notes.push(format!(
            "is {total} bytes; only the first {limit} were read"
        ));
    }
    text
}

/// Read one source file whole, the way the watcher and the `skills` verb do:
/// the same decode rule, so the same bytes hash the same everywhere.
///
/// `None` when it is missing or unreadable: a deleted source is silent.
pub fn read_text(path: &Path) -> Option<String> {
    read_source(path, MAX_INSTRUCTION_BYTES).map(|(text, _)| text)
}

/// Read one source file: its text and the notes its decoding produced.
pub fn read_source(path: &Path, limit: usize) -> Option<(String, Vec<String>)> {
    if !path.is_file() {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let mut notes = Vec::new();
    let text = decode(&bytes, limit, &mut notes);
    Some((text, notes))
}

/// The limit a source of this kind is read with.
pub fn limit_for(kind: SourceKind) -> usize {
    match kind {
        SourceKind::Skill => MAX_SKILL_BYTES,
        SourceKind::Base | SourceKind::Instruction => MAX_INSTRUCTION_BYTES,
    }
}

/// The base prompt with the skills directories named in it.
///
/// The `{{skills_dirs}}` placeholder is where they go; a prompt without it gets
/// them appended, so a hand-written `prompt.md` can never lose the pull path to
/// the skills.
pub fn render_base(base: &str, places: &Places) -> String {
    let directories: Vec<String> = places
        .skills_directories()
        .iter()
        .map(|dir| format!("  - {}", crate::util::path_string(dir)))
        .collect();
    if base.contains(SKILLS_PLACEHOLDER) {
        return base.replace(SKILLS_PLACEHOLDER, &directories.join("\n"));
    }
    if directories.is_empty() {
        return base.to_string();
    }
    format!(
        "{}\n\n# Skills\n{}",
        base.trim_end(),
        directories.join("\n")
    )
}

/// Parse a `SKILL.md`: the Agent Skills front matter, and the body.
///
/// Front matter is a leading `---` fence with `key: value` lines (`name`,
/// `description`, optional `metadata`). A file without front matter, or without
/// a `name`, is not a skill: it is skipped with a reason, never rendered.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The `metadata` block as `key: value` lines, for a render or a hint.
    pub metadata: Option<String>,
    pub body: String,
}

pub fn parse_skill(text: &str) -> Result<Skill, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text
        .strip_prefix("---")
        .ok_or_else(|| "no `---` front matter".to_string())?;
    // Accept both `---\r\n` and `---\n` after the opening fence.
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .ok_or_else(|| "front matter: the opening `---` is not alone on its line".to_string())?;
    let Some(end) = find_fence(rest) else {
        return Err("front matter: no closing `---`".into());
    };
    let (front, after) = rest.split_at(end);
    let body = after
        .strip_prefix("---")
        .unwrap_or(after)
        .trim_start_matches(['\r', '\n'])
        .to_string();
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut metadata_lines: Vec<String> = Vec::new();
    let mut in_metadata = false;
    for line in front.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        if in_metadata && line.starts_with([' ', '\t']) {
            metadata_lines.push(line.trim().to_string());
            continue;
        }
        in_metadata = false;
        let Some((key, value)) = line.split_once(':') else {
            return Err(format!("front matter: `{line}` is not `key: value`"));
        };
        let key = key.trim();
        let value = unquote(value.trim());
        match key {
            "name" => name = Some(value),
            "description" => description = Some(value),
            "metadata" => {
                in_metadata = true;
                if !value.is_empty() {
                    metadata_lines.push(value);
                }
            }
            // Unknown keys are tolerated: the format allows more, and a skill
            // must not be dropped for one kelir does not read.
            _ => {}
        }
    }
    let name = name.unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return Err("front matter: no `name`".into());
    }
    Ok(Skill {
        name,
        description: description.unwrap_or_default().trim().to_string(),
        metadata: (!metadata_lines.is_empty()).then(|| metadata_lines.join("\n")),
        body,
    })
}

/// The byte offset of the closing `---` fence at the start of a line.
fn find_fence(text: &str) -> Option<usize> {
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return value[1..value.len() - 1].to_string();
    }
    value.to_string()
}

/// Everything the system prompt is rendered from.
pub struct Inputs<'a> {
    /// The config's base prompt (an explicit `prompt` file, the machine
    /// `prompt.md`, or the embedded default) and where it came from.
    pub base: &'a str,
    pub base_path: Option<&'a std::path::Path>,
    pub places: &'a Places,
    pub cwd: &'a std::path::Path,
    /// `YYYY-MM-DD`: the date goes in, never the time.
    pub date: &'a str,
}

/// Render the system prompt once: the base prompt with the skills directories
/// named in it, the instruction files, then the environment.
///
/// Same inputs, same bytes: the caller passes the date (never the clock), the
/// sources are sorted by the order [`discover`] finds them in, and nothing
/// depends on a hash map's iteration order.
pub fn render_system(inputs: &Inputs) -> Rendered {
    let mut warnings = Vec::new();
    // The base prompt: the project's `.kelir/prompt.md` wins over whatever the
    // config named (an explicit `prompt` file, the config dir's `prompt.md`, or
    // the embedded default), and each case records the path the text really
    // came from (F2).
    let project_base = inputs.cwd.join(".kelir").join("prompt.md");
    let (base_text, base_path) = match read_source(&project_base, MAX_INSTRUCTION_BYTES) {
        Some((text, notes)) => {
            warnings.extend(notes);
            (text, SourcePath::File(project_base))
        }
        None => {
            let (text, notes) = match inputs.base_path {
                Some(path) => match read_source(path, MAX_INSTRUCTION_BYTES) {
                    Some((text, notes)) => (text, notes),
                    None => (strip_bom(inputs.base), Vec::new()),
                },
                None => (strip_bom(inputs.base), Vec::new()),
            };
            warnings.extend(notes);
            let path = match inputs.base_path {
                Some(path) => SourcePath::File(path.to_path_buf()),
                None => SourcePath::Embedded,
            };
            (text, path)
        }
    };
    let base = render_base(&base_text, inputs.places);
    let discovered = discover_with_notes(inputs.places, &mut warnings);
    let instructions: Vec<&Found> = discovered
        .iter()
        .filter(|found| found.kind == SourceKind::Instruction)
        .collect();

    let mut text = base.trim_end().to_string();
    if !instructions.is_empty() {
        text.push_str("\n\n# Instructions\n");
        for found in instructions.iter() {
            text.push_str(&format!(
                "\n## {} — {}\n\n{}\n",
                found.name,
                crate::util::path_string(&found.path),
                found.text.trim_end()
            ));
        }
    }
    text.push_str(&format!(
        "\n# Environment\ncwd: {}\nos: {}\ndate: {}\n",
        crate::util::path_string(inputs.cwd),
        std::env::consts::OS,
        inputs.date
    ));

    let mut sources = vec![info("prompt.md", &base_path, SourceKind::Base, &base_text)];
    for found in &instructions {
        sources.push(info(
            &found.name,
            &SourcePath::File(found.path.clone()),
            found.kind,
            &found.text,
        ));
    }
    // Skills are pull: their bodies stay out of the prompt, but they are
    // recorded (and watched) so a skill edited mid-session arrives as a
    // `source` message like any other file.
    let mut skills: Vec<&Found> = discovered
        .iter()
        .filter(|found| found.kind == SourceKind::Skill)
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    for found in skills {
        sources.push(info(
            &found.name,
            &SourcePath::File(found.path.clone()),
            found.kind,
            &found.text,
        ));
    }
    Rendered {
        hash: hash_of(&text),
        text,
        sources,
        warnings,
    }
}

fn info(name: &str, path: &SourcePath, kind: SourceKind, text: &str) -> SourceInfo {
    SourceInfo {
        name: name.to_string(),
        path: path.log_path(),
        kind: Some(kind.as_str().to_string()),
        hash: hash_of(text),
    }
}

/// A base prompt read from the config keeps its bytes; only the BOM goes, so an
/// embedded default is never changes by a stray marker.
fn strip_bom(text: &str) -> String {
    text.strip_prefix('\u{feff}').unwrap_or(text).to_string()
}

pub fn hash_of(text: &str) -> String {
    crate::util::fnv1a64_hex(text.as_bytes())
}

/// The instruction files only (what the system prompt renders).
pub fn instructions(places: &Places) -> Vec<Found> {
    discover(places)
        .into_iter()
        .filter(|found| found.kind == SourceKind::Instruction)
        .collect()
}

/// The kind a `sources[]` entry stands for.
///
/// The log records the kind (`prompt`, `instruction`, `skill`) where the source
/// was found, so a skill directory called `notes.md` stays a skill (F9). An
/// older log has no kind and is read back from the name.
pub fn kind_of(source: &SourceInfo) -> SourceKind {
    match source.kind.as_deref() {
        Some(kind) => SourceKind::parse(kind),
        None => match source.name.as_str() {
            "prompt.md" => SourceKind::Base,
            "AGENTS.md" | "CLAUDE.md" => SourceKind::Instruction,
            _ => SourceKind::Skill,
        },
    }
}

/// One source whose text is in this context, as the watcher re-checks it.
#[derive(Debug, Clone, PartialEq)]
pub struct Tracked {
    pub kind: SourceKind,
    pub name: String,
    pub path: SourcePath,
    /// The hash of the text this context **currently** carries for the source:
    /// the frozen system's, or the newest `source` message's. Comparing against
    /// this one hash (not every hash ever carried) is what makes A→B→A a
    /// change: the model's latest word on the file is B (F1).
    pub hash: String,
    /// A compaction folded the source's message away: the next boundary says
    /// what the file holds now, even if it holds the same bytes.
    pub stale: bool,
    /// The file was already reported as gone, so a missing source is one note,
    /// not one per boundary (N4).
    pub gone: bool,
}

/// A source whose file was deleted: the context still carries its last text,
/// which is now nothing on disk. Reported once, so a reader knows why the model
/// still believes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Removal {
    pub kind: SourceKind,
    pub name: String,
    pub path: PathBuf,
}

/// A source edited on disk since it went into the context.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub kind: SourceKind,
    pub name: String,
    pub path: PathBuf,
    pub text: String,
    /// What the re-read had to say beyond the text (a truncation, invalid
    /// UTF-8): recorded next to the `source` event, never dropped silently.
    pub notes: Vec<String>,
}

/// The per-boundary mtime/hash check of the recorded sources.
///
/// A watcher (a thread, an OS notification, a handle to close) is one more
/// thing to leak; the recorded sources are a handful of small files, so each
/// boundary re-reads them and compares a content hash. That is also why the
/// check is exact: an mtime that changed but a body that did not is not news.
#[derive(Debug, Clone, Default)]
pub struct Watch {
    tracked: Vec<Tracked>,
}

impl Watch {
    /// Track the sources the system prompt was rendered from.
    pub fn from_rendered(rendered: &Rendered) -> Watch {
        Watch::from_sources(&rendered.sources)
    }

    /// Track the sources `session_started` recorded.
    pub fn from_sources(sources: &[SourceInfo]) -> Watch {
        let mut watch = Watch::default();
        for source in sources {
            let kind = kind_of(source);
            watch.add(
                kind,
                &source.name,
                &SourcePath::from_log(kind, &source.path),
                &source.hash,
            );
        }
        watch
    }

    /// Rebuild the watcher from a log: the frozen `sources[]` first, then every
    /// `source` event in order, so the context's *current* word on each source
    /// is the last message it carries (F8).
    ///
    /// A resume therefore does not announce an edit its own log already has,
    /// does keep watching a source the `skills` verb added (its path is only in
    /// the event), and does announce a source whose message a compaction folded
    /// away ([`Watch::forget`] is called for those).
    pub fn rebuild(events: &[LogEvent], dir: &Path) -> Watch {
        let mut watch = Watch::from_sources(&sources_of(events));
        for event in events {
            let crate::event::EventBody::Source {
                kind,
                name,
                text,
                path,
            } = &event.body
            else {
                continue;
            };
            let kind = SourceKind::parse(kind);
            let Some(text) = text else {
                continue;
            };
            let text = text.resolve_text(dir).unwrap_or_default();
            let path = match path.as_deref() {
                Some(path) => SourcePath::from_log(kind, path),
                None => SourcePath::Embedded,
            };
            watch.add(kind, name, &path, &hash_of(&text));
        }
        // A `source` event a standing compaction folds into its summary is not
        // in the context any more: the resume says what the file holds at its
        // first boundary, exactly as a live session would after the same
        // compaction.
        for event in events {
            if let crate::event::EventBody::Compacted {
                through_seq,
                pinned_seqs,
                ..
            } = &event.body
            {
                watch.forget_folded(events, *through_seq, pinned_seqs);
            }
        }
        watch
    }

    /// Track one source as the context's current word on it.
    pub fn add(&mut self, kind: SourceKind, name: &str, path: &SourcePath, hash: &str) {
        if let Some(existing) = self
            .tracked
            .iter_mut()
            .find(|tracked| tracked.kind == kind && tracked.name == name)
        {
            existing.path = path.clone();
            existing.hash = hash.to_string();
            existing.stale = false;
            existing.gone = false;
            return;
        }
        self.tracked.push(Tracked {
            kind,
            name: name.to_string(),
            path: path.clone(),
            hash: hash.to_string(),
            stale: false,
            gone: false,
        });
    }

    /// The context no longer carries this source's text: announce the file as
    /// it is at the next boundary, even though its bytes did not change.
    pub fn forget(&mut self, name: &str) {
        for tracked in &mut self.tracked {
            if tracked.name == name {
                tracked.stale = true;
            }
        }
    }

    /// A compaction folded every message up to `through_seq` into a summary, so
    /// a `source` event at or below it (rendered from a pinned user message
    /// excepted) is no longer in the context. `pinned` are the seqs the fold
    /// still renders verbatim.
    pub fn forget_folded(&mut self, events: &[LogEvent], through_seq: u64, pinned: &[u64]) {
        for event in events {
            if event.seq > through_seq || pinned.contains(&event.seq) {
                continue;
            }
            if let crate::event::EventBody::Source { name, .. } = &event.body {
                self.forget(name);
            }
        }
    }

    pub fn tracked(&self) -> &[Tracked] {
        &self.tracked
    }

    /// Every tracked source whose file has disappeared since the context last
    /// heard about it, once per disappearance (N4).
    ///
    /// A deleted source is not a `source` event — there is no text to send —
    /// but it is not silence either: the model still holds the last text of a
    /// file that no longer exists, so the log says so.
    pub fn removals(&mut self) -> Vec<Removal> {
        let mut out = Vec::new();
        for tracked in &mut self.tracked {
            if tracked.gone {
                continue;
            }
            let Some(path) = tracked.path.file() else {
                continue;
            };
            if path.is_file() {
                continue;
            }
            tracked.gone = true;
            out.push(Removal {
                kind: tracked.kind,
                name: tracked.name.clone(),
                path: path.to_path_buf(),
            });
        }
        out
    }

    /// Every source whose current content this context does not carry yet. The
    /// hash is replaced as it is returned, so one edit is one message — and a
    /// file edited back to an older body is a message again, because the
    /// model's latest word on it is the one before.
    pub fn changes(&mut self) -> Vec<Change> {
        let mut out = Vec::new();
        for tracked in &mut self.tracked {
            let Some(path) = tracked.path.file().map(Path::to_path_buf) else {
                continue;
            };
            let Some((text, notes)) = read_source(&path, limit_for(tracked.kind)) else {
                continue;
            };
            let hash = hash_of(&text);
            tracked.gone = false;
            if !tracked.stale && tracked.hash == hash {
                continue;
            }
            tracked.hash = hash;
            tracked.stale = false;
            out.push(Change {
                kind: tracked.kind,
                name: tracked.name.clone(),
                path,
                text,
                notes,
            });
        }
        out
    }
}

/// The `sources[]` of the log's `session_started`.
pub fn sources_of(events: &[LogEvent]) -> Vec<SourceInfo> {
    events
        .iter()
        .find_map(|event| match &event.body {
            crate::event::EventBody::SessionStarted { sources, .. } => Some(sources.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kelir-sources-{}-{name}", ulid::Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    fn places_in(root: &Path) -> Places {
        Places {
            config_dir: Some(root.join("config")),
            machine_claude: Some(root.join("machine-claude")),
            skills_dirs: vec![root.join("extra-skills")],
            cwd: root.join("work").join("deep"),
            home: None,
            repo_root: None,
        }
    }

    const SKILL: &str = "---\nname: sift\n\
description: use when a log is too long to read\n\
metadata:\n  owner: gates\n---\n\n# Sift\n\nRead the tail first.\n";

    #[test]
    fn the_embedded_prompt_fits_the_budget_and_names_the_skills_dirs() {
        let dir = temp_dir("budget");
        let places = places_in(&dir);
        let base = crate::config::DEFAULT_PROMPT;
        assert!(
            base.len() <= 1024,
            "the base prompt is {} bytes",
            base.len()
        );
        let rendered = render_base(base, &places);
        assert!(
            !rendered.contains(SKILLS_PLACEHOLDER),
            "the placeholder is replaced"
        );
        for path in places.skills_directories() {
            assert!(
                rendered.contains(&crate::util::path_string(&path)),
                "{} is named in:\n{rendered}",
                path.display()
            );
        }
    }

    #[test]
    fn a_prompt_without_the_placeholder_gets_the_dirs_appended() {
        let dir = temp_dir("append");
        let places = places_in(&dir);
        let rendered = render_base("BASE", &places);
        assert!(rendered.starts_with("BASE\n\n# Skills\n"), "{rendered}");
        assert!(rendered.contains(".claude/skills"), "{rendered}");
    }

    #[test]
    fn a_machine_instruction_file_of_another_name_is_kept() {
        let dir = temp_dir("machine-keep");
        let places = places_in(&dir);
        write(&dir.join("config").join("CLAUDE.md"), "MACHINE CLAUDE");
        write(
            &dir.join("work").join("deep").join("AGENTS.md"),
            "PROJECT AGENTS",
        );
        let found = discover(&places);
        let texts: Vec<&str> = found
            .iter()
            .filter(|item| item.kind == SourceKind::Instruction)
            .map(|item| item.text.as_str())
            .collect();
        assert_eq!(texts, vec!["MACHINE CLAUDE", "PROJECT AGENTS"]);
    }

    #[test]
    fn discovery_is_project_over_machine_and_nearest_first() {
        let dir = temp_dir("discover");
        let places = places_in(&dir);
        // Machine layer: one instruction file and one skill.
        write(&dir.join("config").join("AGENTS.md"), "MACHINE AGENTS");
        write(
            &dir.join("machine-claude")
                .join("skills")
                .join("mine")
                .join("SKILL.md"),
            "---\nname: mine\ndescription: machine\n---\nMACHINE BODY",
        );
        // Project layer: the same skill name (project wins) and two
        // instruction files, one in the cwd and one two levels up.
        write(
            &dir.join("work").join("deep").join("AGENTS.md"),
            "NEAR TASK",
        );
        write(&dir.join("work").join("CLAUDE.md"), "FAR CLAUDE");
        write(
            &dir.join("work")
                .join("deep")
                .join(".kelir")
                .join("skills")
                .join("mine")
                .join("SKILL.md"),
            "---\nname: mine\ndescription: project\n---\nPROJECT BODY",
        );
        let found = discover(&places);
        let instructions: Vec<&Found> = found
            .iter()
            .filter(|item| item.kind == SourceKind::Instruction)
            .collect();
        assert_eq!(instructions.len(), 2, "{instructions:?}");
        // The project's AGENTS.md wins over the machine one of the same name,
        // and it is the *nearest* one (there is none in `work` here).
        assert_eq!(instructions[0].name, "AGENTS.md");
        assert_eq!(instructions[0].text, "NEAR TASK");
        assert_eq!(instructions[0].layer, Layer::Project);
        // The file further up renders after it: nearest first.
        assert_eq!(instructions[1].name, "CLAUDE.md");
        assert_eq!(instructions[1].text, "FAR CLAUDE");
        // One skill survives the clash, and it is the project's.
        let skills: Vec<&Found> = found
            .iter()
            .filter(|item| item.kind == SourceKind::Skill)
            .collect();
        assert_eq!(skills.len(), 1, "{skills:?}");
        // The watcher hashes the file it read, front matter included.
        assert!(skills[0].text.ends_with("PROJECT BODY"), "{skills:?}");
        assert_eq!(parse_skill(&skills[0].text).unwrap().name, "mine");
        assert_eq!(skills[0].layer, Layer::Project);
    }

    #[test]
    fn every_ancestors_instruction_file_is_collected_nearest_first() {
        let dir = temp_dir("nearest");
        let mut places = places_in(&dir);
        places.cwd = dir.join("work").join("deep").join("deeper");
        write(&dir.join("work").join("deep").join("AGENTS.md"), "DEEP");
        write(&dir.join("work").join("AGENTS.md"), "SHALLOW");
        let found = discover(&places);
        let instructions: Vec<&Found> = found
            .iter()
            .filter(|item| item.kind == SourceKind::Instruction)
            .collect();
        // Both files are kept (F4), nearest first: a monorepo's root rules and
        // a subdirectory's rules are both the model's business.
        assert_eq!(instructions.len(), 2, "{instructions:?}");
        assert_eq!(instructions[0].text, "DEEP");
        assert_eq!(instructions[1].text, "SHALLOW");
    }

    #[test]
    fn the_walk_stops_at_the_repo_root_and_at_home() {
        let dir = temp_dir("stop");
        let mut places = places_in(&dir);
        write(&dir.join("AGENTS.md"), "ABOVE THE REPO");
        write(&dir.join("repo").join(".git").join("HEAD"), "ref");
        write(&dir.join("repo").join("AGENTS.md"), "REPO ROOT");
        write(
            &dir.join("repo").join("sub").join("AGENTS.md"),
            "IN THE SUB",
        );
        places.cwd = dir.join("repo").join("sub");
        places.repo_root = Some(dir.join("repo"));
        let texts: Vec<String> = discover(&places)
            .into_iter()
            .filter(|item| item.kind == SourceKind::Instruction)
            .map(|item| item.text)
            .collect();
        assert_eq!(texts, vec!["IN THE SUB", "REPO ROOT"], "{texts:?}");

        // The same walk without a repository stops at home.
        let mut places = places_in(&dir);
        places.cwd = dir.join("home").join("work");
        places.home = Some(dir.join("home"));
        places.repo_root = None;
        write(&dir.join("home").join("work").join("AGENTS.md"), "NEAR");
        write(&dir.join("home").join("AGENTS.md"), "HOME");
        write(&dir.join("AGENTS.md"), "ABOVE HOME");
        let texts: Vec<String> = discover(&places)
            .into_iter()
            .filter(|item| item.kind == SourceKind::Instruction)
            .map(|item| item.text)
            .collect();
        assert_eq!(texts, vec!["NEAR", "HOME"], "{texts:?}");
    }

    #[test]
    fn missing_directories_are_tolerated() {
        let dir = temp_dir("missing");
        let places = Places {
            config_dir: Some(dir.join("nope")),
            machine_claude: Some(dir.join("nope-claude")),
            skills_dirs: vec![dir.join("nope-skills")],
            cwd: dir.join("nope-work"),
            home: None,
            repo_root: None,
        };
        assert!(discover(&places).is_empty());
        let rendered = render_system(&Inputs {
            base: "BASE",
            base_path: None,
            places: &places,
            cwd: &places.cwd,
            date: "2026-11-19",
        });
        assert!(rendered.text.starts_with("BASE"));
    }

    #[test]
    fn the_decode_rule_drops_the_bom_keeps_a_lossy_read_and_truncates_visibly() {
        let mut notes = Vec::new();
        // A UTF-8 BOM is dropped (N6), and CRLF is kept as it is.
        assert_eq!(decode(b"\xef\xbb\xbfA\r\nB", 4096, &mut notes), "A\r\nB");
        assert!(notes.is_empty(), "{notes:?}");
        // Invalid UTF-8 is decoded lossily, and said so (F5).
        notes.clear();
        let text = decode(b"caf\xe9 rules", 4096, &mut notes);
        assert!(text.starts_with("caf"), "{text:?}");
        assert!(text.contains('\u{fffd}'), "{text:?}");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("not valid UTF-8"), "{notes:?}");
        // Over the cap: cut on a char boundary, with a visible mark (F7), and
        // the mark says how much there was.
        notes.clear();
        let text = decode("ééééé".as_bytes(), 5, &mut notes);
        assert!(text.starts_with("éé"), "{text:?}");
        assert!(text.contains("[kelir: truncated"), "{text:?}");
        assert!(text.contains("10 bytes on disk"), "{text:?}");
        assert_eq!(notes.len(), 1, "{notes:?}");
    }

    #[test]
    fn a_huge_instruction_file_is_capped_and_the_watcher_agrees_with_the_render() {
        let dir = temp_dir("cap");
        let places = places_in(&dir);
        let big = "x".repeat(MAX_INSTRUCTION_BYTES * 3);
        write(&places.cwd.join("AGENTS.md"), &big);
        let mut notes = Vec::new();
        let found = discover_with_notes(&places, &mut notes);
        let agents = found
            .iter()
            .find(|item| item.name == "AGENTS.md")
            .expect("AGENTS.md");
        assert!(
            agents.text.len() < MAX_INSTRUCTION_BYTES + 200,
            "{} bytes",
            agents.text.len()
        );
        assert!(agents.text.contains("[kelir: truncated"), "{notes:?}");
        assert!(
            notes.iter().any(|note| note.contains("AGENTS.md")),
            "{notes:?}"
        );
        // Every file is read the same way the watcher re-reads it, so a source
        // is not announced again at the first boundary.
        let rendered = render_system(&Inputs {
            base: "BASE",
            base_path: None,
            places: &places,
            cwd: &places.cwd,
            date: "2026-11-19",
        });
        let mut watch = Watch::from_rendered(&rendered);
        assert!(watch.changes().is_empty(), "{:?}", watch.changes());
    }

    #[test]
    fn a_malformed_skill_is_skipped_with_a_reason() {
        let dir = temp_dir("bad-skill");
        write(&dir.join("good").join("SKILL.md"), SKILL);
        write(&dir.join("nofm").join("SKILL.md"), "just prose");
        write(
            &dir.join("noname").join("SKILL.md"),
            "---\ndescription: d\n---\nbody",
        );
        let mut notes = Vec::new();
        let found = skills_in(&dir, &mut notes);
        let names: Vec<&str> = found.iter().map(|skill| skill.name.as_str()).collect();
        assert_eq!(names, vec!["good"], "{found:?}");
        assert_eq!(notes.len(), 2, "{notes:?}");
        for note in &notes {
            assert!(note.contains("is not a skill"), "{note}");
        }
    }

    #[test]
    fn skills_parse_front_matter_and_metadata() {
        let skill = parse_skill(SKILL).unwrap();
        assert_eq!(skill.name, "sift");
        assert_eq!(skill.description, "use when a log is too long to read");
        assert_eq!(skill.metadata.as_deref(), Some("owner: gates"));
        assert_eq!(skill.body, "# Sift\n\nRead the tail first.\n");
        // CRLF front matter parses the same way.
        let crlf = SKILL.replace('\n', "\r\n");
        assert_eq!(parse_skill(&crlf).unwrap().name, "sift");
        // Quoted values are unquoted; a BOM is tolerated.
        let quoted = "\u{feff}---\nname: \"my skill\"\ndescription: 'when x'\n---\nbody";
        let skill = parse_skill(quoted).unwrap();
        assert_eq!(skill.name, "my skill");
        assert_eq!(skill.description, "when x");
        assert_eq!(skill.body, "body");
        // Unknown keys are tolerated.
        let extra = "---\nname: a\nlicense: MIT\nallowed-tools: bash\n---\nb";
        assert_eq!(parse_skill(extra).unwrap().name, "a");
    }

    #[test]
    fn malformed_skills_are_refused_with_a_reason() {
        let error = parse_skill("just a body").unwrap_err();
        assert!(error.contains("front matter"), "{error}");
        let error = parse_skill("---\nname: a\n").unwrap_err();
        assert!(error.contains("closing"), "{error}");
        let error = parse_skill("---\ndescription: no name\n---\nb").unwrap_err();
        assert!(error.contains("no `name`"), "{error}");
        let error = parse_skill("---\nname:\n---\nb").unwrap_err();
        assert!(error.contains("no `name`"), "{error}");
        let error = parse_skill("---\nname a\n---\nb").unwrap_err();
        assert!(error.contains("key: value"), "{error}");
        assert!(parse_skill("---").is_err());
    }

    #[test]
    fn the_render_is_byte_identical_for_the_same_inputs() {
        let dir = temp_dir("determinism");
        let places = places_in(&dir);
        write(&dir.join("work").join("deep").join("AGENTS.md"), "TASK");
        write(
            &dir.join("machine-claude")
                .join("skills")
                .join("b")
                .join("SKILL.md"),
            "---\nname: b\ndescription: bee\n---\nb",
        );
        write(
            &dir.join("machine-claude")
                .join("skills")
                .join("a")
                .join("SKILL.md"),
            "---\nname: a\ndescription: ay\n---\na",
        );
        let inputs = Inputs {
            base: crate::config::DEFAULT_PROMPT,
            base_path: None,
            places: &places,
            cwd: &places.cwd,
            date: "2026-11-19",
        };
        let first = render_system(&inputs);
        let second = render_system(&inputs);
        assert_eq!(first.text, second.text);
        assert_eq!(first.hash, second.hash);
        assert_eq!(first.hash, crate::util::fnv1a64_hex(first.text.as_bytes()));
        // The date goes in, never the time.
        assert!(first.text.contains("date: 2026-11-19"));
        assert_eq!(first.text.matches("date:").count(), 1);
    }

    #[test]
    fn the_system_carries_the_instruction_text_and_the_sources() {
        let dir = temp_dir("system");
        let places = places_in(&dir);
        write(
            &dir.join("work").join("deep").join("AGENTS.md"),
            "ALWAYS RUN THE GATES",
        );
        let inputs = Inputs {
            base: "BASE {{skills_dirs}}",
            base_path: None,
            places: &places,
            cwd: &places.cwd,
            date: "2026-11-19",
        };
        let rendered = render_system(&inputs);
        assert!(
            rendered.text.contains("ALWAYS RUN THE GATES"),
            "{}",
            rendered.text
        );
        assert!(
            rendered.text.contains("# Instructions"),
            "{}",
            rendered.text
        );
        assert!(rendered.text.contains("cwd: "), "{}", rendered.text);
        let names: Vec<&str> = rendered.sources.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["prompt.md", "AGENTS.md"]);
        // The embedded default records an empty path and its kind, never a
        // sentinel a reader could mistake for a file (F2, F2b).
        assert_eq!(rendered.sources[0].path, "");
        assert_eq!(rendered.sources[0].kind.as_deref(), Some("prompt"));
        assert!(rendered.sources[1].path.ends_with("AGENTS.md"));
        assert_eq!(rendered.sources[1].kind.as_deref(), Some("instruction"));
        assert_eq!(rendered.sources[1].hash.len(), 16);
    }

    #[test]
    fn the_project_prompt_wins_over_the_machine_one() {
        let dir = temp_dir("prompt-layer");
        let places = places_in(&dir);
        let machine = dir.join("config").join("prompt.md");
        write(&machine, "MACHINE PROMPT");
        let inputs = Inputs {
            base: "MACHINE PROMPT",
            base_path: Some(&machine),
            places: &places,
            cwd: &places.cwd,
            date: "2026-11-19",
        };
        assert!(render_system(&inputs).text.starts_with("MACHINE PROMPT"));
        write(
            &places.cwd.join(".kelir").join("prompt.md"),
            "PROJECT PROMPT",
        );
        let rendered = render_system(&inputs);
        assert!(
            rendered.text.starts_with("PROJECT PROMPT"),
            "{}",
            rendered.text
        );
        assert!(rendered.sources[0].path.ends_with("prompt.md"));
    }

    #[test]
    fn an_edit_is_one_change_and_editing_back_is_a_change_again() {
        let dir = temp_dir("watch");
        let places = places_in(&dir);
        let file = places.cwd.join("AGENTS.md");
        write(&file, "ONE");
        let rendered = render_system(&Inputs {
            base: "BASE",
            base_path: None,
            places: &places,
            cwd: &places.cwd,
            date: "2026-11-19",
        });
        let mut watch = Watch::from_rendered(&rendered);
        // Nothing changed yet.
        assert!(watch.changes().is_empty());
        // One edit is exactly one change, and it is not repeated.
        write(&file, "TWO");
        let changes = watch.changes();
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].text, "TWO");
        assert_eq!(changes[0].kind, SourceKind::Instruction);
        assert!(watch.changes().is_empty(), "one edit, one message");
        // Editing it back to the text the system prompt rendered is a change
        // again: the model's latest word on the file is TWO, and the file on
        // disk says ONE (F1).
        write(&file, "ONE");
        let changes = watch.changes();
        assert_eq!(changes.len(), 1, "A->B->A is announced as A again");
        assert_eq!(changes[0].text, "ONE");
        // A third body is news too.
        write(&file, "THREE");
        assert_eq!(watch.changes().len(), 1);
    }

    #[test]
    fn a_compaction_makes_the_next_boundary_say_the_file_again() {
        let dir = temp_dir("compacted");
        let places = places_in(&dir);
        let file = places.cwd.join("AGENTS.md");
        write(&file, "SAME");
        let rendered = render_system(&Inputs {
            base: "BASE",
            base_path: None,
            places: &places,
            cwd: &places.cwd,
            date: "2026-11-19",
        });
        let mut watch = Watch::from_rendered(&rendered);
        watch.forget("AGENTS.md");
        let changes = watch.changes();
        assert_eq!(changes.len(), 1, "the folded source is announced again");
        assert_eq!(changes[0].text, "SAME");
        assert!(watch.changes().is_empty(), "and only once");
    }

    #[test]
    fn a_verb_source_lands_at_the_tail_and_keeps_being_tracked() {
        let dir = temp_dir("verb");
        let file = dir.join("extra.md");
        write(&file, "VERB BODY");
        let mut watch = Watch::default();
        watch.add(
            SourceKind::Instruction,
            "extra.md",
            &SourcePath::File(file.clone()),
            &hash_of("VERB BODY"),
        );
        assert!(watch.changes().is_empty());
        // A file that arrived by verb is tracked like any other.
        write(&file, "NEWER");
        assert_eq!(watch.changes().len(), 1);
        // A tracked source whose file vanished is not a change.
        std::fs::remove_file(&file).unwrap();
        assert!(watch.changes().is_empty());
    }

    #[test]
    fn only_a_skill_directory_with_a_skill_md_is_a_skill() {
        let dir = temp_dir("skills");
        write(
            &dir.join("one").join("SKILL.md"),
            "---\nname: one\ndescription: d\n---\nbody",
        );
        write(&dir.join("two").join("README.md"), "not a skill");
        write(&dir.join(".git").join("SKILL.md"), "---\nname: git\n---\n");
        let mut notes = Vec::new();
        let found = skills_in(&dir, &mut notes);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].name, "one");
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// The `skills` verb's two shapes, at the level the session uses them: a
    /// skill directory is read as its `SKILL.md` body, and an instruction file
    /// as its whole text.
    #[test]
    fn a_skill_directory_reads_as_its_body() {
        let dir = temp_dir("skill-read");
        write(
            &dir.join("sift").join("SKILL.md"),
            "---
name: sift
description: d
---

READ THE TAIL
",
        );
        let text = read_text(&dir.join("sift").join("SKILL.md")).unwrap();
        let skill = parse_skill(&text).unwrap();
        assert_eq!(skill.name, "sift");
        assert_eq!(
            skill.body,
            "READ THE TAIL
"
        );
        // A directory without a SKILL.md is not a skill source.
        std::fs::create_dir_all(dir.join("empty")).unwrap();
        assert!(read_text(&dir.join("empty").join("SKILL.md")).is_none());
    }

    #[test]
    fn a_sources_kind_comes_from_the_log_and_never_from_its_name() {
        let info = |name: &str, kind: Option<&str>| SourceInfo {
            name: name.to_string(),
            path: String::new(),
            kind: kind.map(str::to_string),
            hash: "h".into(),
        };
        assert_eq!(
            kind_of(&info("prompt.md", Some("prompt"))),
            SourceKind::Base
        );
        assert_eq!(
            kind_of(&info("AGENTS.md", Some("instruction"))),
            SourceKind::Instruction
        );
        // A skill directory may legally be called `notes.md`: the recorded kind
        // wins over the name (F9).
        assert_eq!(kind_of(&info("notes.md", Some("skill"))), SourceKind::Skill);
        // An older log has no kind: the name is the fallback.
        assert_eq!(kind_of(&info("prompt.md", None)), SourceKind::Base);
        assert_eq!(kind_of(&info("AGENTS.md", None)), SourceKind::Instruction);
        assert_eq!(kind_of(&info("CLAUDE.md", None)), SourceKind::Instruction);
        assert_eq!(kind_of(&info("sift", None)), SourceKind::Skill);
        assert_eq!(SourceKind::Instruction.as_str(), "instruction");
    }

    #[test]
    fn the_embedded_base_prompt_has_no_path_to_read() {
        let dir = temp_dir("embedded");
        let places = places_in(&dir);
        let rendered = render_system(&Inputs {
            base: "BASE",
            base_path: None,
            places: &places,
            cwd: &places.cwd,
            date: "2026-11-19",
        });
        let mut watch = Watch::from_rendered(&rendered);
        // A file in the process cwd called `embedded` is nobody's source.
        write(&PathBuf::from("embedded"), "NOT THE BASE PROMPT");
        assert!(watch.changes().is_empty(), "{:?}", watch.tracked());
        let _ = std::fs::remove_file("embedded");
        assert_eq!(rendered.sources[0].path, "");
        assert_eq!(
            SourcePath::from_log(SourceKind::Base, ""),
            SourcePath::Embedded
        );
        // A path that happens to be the old sentinel is only the embedded
        // default for the base prompt itself.
        assert_eq!(
            SourcePath::from_log(SourceKind::Base, "embedded"),
            SourcePath::Embedded
        );
        assert_eq!(
            SourcePath::from_log(SourceKind::Skill, "embedded"),
            SourcePath::File(PathBuf::from("embedded"))
        );
    }
}
