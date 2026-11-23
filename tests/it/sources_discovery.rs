//! sources discovery tests: a_huge_instruction_file_still_, a_malformed_skill_is_not_recor, a_non_utf8_instruction_file_is, a_symlink_or_junction_loop_in_, an_ancestors_instruction_file_, an_explicit_prompt_file_is_rec, awkward_files_are_tolerated, base_prompt_layers_and_the_ski, nothing_outside_the_intended_p, project_wins_over_machine_and_, the_system_render_is_byte_iden.
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::{Path, PathBuf};

use crate::support::*;
use serde_json::{Value, json};

use super::sources_edges::*;

#[tokio::test]
async fn project_wins_over_machine_and_the_nearest_file_wins() {
    let tree = Tree::new();
    // Machine layer.
    write(&tree.config().join("AGENTS.md"), "MACHINE-AGENTS");
    write(&tree.config().join("CLAUDE.md"), "MACHINE-CLAUDE");
    write(
        &tree.home().join(".claude/skills/shared/SKILL.md"),
        skill("shared").replace("body of", "MACHINE body of"),
    );
    write(
        &tree.home().join(".claude/skills/only-machine/SKILL.md"),
        skill("only-machine"),
    );
    // Project layer: a root file and a nearer one.
    write(&tree.repo().join("AGENTS.md"), "ROOT-AGENTS");
    write(&tree.repo().join("sub/AGENTS.md"), "SUB-AGENTS");
    write(&tree.repo().join("sub/CLAUDE.md"), "SUB-CLAUDE");
    let cwd = tree.repo().join("sub/leaf");
    std::fs::create_dir_all(&cwd).unwrap();
    write(
        &cwd.join(".claude/skills/shared/SKILL.md"),
        skill("shared").replace("body of", "PROJECT body of"),
    );

    let (run, first) = run_in(&tree, &cwd, "").await;
    let started = run.one_event("session_started");
    let system = system_of(&first);
    // Project over machine on the same name; the nearer file over the farther.
    assert!(system.contains("SUB-AGENTS"), "{system}");
    assert!(system.contains("SUB-CLAUDE"), "{system}");
    assert!(!system.contains("MACHINE-AGENTS"), "{system}");
    assert!(!system.contains("MACHINE-CLAUDE"), "{system}");
    // The machine-only skill is still discovered; the clash resolves to the project.
    let sources = started["sources"].as_array().unwrap();
    let shared: Vec<&Value> = sources.iter().filter(|s| s["name"] == "shared").collect();
    assert_eq!(shared.len(), 1, "{sources:?}");
    assert!(
        shared[0]["path"]
            .as_str()
            .unwrap()
            .contains("leaf/.claude/skills/shared"),
        "{shared:?}"
    );
    assert!(source_names(started).contains(&"only-machine".to_string()));
    // Skills are pull: no body, no listing in the prompt.
    assert!(!system.contains("PROJECT body"), "{system}");
    assert!(!system.contains("only-machine"), "{system}");
    assert!(!system.contains("a skill called"), "{system}");
}

#[tokio::test]
async fn an_ancestors_instruction_file_is_collected_too() {
    let tree = Tree::new();
    write(&tree.repo().join("AGENTS.md"), "ROOT-AGENTS");
    write(&tree.repo().join("sub/AGENTS.md"), "SUB-AGENTS");
    let (_, first) = run_in(&tree, &tree.repo().join("sub"), "").await;
    let system = system_of(&first);
    assert!(system.contains("SUB-AGENTS"), "{system}");
    assert!(
        system.contains("ROOT-AGENTS"),
        "the brief says walk up and *collect*; the parent's file is missing:\n{system}"
    );
}

#[tokio::test]
async fn nothing_outside_the_intended_places_is_read() {
    let tree = Tree::new();
    // Decoys: a sibling, a child, an ancestor's .claude/skills (ancestors only
    // contribute instruction files), and a skills dir in the config's parent.
    write(&tree.root.join("sibling/AGENTS.md"), "SIBLING-DECOY");
    write(&tree.repo().join("work/child/AGENTS.md"), "CHILD-DECOY");
    write(
        &tree.repo().join("work/child/.claude/skills/c/SKILL.md"),
        skill("child-skill"),
    );
    write(
        &tree.repo().join(".claude/skills/anc/SKILL.md"),
        skill("ancestor-skill"),
    );
    write(
        &tree.repo().join(".kelir/prompt.md"),
        "ANCESTOR-PROMPT-DECOY",
    );
    write(
        &tree.root.join("machine/skills/m/SKILL.md"),
        skill("config-parent-skill"),
    );
    write(
        &tree.home().join("skills/h/SKILL.md"),
        skill("home-skills-decoy"),
    );
    write(&tree.home().join("CLAUDE.md"), "HOME-CLAUDE-DECOY");
    write(&tree.repo().join("work/AGENTS.md"), "REAL");
    let (run, first) = run_in(&tree, &tree.repo().join("work"), "").await;
    let system = system_of(&first);
    for decoy in [
        "SIBLING-DECOY",
        "CHILD-DECOY",
        "ANCESTOR-PROMPT-DECOY",
        "HOME-CLAUDE-DECOY",
    ] {
        assert!(!system.contains(decoy), "{decoy} leaked into:\n{system}");
    }
    let names = source_names(run.one_event("session_started"));
    for decoy in [
        "child-skill",
        "ancestor-skill",
        "config-parent-skill",
        "home-skills-decoy",
    ] {
        assert!(!names.contains(&decoy.to_string()), "{names:?}");
    }
    assert!(system.contains("REAL"));
}

#[tokio::test]
async fn the_system_render_is_byte_identical_and_has_no_time() {
    let tree = Tree::new();
    write(&tree.repo().join("AGENTS.md"), "SAME");
    write(&tree.repo().join(".claude/skills/sift/SKILL.md"), SKILL);
    // The session id is per-session, so strip the session block before the
    // byte-identical check; both prompts must otherwise be the same bytes.
    let (a, first_a) = run_in(&tree, &tree.repo(), "").await;
    let (b, first_b) = run_in(&tree, &tree.repo(), "").await;
    let strip_session = |text: &str| {
        text.split("\n# Session\n")
            .next()
            .unwrap_or(text)
            .to_string()
    };
    assert_eq!(
        strip_session(&system_of(&first_a)),
        strip_session(&system_of(&first_b))
    );
    let (sa, sb) = (
        a.one_event("session_started"),
        b.one_event("session_started"),
    );
    // Each prompt is self-aware: it carries its own id and resume command.
    let id_a = sa["id"].as_str().unwrap();
    let id_b = sb["id"].as_str().unwrap();
    assert_ne!(id_a, id_b);
    assert!(system_of(&first_a).contains(&format!("id: {id_a}")));
    assert!(system_of(&first_b).contains(&format!("resume: kelir tui --resume {id_b}")));
    assert_eq!(
        sa["system"]["hash"].as_str().is_some(),
        sb["system"]["hash"].as_str().is_some()
    );
    assert_eq!(sa["sources"], sb["sources"]);
    let system = system_of(&first_a);
    let date = system.lines().find(|l| l.starts_with("date:")).unwrap();
    assert_eq!(date.len(), "date: 2026-11-19".len(), "{date}");
    assert!(
        !has_clock(&system),
        "a time of day is in the system prompt:
{system}"
    );
}

#[tokio::test]
async fn awkward_files_are_tolerated() {
    let tree = Tree::new();
    let repo = tree.repo();
    // CRLF instruction file with a BOM.
    write(&repo.join("AGENTS.md"), "\u{feff}line one\r\nline two\r\n");
    // Invalid UTF-8.
    write(&repo.join("CLAUDE.md"), [b'x', 0xff, 0xfe, b'y']);
    let skills = repo.join(".claude/skills");
    write(&skills.join("good/SKILL.md"), skill("good"));
    write(&skills.join(".hidden/SKILL.md"), skill("hidden"));
    write(&skills.join("stray.md"), "not a skill dir");
    std::fs::create_dir_all(skills.join("empty")).unwrap();
    let (run, first) = run_in(&tree, &repo, "").await;
    let names = source_names(run.one_event("session_started"));
    assert!(names.contains(&"good".to_string()), "{names:?}");
    assert!(!names.contains(&"hidden".to_string()), "{names:?}");
    assert!(!names.contains(&"empty".to_string()), "{names:?}");
    assert!(!names.contains(&"stray.md".to_string()), "{names:?}");
    let system = system_of(&first);
    assert!(system.contains("line one"), "{system}");
    assert!(system.contains("line two"), "{system}");
}

#[tokio::test]
async fn a_non_utf8_instruction_file_is_not_silently_dropped() {
    let tree = Tree::new();
    write(
        &tree.repo().join("AGENTS.md"),
        b"caf\xe9 rules: ALWAYS-RUN-GATES",
    );
    let (run, first) = run_in(&tree, &tree.repo(), "").await;
    let names = source_names(run.one_event("session_started"));
    let system = system_of(&first);
    assert!(
        names.contains(&"AGENTS.md".to_string()) || system.contains("ALWAYS-RUN-GATES"),
        "the file vanished without a trace: {names:?}"
    );
}

#[tokio::test]
async fn a_malformed_skill_is_not_recorded_as_a_source() {
    let tree = Tree::new();
    let skills = tree.repo().join(".claude/skills");
    write(&skills.join("good/SKILL.md"), skill("good"));
    write(
        &skills.join("nofm/SKILL.md"),
        "just prose, no front matter\n",
    );
    write(
        &skills.join("unclosed/SKILL.md"),
        "---\nname: unclosed\n\nno closing fence\n",
    );
    write(
        &skills.join("noname/SKILL.md"),
        "---\ndescription: d\n---\nbody\n",
    );
    let (run, _) = run_in(&tree, &tree.repo(), "").await;
    let names = source_names(run.one_event("session_started"));
    for bad in ["nofm", "unclosed", "noname"] {
        assert!(
            !names.contains(&bad.to_string()),
            "{bad} is not a valid Agent Skill but is in sources[]: {names:?}"
        );
    }
    // And the skip is explained, once per file, with the reason.
    let notes: Vec<String> = run
        .events_of("error")
        .iter()
        .map(|event| event["message"].as_str().unwrap_or("").to_string())
        .collect();
    for bad in ["nofm", "unclosed", "noname"] {
        assert!(
            notes
                .iter()
                .any(|message| message.contains(&format!("{bad}/SKILL.md"))
                    && message.contains("is not a skill")),
            "{bad} was skipped without a reason: {notes:?}"
        );
    }
}

#[tokio::test]
async fn a_huge_instruction_file_still_starts_and_is_recorded() {
    // Characterisation (see F7): a 3 MB instruction file is capped, not inlined
    // whole, and the truncation is visible in the prompt and in the log. Before
    // the fix the system prompt was 3.1 MB.
    let tree = Tree::new();
    let big = "rule: keep steps small and verified\n".repeat(3 * 1024 * 1024 / 36);
    write(&tree.repo().join("AGENTS.md"), &big);
    let started = std::time::Instant::now();
    let (run, first) = run_in(&tree, &tree.repo(), "").await;
    let system = system_of(&first);
    eprintln!(
        "huge AGENTS.md: system prompt {} bytes, run took {:?}",
        system.len(),
        started.elapsed()
    );
    assert!(
        system.len() < 512 * 1024,
        "the system prompt is {} bytes: the cap (F7) did not hold",
        system.len()
    );
    assert!(
        system.contains("[kelir: truncated"),
        "the truncation is not visible in the prompt"
    );
    assert_eq!(
        run.one_event("session_started")["system"]["hash"]
            .as_str()
            .unwrap()
            .len(),
        16
    );
    // And the log says why the file is short.
    let warnings: Vec<String> = run
        .events_of("error")
        .iter()
        .map(|event| event["message"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        warnings
            .iter()
            .any(|message| message.contains("AGENTS.md") && message.contains("only the first")),
        "no truncation note: {warnings:?}"
    );
}

#[tokio::test]
async fn a_symlink_or_junction_loop_in_a_skills_dir_is_harmless() {
    let tree = Tree::new();
    let skills = tree.repo().join(".claude/skills");
    write(&skills.join("sift/SKILL.md"), SKILL);
    let link = skills.join("self");
    link_dir(&link, &skills);
    // And a loop through the cwd itself.
    let cwd_link = tree.repo().join("loop");
    link_dir(&cwd_link, &tree.repo());
    let (run, _) = run_in(&tree, &tree.repo().join("loop"), "").await;
    let names = source_names(run.one_event("session_started"));
    assert_eq!(
        names.iter().filter(|n| *n == "sift").count(),
        1,
        "{names:?}"
    );
    // Remove the junction without following it.
    #[cfg(windows)]
    {
        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir(&cwd_link);
    }
}

#[tokio::test]
async fn base_prompt_layers_and_the_skills_placeholder() {
    let tree = Tree::new();
    // Machine prompt without the placeholder: the dirs are appended, never lost.
    write(&tree.config().join("prompt.md"), "MACHINE BASE PROMPT");
    let (run, first) = run_in(&tree, &tree.repo(), "").await;
    let system = system_of(&first);
    assert!(system.starts_with("MACHINE BASE PROMPT"), "{system}");
    assert!(system.contains(".claude/skills"), "{system}");
    let started = run.one_event("session_started");
    assert_eq!(started["sources"][0]["name"], "prompt.md");
    assert!(
        started["sources"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("prompt.md")
            && started["sources"][0]["path"] != "embedded",
        "{}",
        started["sources"][0]
    );
    // A project prompt wins over the machine one.
    write(
        &tree.repo().join(".kelir/prompt.md"),
        "PROJECT BASE PROMPT {{skills_dirs}}",
    );
    let (run, first) = run_in(&tree, &tree.repo(), "").await;
    let system = system_of(&first);
    assert!(system.starts_with("PROJECT BASE PROMPT"), "{system}");
    assert!(!system.contains("MACHINE BASE PROMPT"), "{system}");
    assert!(
        run.one_event("session_started")["sources"][0]["path"]
            .as_str()
            .unwrap()
            .contains(".kelir/prompt.md")
    );
}

#[tokio::test]
async fn an_explicit_prompt_file_is_recorded_with_its_path() {
    let tree = Tree::new();
    let prompt = tree.root.join("my-prompt.txt");
    write(&prompt, "EXPLICIT BASE PROMPT");
    let extra = format!(
        "prompt = \"{}\"\n",
        prompt.to_string_lossy().replace('\\', "/")
    );
    let (run, first) = run_in(&tree, &tree.repo(), &extra).await;
    assert!(system_of(&first).starts_with("EXPLICIT BASE PROMPT"));
    let path = run.one_event("session_started")["sources"][0]["path"].clone();
    assert_ne!(
        path, "embedded",
        "the source says it is the embedded default"
    );
}
