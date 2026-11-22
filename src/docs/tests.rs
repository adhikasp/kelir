//! The drift test: every name the code owns appears at least once in the
//! embedded reference. One direction only — code to docs. A doc mentioning a
//! removed name is caught at review.

use super::*;

const SKILL: &str = include_str!("skill-kelir.md");
const SPEC: &str = include_str!("../../docs/SPEC.md");

/// Log event names retired by format 1. These topics describe the log; a name
/// here must be the v1 spelling.
const OLD_EVENTS: &[&str] = &[
    "mcp_started",
    "mcp_settled",
    "tool_started",
    "tool_settled",
    "bg_started",
    "bg_settled",
    "child_started",
    "child_settled",
];
/// `config`, `env` and `notify` name *notify hooks*, which keep their spellings;
/// the log-event drift check does not apply to them.
const EVENT_TOPICS: &[&str] = &[
    "events",
    "mcp",
    "children",
    "troubleshooting",
    "verbs",
    "gates",
    "attach",
];

/// The `note.kind` values a producer writes. A value in this list must be
/// described in `events.md`; a value the docs describe must be written here.
const NOTE_KINDS: &[(&str, &str)] = &[
    ("prompt_tokens", include_str!("../usage.rs")),
    ("notify", include_str!("../core/session/report.rs")),
];

#[test]
fn documented_note_kinds_match_the_producers() {
    let events = TOPICS
        .iter()
        .find(|entry| entry.name == "events")
        .expect("the events topic exists")
        .body;
    for (kind, producer) in NOTE_KINDS {
        assert!(
            producer.contains(&format!("kind: \"{kind}\"")),
            "no producer writes note.kind `{kind}`"
        );
        assert!(
            events.contains(&format!("`{kind}`")),
            "events.md does not document note.kind `{kind}`"
        );
    }
}

#[test]
fn no_removed_event_names_in_the_event_reference() {
    for entry in TOPICS
        .iter()
        .filter(|entry| EVENT_TOPICS.contains(&entry.name))
    {
        for old in OLD_EVENTS {
            assert!(
                !entry.body.contains(old),
                "docs/reference/{}.md still names removed event `{old}`",
                entry.name
            );
        }
    }
    for old in OLD_EVENTS {
        assert!(
            !SPEC.contains(old),
            "docs/SPEC.md still names removed event `{old}`"
        );
    }
}

/// A code name is checked against the topic that owns it: a verb in another
/// topic's prose is not a row in `verbs.md`.
fn quoted_in(topic_name: &str, name: &str) -> bool {
    let body = TOPICS
        .iter()
        .find(|entry| entry.name == topic_name)
        .expect("the topic exists")
        .body;
    body.contains(&format!("`{name}`"))
}

#[test]
fn reference_mentions_everything() {
    let mut missing = Vec::new();
    let mut check = |topic: &str, name: &str, label: &str| {
        if !quoted_in(topic, name) {
            missing.push(format!("{label} `{name}` (docs/reference/{topic}.md)"));
        }
    };
    for name in crate::verbs::Verb::NAMES {
        check("verbs", name, "verb");
    }
    for name in crate::config::KEYS {
        check("config", name, "config key");
    }
    for name in crate::event::EventBody::NAMES {
        check("events", name, "event");
    }
    for var in crate::envs::VARS {
        check("env", var.name, "env var");
    }
    assert!(
        missing.is_empty(),
        "the reference does not mention code that exists:\n{}",
        missing.join("\n")
    );
}

#[test]
fn every_topic_opens_with_its_summary_and_has_one_line_in_the_list() {
    assert!(!TOPICS.is_empty(), "no topics");
    for entry in TOPICS {
        assert!(
            entry.body.starts_with("# "),
            "{}: not a reference file",
            entry.name
        );
        assert!(
            !entry.summary().is_empty() && !entry.summary().contains('\n'),
            "{}: bad summary {:?}",
            entry.name,
            entry.summary()
        );
        let section = entry
            .body
            .split_once("**Summary:**")
            .expect("a summary")
            .1
            .split("**Read this when:**")
            .next()
            .unwrap_or_default();
        for word in entry.summary().split_whitespace() {
            assert!(
                section.contains(word),
                "{}: summary word {word:?} is not in the file's **Summary:**",
                entry.name
            );
        }
        assert_eq!(
            topic_by_name(&format!("{}.md", entry.name)).map(|found| found.name),
            Some(entry.name),
            "{}: `.md` is not accepted",
            entry.name
        );
    }
    // Every markdown file under docs/reference is embedded; one nobody can
    // read with `kelir docs <topic>` is a docs bug.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference");
    for entry in std::fs::read_dir(&root)
        .expect("docs/reference exists")
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_string_lossy();
        assert!(
            topic_by_name(&stem).is_some(),
            "docs/reference/{stem} is not in TOPICS"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            topic_by_name(&stem).unwrap().body,
            "stale embed: {stem}"
        );
    }
}

#[test]
fn search_prints_topic_and_line_numbers_best_topic_first() {
    let hits = search("spill_limit");
    assert!(!hits.is_empty(), "no hit for spill_limit");
    assert_eq!(hits[0].topic, "config", "{:?}", &hits[..3]);
    for hit in &hits {
        assert!(hit.number > 0 && !hit.line.is_empty());
        assert!(
            hit.line.to_ascii_lowercase().contains("spill_limit"),
            "a hit without the term: {hit:?}"
        );
    }
    assert_eq!(search("zzzz-no-such-word").len(), 0);
    assert_eq!(occurrences("AB", "ab ab Ab"), 3);
    assert!(!suggestions("confg").is_empty());
    assert!(suggestions("blobs").contains(&"files"));
}

#[test]
fn the_embedded_reference_fits_its_size_budget() {
    let total: usize = TOPICS.iter().map(|entry| entry.body.len()).sum();
    assert!(total <= 144 * 1024, "the reference is {total} bytes");
}

// ------------------------------------------------------------- the base prompt

#[test]
fn the_base_prompt_names_kelir_docs_within_its_budget() {
    const PROMPT: &str = include_str!("../prompt.md");
    assert!(
        PROMPT.contains("kelir docs"),
        "the base prompt does not tell the model that `kelir docs` exists"
    );
    assert!(
        PROMPT.len() <= 1024,
        "the base prompt is {} bytes",
        PROMPT.len()
    );
}

// ---------------------------------------------------------- the builtin skill

#[test]
fn the_builtin_skill_is_a_valid_agent_skill_that_points_at_the_reference() {
    let skill = crate::sources::parse_skill(SKILL).expect("the builtin skill parses");
    assert_eq!(skill.name, crate::sources::BUILTIN_SKILL_NAME);
    // The description is the "when": it has to say what the skill is for, and
    // name the surface, because it is all a pushed hint would show.
    let description = skill.description.to_ascii_lowercase();
    for word in ["operate", "configure", "kelir"] {
        assert!(
            description.contains(word),
            "the builtin skill's description does not say {word:?}: {}",
            skill.description
        );
    }
    // The body points at the reference and at the source, and names every
    // topic: a topic the skill forgets is one an agent will not find.
    assert!(skill.body.contains("kelir docs"), "{}", skill.body);
    for entry in TOPICS {
        assert!(
            skill.body.contains(&format!("`{}`", entry.name)),
            "the builtin skill does not name the topic `{}`",
            entry.name
        );
    }
    assert!(
        SKILL.len() <= crate::sources::MAX_SKILL_BYTES,
        "the builtin skill is {} bytes",
        SKILL.len()
    );
}

// -------------------------------------------------- the delegation and herdr skills

#[test]
fn the_delegation_and_herdr_skills_are_valid_short_and_say_what_they_must() {
    for (name, text) in crate::sources::BUILTIN_SKILLS {
        let skill = crate::sources::parse_skill(text).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(skill.name, name);
        assert!(!skill.description.is_empty(), "{name}: no description");
        assert!(text.len() <= 6 * 1024, "{name}: skill too long");
        for banned in ["the maintainer", "a Linux box", "a build box", "users/"] {
            assert!(
                !text
                    .to_ascii_lowercase()
                    .contains(&banned.to_ascii_lowercase()),
                "{name}: {banned}"
            );
        }
    }
    let (delegation, herdr) = (
        crate::sources::BUILTIN_SKILLS[1].1,
        crate::sources::BUILTIN_SKILLS[2].1,
    );
    for (text, needles) in [
        (
            delegation,
            &[
                "kelir run --child",
                "--background",
                "kelir brief",
                "kelir ls",
                "kelir kill",
                "kelir docs children",
            ] as &[&str],
        ),
        (
            herdr,
            &[
                "hc ls --json",
                "hc get",
                "hc read",
                "hc send",
                "hc wait",
                "hc machine ls",
                "HC_TERMINAL_ID",
                "where",
                "Never kill",
                "NOT in Herdr",
            ] as &[&str],
        ),
    ] {
        for needle in needles {
            assert!(text.contains(needle), "a skill does not mention {needle}");
        }
    }
}

// ------------------------------------------- a skill names only commands that exist

/// Every `kelir …` command a skill shows in a code span or block, as the words after `kelir`
/// that look like a subcommand path, and the `--flags` that follow on the same command.
fn commands_in(text: &str) -> Vec<(String, Vec<String>, Vec<String>)> {
    let mut code = String::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            code.push_str(line);
            code.push('\n');
        } else {
            let mut parts = line.split('`');
            parts.next();
            for (i, span) in parts.enumerate() {
                if i % 2 == 0 {
                    code.push_str(span);
                    code.push('\n');
                }
            }
        }
    }
    let mut out = Vec::new();
    for line in code.lines() {
        let mut rest = line;
        while let Some(at) = rest.find("kelir ") {
            let before = rest[..at].chars().last();
            let after = &rest[at + "kelir ".len()..];
            rest = after;
            if before.is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '/') {
                continue;
            }
            let mut path = Vec::new();
            let mut flags = Vec::new();
            for word in after.split_whitespace() {
                if word.starts_with("--") {
                    flags.push(word.trim_end_matches([',', ';', ')']).to_string());
                } else if flags.is_empty()
                    && word.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                {
                    path.push(word.to_string());
                } else {
                    break;
                }
            }
            out.push((line.to_string(), path, flags));
        }
    }
    out
}

#[test]
fn builtin_skills_name_only_subcommands_and_flags_kelir_help_lists() {
    use clap::CommandFactory as _;
    let root = crate::cli::Cli::command();
    let mut checked = 0;
    for (name, text) in crate::sources::BUILTIN_SKILLS {
        for (line, path, flags) in commands_in(text) {
            let mut command = &root;
            let mut walked = Vec::new();
            for word in &path {
                let Some(sub) = command
                    .get_subcommands()
                    .find(|sub| !sub.is_hide_set() && sub.get_name() == word)
                else {
                    // A word after the subcommand path is an argument ("kelir docs config").
                    assert!(
                        !walked.is_empty() || command.get_subcommands().next().is_none(),
                        "skill {name}: `kelir {word}` is not a subcommand `kelir --help` lists (line: {line})"
                    );
                    break;
                };
                command = sub;
                walked.push(word.clone());
            }
            for flag in flags {
                let long = flag.trim_start_matches("--");
                let long = long.split('=').next().unwrap_or(long);
                let known = command
                    .get_arguments()
                    .any(|arg| arg.get_long() == Some(long))
                    || root.get_arguments().any(|arg| arg.get_long() == Some(long))
                    || long == "help";
                assert!(
                    known,
                    "skill {name}: `kelir {} {flag}`: no such flag in `kelir --help` (line: {line})",
                    walked.join(" ")
                );
            }
            checked += 1;
        }
    }
    assert!(
        checked >= 8,
        "only {checked} commands were checked: the scanner is broken"
    );
}

#[test]
fn the_command_scanner_catches_a_command_that_does_not_exist() {
    use clap::CommandFactory as _;
    let found = commands_in(
        "```sh
kelir run --child \"x\"
kelir frobnicate --nope \"y\"
```
and `kelir run --warp`
",
    );
    assert_eq!(found.len(), 3);
    let root = crate::cli::Cli::command();
    // The real ones resolve (the merge of children made `--child` and `brief` real)...
    let run = root.find_subcommand("run").unwrap();
    assert!(
        run.get_arguments()
            .any(|arg| arg.get_long() == Some("child"))
    );
    assert!(root.get_subcommands().any(|sub| sub.get_name() == "brief"));
    // ...and invented ones do not, which is what the skill test trips over.
    assert_eq!(found[1].1, vec!["frobnicate".to_string()]);
    assert!(
        root.get_subcommands()
            .all(|sub| sub.get_name() != "frobnicate")
    );
    assert!(
        !run.get_arguments()
            .any(|arg| arg.get_long() == Some("warp"))
    );
    assert_eq!(found[2].2, vec!["--warp".to_string()]);
}
