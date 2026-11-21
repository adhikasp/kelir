//! STEER-11 end-to-end: `kelir docs` and the builtin `kelir` skill, driven
//! through the real binary.
//!
//! The cwd of every run here is an empty temp directory, so there is no
//! `docs/reference` on disk: what `kelir docs` prints is the copy compiled into
//! the binary with `include_str!`. That is the point of the milestone — a
//! session with no checkout can read how to operate kelir. `--generate` and
//! `--check` need a checkout, and those tests point `KELIR_DOCS_ROOT` at a
//! scratch copy so they never touch the repo's own files.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
mod support;

use std::path::{Path, PathBuf};

use support::*;

/// A scratch copy of the repo's `docs/reference`, so `--generate` writes into a
/// temp dir rather than into the checkout the test is running from.
fn reference_copy() -> (tempfile::TempDir, PathBuf) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("reference");
    let temp = tempfile::tempdir().expect("temp dir");
    let target = temp.path().join("reference");
    std::fs::create_dir_all(&target).unwrap();
    for entry in std::fs::read_dir(&source)
        .expect("docs/reference exists")
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
            std::fs::copy(&path, target.join(path.file_name().unwrap())).unwrap();
        }
    }
    (temp, target)
}

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
async fn docs_find_ranks_the_topic_that_answers_and_exits_1_on_no_hit() {
    let mock = Mock::start(vec![]).await;
    // A query whose answer is unambiguous ranks its file first: `429` is the
    // troubleshooting page, and `spill_limit` is a config key.
    for (query, expected) in [("429", "troubleshooting"), ("spill_limit", "config")] {
        let run = run_task(&mock, &["docs", "--find", query], &[], None, "").await;
        assert_eq!(run.code, 0, "{query}: {}", run.stderr);
        let first = run.stdout.lines().next().unwrap_or_default();
        assert!(first.starts_with(expected), "{query}: {}", run.stdout);
    }
    // A two-word query lists every file that holds a term, best first.
    let run = run_task(&mock, &["docs", "--find", "retry backoff"], &[], None, "").await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("providers"), "{}", run.stdout);
    assert!(run.stdout.contains("config"), "{}", run.stdout);
    // Every hit names the file and the lines that matched, with line numbers.
    assert!(run.stdout.contains("  "), "{}", run.stdout);
    let no_hit = run_task(
        &mock,
        &["docs", "--find", "zzzz-no-such-word"],
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
async fn generate_is_idempotent_and_check_catches_a_stale_block() {
    let mock = Mock::start(vec![]).await;
    let (_temp, root) = reference_copy();
    let root_env = root.to_string_lossy().into_owned();
    let env = [("KELIR_DOCS_ROOT", root_env.as_str())];

    // A clean copy: nothing to write, and `--check` is green.
    let run = run_task(&mock, &["docs", "--generate"], &env, None, "").await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("nothing to generate"), "{}", run.stdout);
    let check = run_task(&mock, &["docs", "--check"], &env, None, "").await;
    assert_eq!(check.code, 0, "{}", check.stderr);
    assert!(check.stdout.contains("up to date"), "{}", check.stdout);
    let topics = listed(
        &run_task(&mock, &["docs", "--list"], &[], None, "")
            .await
            .stdout,
    )
    .len();
    assert!(
        check.stdout.contains(&topics.to_string()),
        "the count of topics is in the message: {}",
        check.stdout
    );

    // Tamper with a generated row: `--check` fails and names the file, and
    // `--generate` puts it back byte for byte.
    let verbs = root.join("verbs.md");
    let original = std::fs::read_to_string(&verbs).unwrap();
    let tampered = original.replace("| `later` |", "| `later` (edited) |");
    assert_ne!(tampered, original, "the verbs table has a `later` row");
    std::fs::write(&verbs, &tampered).unwrap();
    let check = run_task(&mock, &["docs", "--check"], &env, None, "").await;
    assert_eq!(check.code, 1, "a stale block must fail: {}", check.stdout);
    assert!(check.stderr.contains("verbs.md"), "{}", check.stderr);
    assert!(check.stderr.contains("--generate"), "{}", check.stderr);
    let fixed = run_task(&mock, &["docs", "--generate"], &env, None, "").await;
    assert_eq!(fixed.code, 0, "{}", fixed.stderr);
    assert!(fixed.stdout.contains("wrote"), "{}", fixed.stdout);
    assert_eq!(std::fs::read_to_string(&verbs).unwrap(), original);
    // And a second run has nothing to do: the generator is deterministic.
    let again = run_task(&mock, &["docs", "--generate"], &env, None, "").await;
    assert_eq!(again.code, 0);
    assert!(
        again.stdout.contains("nothing to generate"),
        "{}",
        again.stdout
    );

    // Prose around the markers survives a generate.
    let providers = root.join("providers.md");
    let before = std::fs::read_to_string(&providers).unwrap();
    std::fs::write(&providers, format!("{before}\nHAND WRITTEN TAIL\n")).unwrap();
    let run = run_task(&mock, &["docs", "--generate"], &env, None, "").await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        std::fs::read_to_string(&providers)
            .unwrap()
            .contains("HAND WRITTEN TAIL")
    );
}

#[tokio::test]
async fn generate_and_check_are_different_jobs_and_both_need_a_checkout() {
    let mock = Mock::start(vec![]).await;
    let both = run_task(&mock, &["docs", "--generate", "--check"], &[], None, "").await;
    assert_eq!(both.code, 2, "{}", both.stderr);
    assert!(both.stderr.contains("pick one"), "{}", both.stderr);

    // No `docs/reference` under the cwd and no `KELIR_DOCS_ROOT`: the message
    // names both ways out rather than doing nothing.
    for args in [vec!["docs", "--generate"], vec!["docs", "--check"]] {
        let run = run_task(&mock, &args, &[], None, "").await;
        assert_eq!(run.code, 1, "{args:?}: {}", run.stderr);
        assert!(
            run.stderr.contains("docs/reference"),
            "{:?}: {}",
            args,
            run.stderr
        );
        assert!(
            run.stderr.contains("KELIR_DOCS_ROOT"),
            "{args:?}: {}",
            run.stderr
        );
    }
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
    let description = kelir::sources::parse_skill(kelir::sources::BUILTIN_SKILL)
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
