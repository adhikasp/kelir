//! STEER-11 end-to-end: `kelir docs` and the builtin `kelir` skill, driven
//! through the real binary.
//!
//! The cwd of every run here is an empty temp directory, so there is no
//! `docs/reference` on disk: what `kelir docs` prints is the copy compiled into
//! the binary with `include_str!`. That is the point of the milestone — a
//! session with no checkout can read how to operate kelir. Every topic is
//! embedded with `include_str!`; nothing in `docs` touches the filesystem.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use crate::support::*;

/// Every topic `--list` printed, as `(name, summary)`.
fn listed(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (name, summary) = line.split_once("  ")?;
            Some((name.trim().to_string(), summary.trim().to_string()))
        })
        .collect()
}

#[tokio::test]
async fn docs_lists_every_topic_with_a_one_line_summary_and_no_checkout() {
    let mock = Mock::start(vec![]).await;
    // The cwd has no `docs/reference`: the binary's own copy answers.
    let run = run_task(&mock, &["docs", "--list"], &[], None, "").await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let topics = listed(&run.stdout);
    let names: Vec<&str> = topics.iter().map(|(name, _)| name.as_str()).collect();
    for expected in [
        "README",
        "config",
        "cli",
        "env",
        "verbs",
        "providers",
        "sources-and-skills",
        "files",
        "events",
        "tui",
        "troubleshooting",
        "source-map",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} is not listed: {names:?}"
        );
    }
    for (name, summary) in &topics {
        assert!(!summary.is_empty(), "{name} has no summary");
        assert_eq!(
            summary.lines().count(),
            1,
            "{name}'s summary is one line: {summary:?}"
        );
    }
    assert_eq!(mock.request_count(), 0, "`docs` calls no provider");
}

#[tokio::test]
async fn docs_prints_a_topic_and_a_bare_docs_prints_the_index() {
    let mock = Mock::start(vec![]).await;
    for (topic, needle) in [
        ("config", "`config.toml`"),
        ("env", "`KELIR_STATE_DIR`"),
        ("verbs", "`later`"),
        ("events", "`turn_ended`"),
        ("cli", "### `kelir docs`"),
        ("files", "events.jsonl"),
    ] {
        let run = run_task(&mock, &["docs", topic], &[], None, "").await;
        assert_eq!(run.code, 0, "{topic}: {}", run.stderr);
        assert!(
            run.stdout.contains(needle),
            "{topic} does not mention {needle:?}:\n{}",
            run.stdout
        );
        // The three-line opening is what makes a topic skimmable.
        assert!(run.stdout.contains("**Summary:**"), "{topic}");
        assert!(run.stdout.contains("**Read this when:**"), "{topic}");
        // `.md` is accepted too: a person types the file name.
        let with_suffix = run_task(&mock, &["docs", &format!("{topic}.md")], &[], None, "").await;
        assert_eq!(with_suffix.code, 0, "{topic}.md: {}", with_suffix.stderr);
        assert_eq!(with_suffix.stdout, run.stdout, "{topic}.md differs");
    }
    // No argument: the index, then how to read the rest.
    let run = run_task(&mock, &["docs"], &[], None, "").await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("config"), "{}", run.stdout);
    assert!(run.stdout.contains("kelir docs <topic>"), "{}", run.stdout);
}

#[tokio::test]
async fn an_unknown_topic_exits_1_and_suggests_the_nearest_one() {
    let mock = Mock::start(vec![]).await;
    let run = run_task(&mock, &["docs", "confg"], &[], None, "").await;
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("no topic `confg`"), "{}", run.stderr);
    assert!(run.stderr.contains("config"), "{}", run.stderr);
    assert!(run.stderr.contains("--list"), "{}", run.stderr);
    assert!(run.stdout.is_empty(), "{}", run.stdout);
    // A word from a topic's body still points somewhere useful.
    let run = run_task(&mock, &["docs", "blobs"], &[], None, "").await;
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("files"), "{}", run.stderr);
}

#[tokio::test]
async fn docs_search_prints_topic_and_line_numbers_best_topic_first() {
    let mock = Mock::start(vec![]).await;
    // A query whose answer is unambiguous ranks its file first: `429` is the
    // troubleshooting page, and `spill_limit` is a config key.
    for (query, expected) in [("429", "troubleshooting"), ("spill_limit", "config")] {
        let run = run_task(&mock, &["docs", "--search", query], &[], None, "").await;
        assert_eq!(run.code, 0, "{query}: {}", run.stderr);
        let first = run.stdout.lines().next().unwrap_or_default();
        assert!(first.starts_with(expected), "{query}: {}", run.stdout);
    }
    // A two-word query lists every file that holds a term, best first.
    let run = run_task(&mock, &["docs", "--search", "retry backoff"], &[], None, "").await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("providers"), "{}", run.stdout);
    assert!(run.stdout.contains("config"), "{}", run.stdout);
    // Every hit is `topic:line: text`, with a real line number and the term.
    for line in run.stdout.lines() {
        let (topic, after) = line.split_once(':').unwrap_or_else(|| panic!("{line:?}"));
        let (number, text) = after.split_once(": ").unwrap_or_else(|| panic!("{line:?}"));
        assert!(
            !topic.is_empty() && number.parse::<usize>().is_ok(),
            "{line:?}"
        );
        assert!(
            text.to_ascii_lowercase().contains("retry")
                || text.to_ascii_lowercase().contains("backoff"),
            "{line:?}"
        );
    }
    let no_hit = run_task(
        &mock,
        &["docs", "--search", "zzzz-no-such-word"],
        &[],
        None,
        "",
    )
    .await;
    assert_eq!(no_hit.code, 1);
    assert!(
        no_hit.stderr.contains("nothing in the reference"),
        "{}",
        no_hit.stderr
    );
}

#[tokio::test]
async fn the_builtin_kelir_skill_is_live_in_a_session_with_no_repo() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = run_task(&mock, &["run", "--json", "hi"], &[], None, "").await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    let started = run.one_event("session_started");
    let sources = started["sources"].as_array().expect("sources[]");
    let skill = sources
        .iter()
        .find(|source| source["name"] == "kelir")
        .unwrap_or_else(|| panic!("the builtin skill is not in {sources:?}"));
    // Embedded: a skill with no file, so nothing to watch or announce.
    assert_eq!(skill["kind"], "skill");
    assert_eq!(skill["path"], "");
    // The description is the "when", and it is what a pushed hint would show.
    let description =
        kelir::features::sources::parse_skill(kelir::features::sources::BUILTIN_SKILL)
            .expect("the builtin skill parses")
            .description;
    assert!(description.contains("kelir"), "{description}");
    // The body is not pushed (skills are pull), and the base prompt says where
    // the reference is.
    let system = started["system"]["text"].as_str().unwrap();
    // M8 fix F5: the builtin skills are listed (name and description) because
    // they have no file to find; the body is still never pushed.
    assert!(
        system.contains(&description),
        "a builtin skill is listed by description"
    );
    assert!(
        !system.contains("## The reference is inside the binary"),
        "a skill body is pull"
    );
    assert!(system.contains("kelir docs"), "{system}");
    // `kelir docs` answers with the same text from inside a call's cwd.
    let run = run_task(&mock, &["docs", "source-map"], &[], None, "").await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("src/config.rs"), "{}", run.stdout);
}

#[tokio::test]
async fn docs_size_is_reported_and_within_budget() {
    let mock = Mock::start(vec![]).await;
    let list = run_task(&mock, &["docs", "--list"], &[], None, "").await;
    assert_eq!(list.code, 0);
    let mut total = 0;
    let mut count = 0;
    for (name, _) in listed(&list.stdout) {
        let run = run_task(&mock, &["docs", &name], &[], None, "").await;
        assert_eq!(run.code, 0, "{name}: {}", run.stderr);
        total += run.stdout.len();
        count += 1;
    }
    eprintln!("the embedded reference is {total} bytes over {count} topics");
    assert!(count >= 12, "{count} topics");
    // The same ceiling the unit test holds the embed to (144 KB: the
    // M10/M11 topics pushed the reference to 143500). A rounding error next
    // to the binary's 20 MB budget.
    assert!(total <= 144 * 1024, "the reference is {total} bytes");
}
