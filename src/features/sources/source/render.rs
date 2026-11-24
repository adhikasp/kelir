//! Sources: the base prompt, instruction files and skills directories.
//! The system prompt is rendered **once** at `session_started` and then frozen: a source
//! edited on disk mid-session arrives as a `source` event (a message at the tail), never as
//! a rewrite of the prefix, because prompt caching is the point of rendering once.

use super::*;

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
    /// The session id, when the caller has one.
    pub session_id: Option<&'a str>,
    /// The one Herdr line (`herdr_ctx`), when the session runs in a pane.
    pub herdr: Option<&'a str>,
}

/// Render the system prompt once: the base prompt with the skills directories named in it,
/// the instruction files, then the environment.
pub fn render_system(inputs: &Inputs) -> Rendered {
    let mut warnings = Vec::new();
    // The base prompt: the project's `.kelir/prompt.md` wins over whatever the config named (an explicit
    // `prompt` file, the config dir's `prompt.md`, or the embedded default), and each case records the
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
    // Memory is rendered like an instruction file: a heading, the path, the text. It is the
    // model's own notes, loaded once; later changes are `source` messages at the tail.
    let memory: Vec<&Found> = discovered
        .iter()
        .filter(|found| found.kind == SourceKind::Memory)
        .collect();
    if !memory.is_empty() {
        text.push_str("\n# Memory\n");
        for found in memory.iter() {
            text.push_str(&format!(
                "\n## {} - {}\n\n{}\n",
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
    if let Some(id) = inputs.session_id {
        text.push_str(&format!(
            "\n# Session\nid: {id}\nresume: kelir tui --resume {id}\n"
        ));
    }
    if let Some(line) = inputs.herdr {
        text.push_str(line);
        text.push('\n');
    }

    let mut sources = vec![info("prompt.md", &base_path, SourceKind::Base, &base_text)];
    for found in &instructions {
        sources.push(info(
            &found.name,
            &found_path(found),
            found.kind,
            &found.text,
        ));
    }
    for found in &memory {
        sources.push(info(
            &found.name,
            &found_path(found),
            found.kind,
            &found.text,
        ));
    }
    if let Some(line) = inputs.herdr {
        sources.push(info(
            crate::features::herdr::ctx::SOURCE_NAME,
            &SourcePath::Embedded,
            SourceKind::Herdr,
            line,
        ));
    }
    // Skills are pull: their bodies stay out of the prompt, but they are recorded (and watched)
    // so a skill edited mid-session arrives as a `source` message like any other file.
    let mut skills: Vec<&Found> = discovered
        .iter()
        .filter(|found| found.kind == SourceKind::Skill)
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    for found in skills {
        sources.push(info(
            &found.name,
            &found_path(found),
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

/// The [`SourcePath`] a discovered source stands for: none at all for the
/// builtin skill (it has no file), the file it was read from otherwise.
fn found_path(found: &Found) -> SourcePath {
    if found.builtin {
        SourcePath::Builtin
    } else {
        SourcePath::File(found.path.clone())
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
