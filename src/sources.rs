//! Sources: the base prompt, instruction files and skills directories.
//!
//! The system prompt is rendered **once** at `session_started` and then frozen:
//! a source edited on disk mid-session arrives as a `source` event (a message
//! at the tail), never as a rewrite of the prefix, because prompt caching is
//! the point of rendering once.
//!
//! Two layers, machine then project, and the project wins on a name clash:
//!
//! * the base prompt: the embedded default (at most 1 KB), overridden by
//!   `<config>/prompt.md` and by `<cwd>/.kelir/prompt.md`;
//! * instruction files: `AGENTS.md` and `CLAUDE.md`, found walking up from the
//!   session cwd (nearest first), in the machine config dir too;
//! * skills directories: the config's `skills_dirs`, `<config>/skills`,
//!   `~/.claude/skills` for the machine layer, and `.claude/skills`,
//!   `.kelir/skills` for the project one, read-only.
//!
//! Skills are **pull**: the base prompt names the directories and nothing else.
//! A skill is a directory holding a `SKILL.md` with Agent Skills front matter
//! (`name`, `description`, optional `metadata`); reading it is the model's job
//! (with `bash`), so its body never costs a token until the task needs it.

use std::path::{Path, PathBuf};

use crate::event::SourceInfo;

/// The placeholder in the base prompt where the skills directories go.
pub const SKILLS_PLACEHOLDER: &str = "{{skills_dirs}}";

/// The rendered system prompt: the text, its hash, and everything that went in.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub text: String,
    pub hash: String,
    pub sources: Vec<SourceInfo>,
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

    /// The `source` event's kind, read back.
    pub fn from_event_kind(kind: &str) -> SourceKind {
        match kind {
            "skill" => SourceKind::Skill,
            _ => SourceKind::Instruction,
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
}

/// The paths this process looks in, built from the config and the environment.
pub fn places(cwd: &Path, skills_dirs: &[PathBuf], config_dir: Option<PathBuf>) -> Places {
    Places {
        config_dir,
        machine_claude: Some(crate::paths::home().join(".claude")),
        skills_dirs: skills_dirs.to_vec(),
        cwd: cwd.to_path_buf(),
    }
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

/// Discover every machine and project source for `places`.
///
/// The order is the render order: machine layer, then project layer (nearest
/// instruction file first). A later source of the same name replaces an earlier
/// one: the project wins on a name clash.
pub fn discover(places: &Places) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    // Machine instruction files: the config dir's own AGENTS.md/CLAUDE.md.
    if let Some(config) = &places.config_dir {
        for name in INSTRUCTION_FILES {
            let path = config.join(name);
            if let Some(text) = read(&path) {
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
    }
    // Project instruction files: walking up from the cwd, nearest first. A
    // missing directory contributes nothing, and the walk ends at the root.
    let mut dir = Some(places.cwd.clone());
    while let Some(current) = dir {
        for name in INSTRUCTION_FILES {
            let path = current.join(name);
            if let Some(text) = read(&path) {
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
        dir = current.parent().map(Path::to_path_buf);
    }
    // Skills directories, machine layer then project. A skill's name is its
    // directory's name; a project skill of that name wins.
    for directory in places.skills_directories() {
        let layer = places.skills_layer(&directory);
        for (name, path, text) in skills_in(&directory) {
            push_source(&mut found, SourceKind::Skill, &name, path, text, layer);
        }
    }
    found
}

/// Add a source, resolving a name clash the way the two layers demand: a
/// project source wins over a machine one, and within a layer the first found
/// (the nearest instruction file) wins.
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
    match found
        .iter_mut()
        .find(|other| other.kind == kind && other.name == item.name)
    {
        Some(existing) if layer == Layer::Project && existing.layer == Layer::Machine => {
            *existing = item;
        }
        Some(_) => {}
        None => found.push(item),
    }
}

/// Every skill in one directory, sorted by name so the render is stable.
fn skills_in(directory: &Path) -> Vec<(String, PathBuf, String)> {
    let mut out: Vec<(String, PathBuf, String)> = Vec::new();
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
        let Some(text) = read(&file) else {
            continue;
        };
        out.push((name.to_string(), file, text));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn read(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    std::fs::read_to_string(path).ok()
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
    // The base prompt: the project's `.kelir/prompt.md` wins over the machine's
    // (an explicit `prompt` file, the config dir's `prompt.md`, or the embedded
    // default).
    let project_base = inputs.cwd.join(".kelir").join("prompt.md");
    let (base_text, base_path) = match read(&project_base) {
        Some(text) => (text, Some(project_base)),
        None => (
            inputs.base.to_string(),
            inputs.base_path.map(Path::to_path_buf),
        ),
    };
    let base = render_base(&base_text, inputs.places);
    let discovered = discover(inputs.places);
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

    let mut sources = vec![SourceInfo {
        name: "prompt.md".to_string(),
        path: match &base_path {
            Some(path) => crate::util::path_string(path),
            None => EMBEDDED.to_string(),
        },
        hash: crate::util::fnv1a64_hex(base_text.as_bytes()),
    }];
    for found in &instructions {
        sources.push(SourceInfo {
            name: found.name.clone(),
            path: crate::util::path_string(&found.path),
            hash: hash_of(&found.text),
        });
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
        sources.push(SourceInfo {
            name: found.name.clone(),
            path: crate::util::path_string(&found.path),
            hash: hash_of(&found.text),
        });
    }
    Rendered {
        hash: hash_of(&text),
        text,
        sources,
    }
}

/// The `sources[]` path of the embedded base prompt.
pub const EMBEDDED: &str = "embedded";

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

/// The kind a `sources[]` entry stands for, from its name alone: the log keeps
/// `{name, path, hash}`, and a resume rebuilds the watcher from it.
pub fn kind_of(name: &str) -> SourceKind {
    match name {
        "prompt.md" => SourceKind::Base,
        "AGENTS.md" | "CLAUDE.md" => SourceKind::Instruction,
        _ if name.ends_with(".md") => SourceKind::Instruction,
        _ => SourceKind::Skill,
    }
}

/// One source whose text is in this context, as the watcher re-checks it.
#[derive(Debug, Clone, PartialEq)]
pub struct Tracked {
    pub kind: SourceKind,
    pub name: String,
    pub path: PathBuf,
    /// Every content hash this context already carries for the source: the one
    /// the system prompt was rendered from, and each one a `source` message
    /// carried.
    pub hashes: Vec<String>,
}

impl Tracked {
    fn has(&self, hash: &str) -> bool {
        self.hashes.iter().any(|seen| seen == hash)
    }
}

/// A source edited on disk since it went into the context.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub kind: SourceKind,
    pub name: String,
    pub path: PathBuf,
    pub text: String,
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

    /// Track the sources `session_started` recorded (what a resume reads).
    pub fn from_sources(sources: &[SourceInfo]) -> Watch {
        let mut watch = Watch::default();
        for source in sources {
            watch.add(
                kind_of(&source.name),
                &source.name,
                &source.path,
                &source.hash,
            );
        }
        watch
    }

    /// Track one source (the system prompt's, a `skills` verb's).
    pub fn add(&mut self, kind: SourceKind, name: &str, path: &str, hash: &str) {
        if let Some(existing) = self
            .tracked
            .iter_mut()
            .find(|tracked| tracked.kind == kind && tracked.name == name)
        {
            if !existing.has(hash) {
                existing.hashes.push(hash.to_string());
            }
            return;
        }
        self.tracked.push(Tracked {
            kind,
            name: name.to_string(),
            path: PathBuf::from(path.replace('/', std::path::MAIN_SEPARATOR_STR)),
            hashes: vec![hash.to_string()],
        });
    }

    /// Note that this context now carries `text` for a source (a `source`
    /// message, or a resumed session reading its own log).
    pub fn note_text(&mut self, kind: SourceKind, name: &str, text: &str) {
        let hash = hash_of(text);
        if let Some(existing) = self
            .tracked
            .iter_mut()
            .find(|tracked| tracked.kind == kind && tracked.name == name)
            && !existing.has(&hash)
        {
            existing.hashes.push(hash);
            return;
        }
        if !self
            .tracked
            .iter()
            .any(|tracked| tracked.kind == kind && tracked.name == name)
        {
            self.tracked.push(Tracked {
                kind,
                name: name.to_string(),
                path: PathBuf::new(),
                hashes: vec![hash],
            });
        }
    }

    pub fn tracked(&self) -> &[Tracked] {
        &self.tracked
    }

    /// Every source whose current content this context does not carry yet. The
    /// hash is remembered as it is returned, so one edit is one message — and a
    /// file edited back to text that is already in the context (the system
    /// prompt's own, or an earlier message's) is not a duplicate.
    pub fn changes(&mut self) -> Vec<Change> {
        let mut out = Vec::new();
        for tracked in &mut self.tracked {
            if tracked.path.as_os_str().is_empty() {
                continue;
            }
            let Some(text) = read(&tracked.path) else {
                continue;
            };
            let hash = hash_of(&text);
            if tracked.has(&hash) {
                continue;
            }
            tracked.hashes.push(hash);
            out.push(Change {
                kind: tracked.kind,
                name: tracked.name.clone(),
                path: tracked.path.clone(),
                text,
            });
        }
        out
    }
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

    /// The prompt is a contract with the model: a line that survives a retro
    /// must not vanish in a later edit. This pins the *lines* (not the exact
    /// bytes, which wrap), so a rewording has to be deliberate.
    #[test]
    fn the_embedded_prompt_carries_every_retro_rule() {
        let base = crate::config::DEFAULT_PROMPT;
        for (rule, why) in [
            ("Plan once, briefly", "F1: design thrash"),
            ("reasoning is dropped", "F1: the plan has nowhere to live"),
            ("edits", "F3: multi-hunk edits"),
            ("python/sed patch", "F3: a patch script fails with exit 0"),
            ("never chain", "F3: the exit code of each command"),
            ("Batch independent reads", "F4: parallel read-only calls"),
            ("git grep", "F7: search hygiene"),
            ("target/", "F7: do not walk the build directory"),
            ("never `cat` a big file", "F7: ranged reads"),
            (
                "share no shell state",
                "STEER-4: every call starts in the cwd",
            ),
            ("name the proving command", "F6: verify before claiming"),
            ("AGENTS.md", "instruction files are already loaded"),
            ("{{skills_dirs}}", "the skills pull path"),
        ] {
            assert!(base.contains(rule), "{why}: no {rule:?} in\n{base}");
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
    fn the_nearest_instruction_file_of_a_name_wins() {
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
        // One entry per name: the nearest one wins over the one further up.
        assert_eq!(instructions.len(), 1, "{instructions:?}");
        assert_eq!(instructions[0].text, "DEEP");
    }

    #[test]
    fn missing_directories_are_tolerated() {
        let dir = temp_dir("missing");
        let places = Places {
            config_dir: Some(dir.join("nope")),
            machine_claude: Some(dir.join("nope-claude")),
            skills_dirs: vec![dir.join("nope-skills")],
            cwd: dir.join("nope-work"),
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
        assert_eq!(rendered.sources[0].path, EMBEDDED);
        assert!(rendered.sources[1].path.ends_with("AGENTS.md"));
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
    fn an_edit_is_one_change_and_editing_back_is_none() {
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
        // Editing it back to text that is already in the context (here the
        // system prompt's own) is not a duplicate either.
        write(&file, "ONE");
        assert!(watch.changes().is_empty());
        // A third body is news again.
        write(&file, "THREE");
        assert_eq!(watch.changes().len(), 1);
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
            &crate::util::path_string(&file),
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
        let found = skills_in(&dir);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, "one");
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
        let text = read(&dir.join("sift").join("SKILL.md")).unwrap();
        let skill = parse_skill(&text).unwrap();
        assert_eq!(skill.name, "sift");
        assert_eq!(
            skill.body,
            "READ THE TAIL
"
        );
        // A directory without a SKILL.md is not a skill source.
        std::fs::create_dir_all(dir.join("empty")).unwrap();
        assert!(read(&dir.join("empty").join("SKILL.md")).is_none());
    }

    #[test]
    fn kinds_are_read_back_from_a_source_name() {
        assert_eq!(kind_of("prompt.md"), SourceKind::Base);
        assert_eq!(kind_of("AGENTS.md"), SourceKind::Instruction);
        assert_eq!(kind_of("CLAUDE.md"), SourceKind::Instruction);
        assert_eq!(kind_of("sift"), SourceKind::Skill);
        assert_eq!(SourceKind::Instruction.as_str(), "instruction");
    }

    /// N3: the base prompt is a contract with the model, and `include_str!` makes
    /// it part of every request's frozen prefix — a changed byte moves the cache
    /// for every session. So the bytes are pinned: `tests/fixtures/
    /// prompt.md.snapshot` is the prompt as released, and an edit has to update
    /// the snapshot in the same commit (a rewording is then deliberate, not a
    /// side effect of an unrelated change).
    #[test]
    fn the_base_prompt_is_byte_identical_to_its_snapshot() {
        let prompt = crate::config::DEFAULT_PROMPT;
        let snapshot = include_str!("../tests/fixtures/prompt.md.snapshot");
        assert_eq!(
            prompt,
            snapshot,
            "the base prompt changed ({} bytes now, {} in the snapshot); update \
             tests/fixtures/prompt.md.snapshot and the retro rule list deliberately",
            prompt.len(),
            snapshot.len()
        );
        // The rendered base (the prompt with its skills directories named) is
        // the prefix's head: same inputs, same bytes, and the placeholder is
        // the only thing that moved.
        let dir = temp_dir("snapshot");
        let places = places_in(&dir);
        let rendered = render_base(prompt, &places);
        assert!(!rendered.contains(SKILLS_PLACEHOLDER), "{rendered}");
        let head = prompt.split(SKILLS_PLACEHOLDER).next().unwrap();
        assert!(rendered.starts_with(head), "{rendered}");
        assert_eq!(rendered, render_base(prompt, &places), "not deterministic");
        assert!(prompt.len() <= 1024, "{} bytes", prompt.len());
    }
}
