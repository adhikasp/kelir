//! Sources: the base prompt, instruction files and skills directories.
//! The system prompt is rendered **once** at `session_started` and then frozen: a source
//! edited on disk mid-session arrives as a `source` event (a message at the tail), never as
//! a rewrite of the prefix, because prompt caching is the point of rendering once.

use std::path::{Path, PathBuf};

use super::*;

const INSTRUCTION_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// The skill kelir ships about itself (`src/docs/skill-kelir.md`, Agent Skills format).
pub const BUILTIN_SKILL_NAME: &str = "kelir";

pub const BUILTIN_SKILL: &str = include_str!("../../../docs/skill-kelir.md");

/// The skills compiled into the binary besides `kelir`: guidance on delegating to children and on
/// living inside a Herdr pane.
pub const BUILTIN_SKILLS: [(&str, &str); 3] = [
    (BUILTIN_SKILL_NAME, BUILTIN_SKILL),
    (
        "delegation",
        include_str!("../../../docs/skill-delegation.md"),
    ),
    ("herdr", include_str!("../../../docs/skill-herdr.md")),
];

/// The builtin skills, each a valid Agent Skill (a broken one would be a
/// build-time typo, and the drift test proves none is).
fn builtin_skills() -> Vec<Found> {
    BUILTIN_SKILLS
        .iter()
        .filter(|(_, text)| parse_skill(text).is_ok())
        .map(|(name, text)| Found {
            kind: SourceKind::Skill,
            name: (*name).to_string(),
            path: PathBuf::new(),
            text: (*text).to_string(),
            layer: Layer::Machine,
            builtin: true,
        })
        .collect()
}

/// Discover every machine and project source for `places`, dropping the notes.
pub fn discover(places: &Places) -> Vec<Found> {
    let mut notes = Vec::new();
    discover_with_notes(places, &mut notes)
}

/// Discover every machine and project source for `places`.
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
    // Project instruction files: every one found walking up from the cwd, nearest first.
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
    // The configured memory directory: one source per file, project layer.
    if let Some(directory) = &places.memory_dir {
        found.extend(crate::features::memory::scan(directory, notes));
    }
    // The skill kelir ships about itself: machine layer, so a project (or
    // machine) skill directory of the same name replaces it.
    found.extend(builtin_skills());
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

/// The cwd and its ancestors, nearest first, stopping at the repository root (a directory that holds
/// `.git`) and at the home directory (inclusive: a file *in* home still belongs to the project, one
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

/// Add a source, resolving a name clash the way the two layers demand: a project source wins over a
/// machine one of the same name.
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
        builtin: false,
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
pub(crate) fn skills_in(directory: &Path, notes: &mut Vec<String>) -> Vec<SkillFile> {
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

#[derive(Debug)]
pub(crate) struct SkillFile {
    /// The skill's name: its directory's name.
    name: String,
    path: PathBuf,
    text: String,
}

/// Read a source file: BOM dropped, invalid UTF-8 decoded lossily, length capped, and each of
/// those stated in `notes`.
pub(crate) fn read_file(
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
    // The total budget skips a file rather than truncating it, so every file that does go in is read the
    // same way the watcher re-reads it later (a truncation here that the watcher could not reproduce would
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

/// The one decode rule every source text goes through: a UTF-8 BOM is dropped, invalid UTF-8 becomes
/// U+FFFD (and is said so), and text over `limit` bytes is cut on a char boundary with a visible
pub(crate) fn decode(bytes: &[u8], limit: usize, notes: &mut Vec<String>) -> String {
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
        SourceKind::Memory => crate::features::memory::MAX_FILE_BYTES,
        SourceKind::Base | SourceKind::Instruction | SourceKind::Herdr => MAX_INSTRUCTION_BYTES,
    }
}

/// The base prompt with the skills directories named in it.
pub fn render_base(base: &str, places: &Places) -> String {
    let directories: Vec<String> = places
        .skills_directories()
        .iter()
        .map(|dir| format!("  - {}", crate::util::path_string(dir)))
        .collect();
    // The builtin skills have no file to pull: they are named here, with what
    // each is for and the one command that reads it.
    let mut lines = directories;
    lines.push(builtin_listing());
    let listing = lines.join("\n");
    if base.contains(SKILLS_PLACEHOLDER) {
        return base.replace(SKILLS_PLACEHOLDER, &listing);
    }
    format!("{}\n\n# Skills\n{}", base.trim_end(), listing)
}

/// The builtin skills as the prompt shows them: `name: description`, and the
/// command that prints one.
pub fn builtin_listing() -> String {
    let mut out =
        String::from("  - built in (no file; read one with `kelir docs --skill <name>`):");
    for (name, text) in BUILTIN_SKILLS {
        let description = parse_skill(text)
            .map(|skill| skill.description)
            .unwrap_or_default();
        out.push_str(&format!("\n    - {name} - {description}"));
    }
    out
}

/// Parse a `SKILL.md`: the Agent Skills front matter, and the body.
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
