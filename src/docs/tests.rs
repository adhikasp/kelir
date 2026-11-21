//! The drift test: the code and `docs/reference/` must agree, in **both**
//! directions.
//!
//! The generated blocks ([`super::check`]) prove that the files on disk are
//! what the generators produce. That is one direction only: it cannot see a
//! verb, a key, an event type or an environment variable that exists in the
//! code and nowhere in `tables.rs`/`envs.rs`, because a name missing from the
//! table produces no row at all. So these tests also read the *source*: the
//! files are `include_str!`-ed at compile time, parsed with enough care to
//! collect the names, and compared as sets with the tables. A name on one side
//! only is a failure that says which side is missing it.

use super::*;

// The code the reference is generated from, as it is compiled. `include_str!`
// rather than a runtime read keeps the test independent of the cwd, and makes a
// moved file a compile error instead of a silent skip.
const VERBS_SRC: &str = include_str!("../verbs.rs");
const CONFIG_SRC: &str = include_str!("../config.rs");
const EVENT_SRC: &str = include_str!("../event.rs");
const PROMPT: &str = include_str!("../prompt.md");
const SKILL: &str = include_str!("skill-kelir.md");

// ------------------------------------------------------------------ the verbs

/// The verb names `verbs::parse` accepts, read from its own match arms.
fn parsed_verbs(src: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut inside = false;
    for line in src.lines() {
        if line.trim_start().starts_with("pub fn parse(") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        // The catch-all arm ends the match (it is at the match's own
        // indentation: a nested one, inside `skills`, is deeper); every arm
        // above it is a verb.
        if line.len() - line.trim_start().len() == 8 && line.trim_start().starts_with("other =>") {
            break;
        }
        if let Some(name) = arm_name(line) {
            names.push(name);
        }
    }
    names
}

/// `        "say" => {` (or `=> Ok(…`) on a line of its own: the arm's string.
fn arm_name(line: &str) -> Option<String> {
    // The arms of `parse`'s outer match sit at eight spaces; a nested match
    // (the `kind` of a `skills` verb) is deeper and is not a verb name.
    if line.len() - line.trim_start().len() != 8 {
        return None;
    }
    let rest = line.trim_start();
    let rest = rest.strip_prefix('"')?;
    let (name, after) = rest.split_once('"')?;
    after
        .trim_start()
        .starts_with("=>")
        .then(|| name.to_string())
}

#[test]
fn every_verb_the_code_parses_is_in_the_table_and_the_other_way() {
    let mut code = parsed_verbs(VERBS_SRC);
    code.sort();
    let mut table: Vec<String> = tables::VERBS
        .iter()
        .map(|verb| verb.name.to_string())
        .collect();
    table.sort();
    assert!(!code.is_empty(), "no verb arm was found in src/verbs.rs");
    for name in &code {
        assert!(
            table.contains(name),
            "`{name}` is parsed in src/verbs.rs but has no row in docs/tables.rs: \
             add a VerbDoc for it (docs/reference/verbs.md is generated from the table)"
        );
    }
    for name in &table {
        assert!(
            code.contains(name),
            "docs/tables.rs documents the verb `{name}`, but verbs::parse does not accept it: \
             a documented verb that does not exist is a wrong instruction"
        );
    }
}

/// The table's `applies` column against the code's real boundary: a verb the
/// table says applies `next_step` must not be applied at `turn_end`.
#[test]
fn every_verbs_row_says_where_the_code_applies_it() {
    for row in tables::VERBS {
        // Build the smallest args `parse` accepts for the verb, then ask the
        // code where it applies. No `bg` row: its args are a job status file,
        // which is the session's own bookkeeping (covered by src/verbs.rs).
        let args = match row.name {
            "say" | "later" => serde_json::json!({"text": "x"}),
            "model" => serde_json::json!({"model": "openrouter/test/model"}),
            "tools" => serde_json::json!({"disable": "bash"}),
            "skills" => serde_json::json!({"name": "x", "text": "x"}),
            "view" => serde_json::json!({"path": "x.png"}),
            _ => serde_json::json!({}),
        };
        let Ok(verb) = crate::verbs::parse(row.name, &args) else {
            continue;
        };
        let scope = verb.scope();
        assert!(
            row.applies.contains(scope),
            "docs/tables.rs says `{}` applies at {:?}, the code says {scope}",
            row.name,
            row.applies
        );
    }
}

// ------------------------------------------------------------ the config keys

/// The field names of `struct <name> { … }` in `src/config.rs`.
fn struct_fields(src: &str, name: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut inside = false;
    for line in src.lines() {
        if !inside {
            if line.starts_with(&format!("struct {name} {{")) {
                inside = true;
            }
            continue;
        }
        if line == "}" {
            break;
        }
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.is_empty() {
            continue;
        }
        let Some((field, _)) = trimmed.split_once(':') else {
            continue;
        };
        fields.push(field.trim().trim_start_matches("pub ").to_string());
    }
    fields
}

#[test]
fn every_config_key_the_code_reads_is_in_the_table_and_the_other_way() {
    for (struct_name, scope) in [
        ("RawConfig", tables::KeyScope::Top),
        ("RawProvider", tables::KeyScope::Provider),
    ] {
        let mut code = struct_fields(CONFIG_SRC, struct_name);
        code.sort();
        assert!(
            !code.is_empty(),
            "no field was found in `struct {struct_name}`"
        );
        let mut table: Vec<String> = tables::CONFIG_KEYS
            .iter()
            .filter(|key| key.scope == scope)
            .map(|key| key.name.to_string())
            .collect();
        table.sort();
        let where_ = match scope {
            tables::KeyScope::Top => "`RawConfig`, the top level",
            tables::KeyScope::Provider => "`RawProvider`, the `[providers.<name>]` fields",
        };
        for name in &code {
            assert!(
                table.contains(name),
                "`{name}` is a field of {where_} but has no row in docs/tables.rs, so the \
                 reference does not document it"
            );
        }
        for name in &table {
            assert!(
                code.contains(name),
                "docs/tables.rs documents `{name}` for {where_}, which does not have that \
                 field: a key nobody reads is a wrong instruction"
            );
        }
    }
    // The source pointer has to name a function that exists: the column is
    // there to be `git grep`-ed.
    for key in tables::CONFIG_KEYS {
        assert!(
            CONFIG_SRC.contains(&format!("fn {}(", key.field_fn)),
            "`{}` points at `src/config.rs:{}`, which is not a function there",
            key.name,
            key.field_fn
        );
    }
}

// ------------------------------------------------------------- the event types

/// The event names `EventBody::event_type()` returns, read from its arms.
fn event_names(src: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut inside = false;
    for line in src.lines() {
        if line.trim_start().starts_with("pub fn event_type(&self)") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        let trimmed = line.trim();
        if trimmed == "}" {
            break;
        }
        if let Some((_, name)) = trimmed.split_once("=> ") {
            let name = name.trim().trim_end_matches(',');
            if let Some(name) = name.strip_prefix('"').and_then(|n| n.strip_suffix('"')) {
                names.push(name.to_string());
            }
        }
    }
    names
}

#[test]
fn every_event_type_the_code_writes_is_in_the_table_and_the_other_way() {
    let mut code = event_names(EVENT_SRC);
    code.sort();
    let mut table: Vec<String> = tables::EVENTS
        .iter()
        .map(|event| event.name.to_string())
        .collect();
    table.sort();
    assert_eq!(
        code.len(),
        code.iter().collect::<std::collections::BTreeSet<_>>().len(),
        "an event type is written twice: {code:?}"
    );
    for name in &code {
        assert!(
            table.contains(name),
            "`{name}` is written to events.jsonl but has no row in docs/tables.rs \
             (docs/reference/events.md is generated from the table)"
        );
    }
    for name in &table {
        assert!(
            code.contains(name),
            "docs/tables.rs documents the event `{name}`, which `EventBody::event_type()` \
             never returns"
        );
    }
}

// ------------------------------------------------------- the environment table

#[test]
fn every_variable_the_code_names_is_declared_and_the_other_way() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut names: Vec<(String, String)> = Vec::new();
    walk_sources(&root, &mut |path, text| {
        for (number, line) in text.lines().enumerate() {
            // A quoted literal that *looks* like one of the variables kelir
            // owns. The scan is on strings, not on calls, so a variable kelir
            // only *sets* (`command.env("KELIR_SOCK", …)`) is caught too, and
            // so is a name behind a constant.
            for name in quoted_variables(line) {
                names.push((format!("{}:{}", path.display(), number + 1), name));
            }
        }
    });
    // One direction: every kelir-owned name in the code is in the registry.
    for (where_, name) in &names {
        if !owned_family(name) {
            continue;
        }
        assert!(
            crate::envs::declared(name).is_some(),
            "{where_}: `{name}` is read or set here but is not in `envs::VARS`, so \
             docs/reference/env.md does not document it"
        );
    }
    // The other way: a declared variable no code outside the registry names is
    // either dead or a typo, and either way a wrong row in the docs.
    for var in crate::envs::VARS {
        let used = names
            .iter()
            .any(|(where_, name)| name == var.name && !where_.contains("envs.rs"));
        assert!(
            used,
            "`envs::VARS` declares `{}` ({:?}), but no code outside envs.rs names it",
            var.name, var.dir
        );
    }
}

/// Is this one of the namespaces kelir owns, so that any name in it has to be
/// documented? `KELIR_*` and `HC_*` (Herdr's pane variables) are; `PATH`,
/// `HOME` and `TERM` are the platform's, and are in the table because the code
/// reads them, which the other direction checks.
fn owned_family(name: &str) -> bool {
    ["KELIR_", "HC_"]
        .iter()
        .any(|prefix| name.starts_with(prefix) && name.len() > prefix.len())
}

/// The quoted all-caps literals in one line of source (`"KELIR_SOCK"`, and
/// nothing that is not a variable-shaped word).
fn quoted_variables(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('"') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('"') else {
            break;
        };
        let word = &after[..close];
        let shaped = word.len() > 2
            && !word.starts_with(|c: char| c.is_ascii_digit())
            && word
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if shaped {
            out.push(word.to_string());
        }
        rest = &after[close + 1..];
    }
    out
}

fn walk_sources(dir: &std::path::Path, visit: &mut dyn FnMut(&std::path::Path, &str)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_sources(&path, visit);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            visit(&path, &text);
        }
    }
}

// --------------------------------------------------------------- the files

#[test]
fn the_reference_on_disk_is_what_the_generators_produce() {
    // Away from a checkout there is nothing to compare: the embedded copy is
    // what a session reads, and `--check` has no file to look at.
    if docs_root().is_none() {
        eprintln!("no docs/reference here: skipping the on-disk check");
        return;
    }
    let problems = check();
    assert!(
        problems.is_empty(),
        "the reference is stale; run `kelir docs --generate`:\n{}",
        problems.join("\n")
    );
}

#[test]
fn every_topic_is_embedded_opens_with_its_summary_and_matches_its_file() {
    assert!(!TOPICS.is_empty(), "no topics");
    for entry in TOPICS {
        assert!(
            entry.body.starts_with("# "),
            "{}: the file does not start with a `# ` heading",
            entry.name
        );
        // The three-line opening the reference promises: what it answers and
        // when to read it, both in bold so the agent can skim.
        assert!(
            entry.body.contains("**Summary:**"),
            "{}: no `**Summary:**` line",
            entry.name
        );
        assert!(
            entry.body.contains("**Read this when:**"),
            "{}: no `**Read this when:**` line",
            entry.name
        );
        assert!(
            !entry.summary.contains('\n') && entry.summary.len() > 20,
            "{}: the one-line summary of --list is missing or too short: {:?}",
            entry.name,
            entry.summary
        );
        assert_eq!(
            topic(entry.name).map(|found| found.name),
            Some(entry.name),
            "{}: `topic()` does not find it by its own name",
            entry.name
        );
        assert_eq!(
            topic(&format!("{}.md", entry.name)).map(|found| found.name),
            Some(entry.name),
            "{}: `topic()` does not accept the `.md` suffix",
            entry.name
        );
    }
    // The embedded copy is the file: a `--generate` edits the file, and the
    // binary must ship what the file says (a stale embed is a docs bug that
    // only a rebuild could reveal).
    if let Some(root) = docs_root() {
        for entry in TOPICS {
            let path = root.join(format!("{}.md", entry.name));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("{}: missing", path.display()));
            assert_eq!(
                text,
                entry.body,
                "{}: the embedded copy differs from the file (rebuild after `--generate`)",
                path.display()
            );
            let listed: Vec<String> = std::fs::read_dir(&root)
                .unwrap()
                .flatten()
                .filter_map(|entry| entry.path().file_name()?.to_str().map(str::to_string))
                .filter(|name| name.ends_with(".md"))
                .collect();
            // The other way: a file in `docs/reference` that no topic embeds
            // is a document no session can read.
            for name in listed {
                let stem = name.trim_end_matches(".md");
                assert!(
                    topic(stem).is_some(),
                    "docs/reference/{name} is not in `TOPICS`, so `kelir docs {stem}` does not \
                     exist and no binary carries it"
                );
            }
        }
    }
}

#[test]
fn every_table_row_reaches_its_topic() {
    for (kind, name) in [(0, "verbs"), (1, "config"), (2, "events")] {
        let body = topic(name).expect("the topic exists").body;
        let names: Vec<&str> = match kind {
            0 => tables::VERBS.iter().map(|verb| verb.name).collect(),
            1 => tables::CONFIG_KEYS.iter().map(|key| key.name).collect(),
            _ => tables::EVENTS.iter().map(|event| event.name).collect(),
        };
        for name in names {
            assert!(
                body.contains(&format!("`{name}`")),
                "docs/reference/{}.md does not mention `{name}`: the generated block is \
                 missing or the topic stopped embedding one",
                match kind {
                    0 => "verbs",
                    1 => "config",
                    _ => "events",
                }
            );
        }
    }
    // The env table is generated from the registry, so the same per variable.
    let body = topic("env").expect("the topic exists").body;
    for var in crate::envs::VARS {
        assert!(
            body.contains(&format!("`{}`", var.name)),
            "docs/reference/env.md does not mention `{}`",
            var.name
        );
    }
}

// ------------------------------------------------------------ the generators

#[test]
fn regenerating_twice_changes_nothing_and_leaves_the_prose_alone() {
    let text = "\
# A topic

Prose that a generator must not touch.

<!-- generated:verbs -->
| old | rows |
<!-- /generated -->

More prose.

<!-- generated:env -->
<!-- /generated -->
";
    let (once, kinds) = regenerate(text).expect("regenerate");
    assert_eq!(kinds, vec!["verbs".to_string(), "env".to_string()]);
    assert!(once.contains("# A topic"), "{once}");
    assert!(
        once.contains("Prose that a generator must not touch."),
        "{once}"
    );
    assert!(once.contains("More prose."), "{once}");
    assert!(
        !once.contains("| old | rows |"),
        "the old block survived:\n{once}"
    );
    assert!(
        once.contains(&render_verbs().trim_end().to_string()),
        "the verbs table is not in the block"
    );
    let (twice, _) = regenerate(&once).expect("regenerate again");
    assert_eq!(once, twice, "a second run is not idempotent");
}

#[test]
fn an_unknown_or_unclosed_generated_block_is_an_error() {
    let error = regenerate("<!-- generated:nonsense -->\n<!-- /generated -->\n").unwrap_err();
    assert!(error.contains("nonsense"), "{error}");
    let error = regenerate("<!-- generated:verbs -->\nno end\n").unwrap_err();
    assert!(error.contains("no <!-- /generated -->"), "{error}");
    // A marker mentioned in prose (the index explains the format) is text: only
    // a line that is exactly a marker opens a block.
    let prose = "Write <!-- generated:verbs --> in the file to place the table.\n";
    let (same, kinds) = regenerate(prose).expect("prose is not a block");
    assert_eq!(same, prose);
    assert!(kinds.is_empty(), "{kinds:?}");
}

#[test]
fn every_generator_is_named_and_renders_something() {
    for kind in GENERATORS {
        let rendered = generate(kind).unwrap_or_else(|| panic!("{kind}: no generator"));
        assert!(!rendered.trim().is_empty(), "{kind} renders nothing");
        // Tables, not prose: a block's cells are rows, with a heading or the
        // one-line `about` of a command above each table.
        // A cell may not hold a raw newline or an unescaped pipe: either would
        // split the row and lose a column.
        for line in rendered.lines().filter(|line| line.starts_with('|')) {
            assert!(line.ends_with('|'), "{kind}: row does not end: {line}");
            assert!(!line.contains(" | |"), "{kind}: an empty cell in {line}");
        }
    }
    assert!(generate("nonsense").is_none());
    // `GENERATORS` is the list every marker may name, and nothing else.
    for entry in TOPICS {
        let mut rest = entry.body;
        while let Some(open) = rest.find(START_MARKER) {
            let after = &rest[open + START_MARKER.len()..];
            let Some(close) = after.find("-->") else {
                panic!("{}: an unterminated marker", entry.name);
            };
            let kind = after[..close].trim();
            // A marker on its own line is a block; the same text inside a
            // code block is documentation of the format.
            let line_start = rest[..open].rfind('\n').map(|index| index + 1).unwrap_or(0);
            let own_line = rest[line_start..open].trim().is_empty();
            if own_line && !kind.is_empty() && !kind.contains(' ') {
                assert!(
                    GENERATORS.contains(&kind),
                    "{}: the marker names `{kind}`, which is not a generator",
                    entry.name
                );
            }
            rest = &after[close..];
        }
    }
}

#[test]
fn the_cli_renderer_covers_every_visible_subcommand() {
    use clap::CommandFactory as _;
    let rendered = render_cli();
    let cli = crate::cli::Cli::command();
    let mut visible = 0;
    for sub in cli.get_subcommands() {
        let name = sub.get_name();
        if sub.is_hide_set() {
            assert!(
                !rendered.contains(&format!("### `kelir {name}`")),
                "a hidden command is documented: {name}"
            );
            continue;
        }
        visible += 1;
        assert!(
            rendered.contains(&format!("### `kelir {name}`")),
            "`kelir {name}` is a subcommand but has no section in docs/reference/cli.md"
        );
        for arg in sub.get_arguments() {
            let id = arg.get_id().as_str();
            if id == "help" || id == "version" {
                continue;
            }
            let spelled = match (arg.get_long(), arg.get_short()) {
                (Some(long), _) => format!("`--{long}`"),
                (None, Some(short)) => format!("`-{short}`"),
                (None, None) => continue,
            };
            assert!(
                rendered.contains(&spelled),
                "`kelir {name}` takes {spelled}, which its docs section does not list"
            );
            assert!(
                arg.get_help().is_some(),
                "`kelir {name} {spelled}` has no help line, so the docs row would be empty"
            );
        }
    }
    assert!(
        visible >= 8,
        "only {visible} visible subcommands were found"
    );
}

// ------------------------------------------------------------------ the search

#[test]
fn find_ranks_the_topic_that_answers_the_question() {
    for (query, expected) in [
        ("retry backoff", "providers"),
        ("max_steps cap", "config"),
        ("socket verb", "verbs"),
        ("events.jsonl blobs", "files"),
        ("NO_COLOR", "env"),
    ] {
        let hits = find(query);
        assert!(!hits.is_empty(), "{query}: no hit at all");
        assert!(
            hits.iter().any(|hit| hit.topic == expected),
            "{query}: {expected} is not in {:?}",
            hits.iter().map(|hit| hit.topic).collect::<Vec<_>>()
        );
        // Every hit quotes the line it matched, so the model can judge before
        // reading the file.
        for hit in &hits {
            assert!(!hit.lines.is_empty(), "{}: a hit with no lines", hit.topic);
            for (number, line) in &hit.lines {
                assert!(*number >= 1, "{}: line numbers are 1-based", hit.topic);
                assert!(!line.is_empty(), "{}: an empty match line", hit.topic);
            }
        }
        // The ranking is deterministic.
        let again: Vec<&str> = find(query).iter().map(|hit| hit.topic).collect();
        let first: Vec<&str> = hits.iter().map(|hit| hit.topic).collect();
        assert_eq!(first, again, "{query}: the ranking is not deterministic");
    }
    assert!(find("").is_empty());
    assert!(find("   ").is_empty());
    // A word no topic holds is no hit, not a panic.
    assert!(find("zzzz-no-such-word").is_empty());
}

#[test]
fn a_misspelt_topic_suggests_the_one_that_was_meant() {
    let close = suggestions("confg");
    assert_eq!(close.first(), Some(&"config"), "{close:?}");
    // A prefix is enough, and so is a longer word that starts with a topic.
    assert!(suggestions("troub").contains(&"troubleshooting"));
    assert!(suggestions("config-file").contains(&"config"));
    // Nothing close: no suggestion rather than a wrong one.
    assert!(suggestions("zzzz").is_empty(), "{:?}", suggestions("zzzz"));
    assert!(suggestions("").is_empty());
    // The last fallback: a word that is in a body and nowhere in a name.
    assert_eq!(suggestions("blobs").first(), Some(&"files"));
}

#[test]
fn occurrences_counts_case_insensitively() {
    assert_eq!(occurrences("retry", "Retry, then RETRY"), 2);
    assert_eq!(occurrences("", "anything"), 0);
}

// -------------------------------------------------------------------- budget

/// The footprint of the embedded reference, reported so a growth is a decision
/// rather than an accident (a binary that carries 300 KB of prose is a
/// different footprint from one that carries 60 KB).
#[test]
fn the_embedded_reference_fits_its_size_budget() {
    let body: usize = TOPICS.iter().map(|topic| topic.body.len()).sum();
    let skill = SKILL.len();
    eprintln!(
        "embedded reference: {body} bytes in {} topics, skill {skill} bytes, total {} bytes",
        TOPICS.len(),
        body + skill
    );
    for entry in TOPICS {
        assert!(
            entry.body.len() <= 32 * 1024,
            "{} is {} bytes: split it into two topics",
            entry.name,
            entry.body.len()
        );
    }
    // 128 KB: room for the reference to grow, small enough that it is a
    // rounding error next to the binary's budget.
    // M10 adds the channel topic and five config keys; 140 KB is still a
    // rounding error next to the binary's 20 MB budget (measured: ~135 KB).
    assert!(body <= 140 * 1024, "the reference is {body} bytes");
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

// ------------------------------------------------------------- the base prompt

#[test]
fn the_base_prompt_names_kelir_docs_within_its_budget() {
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

// ------------------------------------------- values, not only names (steer11fix)

/// The documented default of a key, as the table words it.
fn default_of(key: &str) -> &'static str {
    tables::CONFIG_KEYS
        .iter()
        .find(|row| row.scope == tables::KeyScope::Top && row.name == key)
        .unwrap_or_else(|| panic!("no row for `{key}`"))
        .default
}

/// A default that changes in the code but not in the table is a wrong
/// instruction: the numbers are read from the running code, not retyped.
#[test]
fn the_documented_defaults_are_the_ones_the_code_applies() {
    let dir = std::env::temp_dir().join(format!("kelir-docs-defaults-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "").unwrap();
    let cfg = crate::config::load(Some(&path)).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let checks: Vec<(&str, String)> = vec![
        ("spill_limit", cfg.spill_limit.to_string()),
        ("compact_keep", crate::compact::DEFAULT_KEEP.to_string()),
        ("compact_keep", cfg.compact_keep.to_string()),
        ("window", crate::compact::DEFAULT_WINDOW.to_string()),
        ("auto_compact_at", crate::compact::AUTO_FRACTION.to_string()),
        (
            "compact_at_tokens",
            crate::compact::DEFAULT_SOFT_TARGET.to_string(),
        ),
        ("promote_after", cfg.promote_after.as_secs().to_string()),
        ("retry_base_ms", crate::retry::DEFAULT_BASE_MS.to_string()),
        ("retry_cap_ms", crate::retry::DEFAULT_CAP_MS.to_string()),
        (
            "retry_max_wait",
            crate::retry::DEFAULT_WAIT_SECS.to_string(),
        ),
        ("provider", cfg.provider.clone()),
        ("model", cfg.model.clone()),
    ];
    for (key, value) in checks {
        assert!(
            default_of(key).contains(&value),
            "docs/tables.rs says the default of `{key}` is {:?}, the code applies `{value}`",
            default_of(key)
        );
    }
}

/// A `rename`/`alias` on a config field would make the key the user writes
/// differ from the field name the table is checked against.
#[test]
fn config_fields_are_not_renamed_behind_the_tables_back() {
    for (name, src) in [("RawConfig", CONFIG_SRC), ("RawProvider", CONFIG_SRC)] {
        let mut inside = false;
        for line in src.lines() {
            if line.starts_with(&format!("struct {name} {{")) {
                inside = true;
                continue;
            }
            if inside && line == "}" {
                break;
            }
            if inside {
                let t = line.trim();
                assert!(
                    !(t.starts_with("#[serde(") && (t.contains("rename") || t.contains("alias"))),
                    "`{name}` renames a key (`{t}`): the drift test reads field names, teach it \
                     the wire name first"
                );
            }
        }
    }
}

/// `field` names of every `EventBody` variant, by its snake_case event name.
fn event_fields(src: &str) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut out = std::collections::BTreeMap::new();
    let mut inside = false;
    let mut current: Option<String> = None;
    let mut rename: Option<String> = None;
    for line in src.lines() {
        if line.starts_with("pub enum EventBody {") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if line == "}" {
            break;
        }
        let t = line.trim();
        if t.starts_with("//") || t.is_empty() {
            continue;
        }
        if let Some(r) = t.strip_prefix("#[serde(rename = \"") {
            rename = r.split('"').next().map(str::to_string);
            continue;
        }
        if t.starts_with("#[") {
            continue;
        }
        if line.starts_with("    ") && !line.starts_with("     ") {
            // `Name {` (or `Name { a: T },`) opens a variant; `},` closes one.
            if t == "}," {
                current = None;
                continue;
            }
            let name = t.split([' ', '{']).next().unwrap();
            let mut snake = String::new();
            for (i, ch) in name.chars().enumerate() {
                if ch.is_uppercase() && i > 0 {
                    snake.push('_');
                }
                snake.push(ch.to_ascii_lowercase());
            }
            out.insert(snake.clone(), Vec::new());
            current = Some(snake);
            continue;
        }
        if let (Some(variant), Some((field, _))) = (&current, t.split_once(':')) {
            let field = rename.take().unwrap_or_else(|| field.trim().to_string());
            out.get_mut(variant).unwrap().push(field);
        }
    }
    out
}

/// The field names a table cell promises: every backticked span's leading
/// identifier (`system{text,hash}` → `system`, `parent?` → `parent`).
fn promised_fields(cell: &str) -> Vec<String> {
    cell.split('`')
        .skip(1)
        .step_by(2)
        .map(|span| {
            span.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

#[test]
fn every_event_field_the_code_writes_is_in_the_table_and_the_other_way() {
    let code = event_fields(EVENT_SRC);
    assert!(code.len() >= 16, "parsed {} variants", code.len());
    for row in tables::EVENTS {
        let mut table = promised_fields(row.fields);
        table.sort();
        let mut fields = code
            .get(row.name)
            .unwrap_or_else(|| panic!("no EventBody variant for `{}`", row.name))
            .clone();
        fields.sort();
        assert_eq!(
            fields, table,
            "event `{}`: the fields in `EventBody` (left) and docs/tables.rs (right) differ",
            row.name
        );
    }
}

// -------------------------------------------------- the delegation and herdr skills

#[test]
fn the_delegation_and_herdr_skills_are_valid_short_and_say_what_they_must() {
    for (name, text) in crate::sources::BUILTIN_SKILLS {
        let skill = crate::sources::parse_skill(text).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(skill.name, name);
        assert!(!skill.description.is_empty(), "{name} has no description");
        assert!(
            text.len() <= 6 * 1024,
            "{name} is {} bytes: keep skills short",
            text.len()
        );
        // No private names, hosts or paths in a public repo's embedded text.
        for banned in [
            "the maintainer",
            "the maintainer",
            "a Linux box",
            "a build box",
            "users/",
        ] {
            assert!(
                !text
                    .to_ascii_lowercase()
                    .contains(&banned.to_ascii_lowercase()),
                "{name}: {banned}"
            );
        }
    }
    let delegation = crate::sources::BUILTIN_SKILLS[1].1;
    for needle in [
        "kelir run --child",
        "--background",
        "kelir brief",
        "kelir ls",
        "kelir kill",
        "kelir docs children",
    ] {
        assert!(
            delegation.contains(needle),
            "delegation does not mention {needle}"
        );
    }
    let herdr = crate::sources::BUILTIN_SKILLS[2].1;
    for needle in [
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
    ] {
        assert!(herdr.contains(needle), "herdr does not mention {needle}");
    }
}

// ------------------------------------------- a skill names only commands that exist

/// Every `kelir …` command a skill shows in a code span or block, as the words
/// after `kelir` that look like a subcommand path, and the `--flags` that follow
/// on the same command.
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
