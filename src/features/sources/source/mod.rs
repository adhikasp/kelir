//! Sources: the base prompt, instruction files and skills directories.
//! The system prompt is rendered **once** at `session_started` and then frozen: a source
//! edited on disk mid-session arrives as a `source` event (a message at the tail), never as
//! a rewrite of the prefix, because prompt caching is the point of rendering once.

use std::path::{Path, PathBuf};

use crate::log::event::{LogEvent, SourceInfo};

pub mod discover;
pub mod render;
pub mod types;
pub mod watch;

pub use discover::*;
pub use render::*;
pub use types::*;
pub use watch::*;

mod tests {
    use super::*;

    #[allow(dead_code)]
    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kelir-source-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[allow(dead_code)]
    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    #[allow(dead_code)]
    fn places_in(root: &Path) -> Places {
        Places {
            memory_dir: None,
            config_dir: Some(root.join("config")),
            machine_claude: Some(root.join("machine-claude")),
            skills_dirs: vec![root.join("extra-skills")],
            cwd: root.join("work").join("deep"),
            home: None,
            repo_root: None,
        }
    }

    #[allow(dead_code)]
    const SKILL: &str = "---\nname: sift\ndescription: use when a log is too long to read\nmetadata:\n  owner: gates\n---\n\n# Sift\n\nRead the tail first.\n";

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
        // One skill of that name survives the clash, and it is the project's;
        // `kelir` is always there too, and it has no file.
        let skills: Vec<&Found> = found
            .iter()
            .filter(|item| item.kind == SourceKind::Skill)
            .collect();
        assert_eq!(skills.len(), 1 + BUILTIN_SKILLS.len(), "{skills:?}");
        let builtin = skills
            .iter()
            .find(|skill| skill.builtin)
            .expect("the embedded `kelir` skill is always discovered");
        assert_eq!(builtin.name, BUILTIN_SKILL_NAME);
        assert!(builtin.path.as_os_str().is_empty());
        assert_eq!(parse_skill(&builtin.text).unwrap().name, "kelir");
        let mine = skills.iter().find(|skill| skill.name == "mine").unwrap();
        // The watcher hashes the file it read, front matter included.
        assert!(mine.text.ends_with("PROJECT BODY"), "{skills:?}");
        assert_eq!(parse_skill(&mine.text).unwrap().name, "mine");
        assert_eq!(mine.layer, Layer::Project);
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
            herdr: None,
            session_id: None,
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
    fn the_system_carries_the_instruction_text_and_the_sources() {
        let dir = temp_dir("system");
        let places = places_in(&dir);
        write(
            &dir.join("work").join("deep").join("AGENTS.md"),
            "ALWAYS RUN THE GATES",
        );
        let inputs = Inputs {
            herdr: None,
            session_id: None,
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
        // The base prompt, the instruction file, and the builtin skill: a
        // skill's body is pull, so only its name and hash are here.
        assert_eq!(
            names,
            vec!["prompt.md", "AGENTS.md", "delegation", "herdr", "kelir"]
        );
        // The embedded default records an empty path and its kind, never a
        // sentinel a reader could mistake for a file.
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
            herdr: None,
            session_id: None,
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
            herdr: None,
            session_id: None,
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
        // Editing it back to the text the system prompt rendered is a change again: the model's
        // latest word on the file is TWO, and the file on disk says ONE .
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
            herdr: None,
            session_id: None,
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
        // wins over the name .
        assert_eq!(kind_of(&info("notes.md", Some("skill"))), SourceKind::Skill);
        // An older log has no kind: the name is the fallback.
        assert_eq!(kind_of(&info("prompt.md", None)), SourceKind::Base);
        assert_eq!(kind_of(&info("AGENTS.md", None)), SourceKind::Instruction);
        assert_eq!(kind_of(&info("CLAUDE.md", None)), SourceKind::Instruction);
        assert_eq!(kind_of(&info("sift", None)), SourceKind::Skill);
        assert_eq!(SourceKind::Instruction.as_str(), "instruction");
    }

    #[test]
    fn decode_drops_bom_decodes_lossily_and_marks_truncation() {
        let mut notes = Vec::new();
        assert_eq!(decode(b"\xef\xbb\xbfA\r\nB", 4096, &mut notes), "A\r\nB");
        assert!(notes.is_empty(), "{notes:?}");
        notes.clear();
        let text = decode(b"caf\xe9 rules", 4096, &mut notes);
        assert!(text.contains('\u{fffd}'), "{text:?}");
        assert!(notes[0].contains("not valid UTF-8"), "{notes:?}");
        notes.clear();
        let text = decode("ééééé".as_bytes(), 5, &mut notes);
        assert!(text.starts_with("éé"), "{text:?}");
        assert!(text.contains("[kelir: truncated"), "{text:?}");
        assert!(text.contains("10 bytes on disk"), "{text:?}");

        let places = Places::default();
        let rendered = render_system(&Inputs {
            herdr: None,
            session_id: Some("ses_abc123"),
            base: "BASE",
            base_path: None,
            places: &places,
            cwd: &places.cwd,
            date: "2026-11-19",
        });
        assert!(
            rendered.text.contains("id: ses_abc123"),
            "{}",
            rendered.text
        );
        assert!(
            rendered
                .text
                .contains("resume: kelir tui --resume ses_abc123")
        );
        assert!(!rendered.hash.is_empty());

        let dir = temp_dir("stop-pins");
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

        let bad = temp_dir("bad-skill-pins");
        write(&bad.join("nofm").join("SKILL.md"), "just prose");
        write(
            &bad.join("noname").join("SKILL.md"),
            "---\ndescription: d\n---\nbody",
        );
        let mut notes = Vec::new();
        assert!(skills_in(&bad, &mut notes).is_empty());
        assert_eq!(notes.len(), 2, "{notes:?}");
        write(&dir.join("repo").join(".git").join("HEAD"), "ref");
        assert_eq!(
            repo_root_above(&dir.join("repo").join("sub")).as_deref(),
            Some(dir.join("repo").as_path())
        );
    }
}
