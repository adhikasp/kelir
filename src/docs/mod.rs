//! `kelir docs`: the reference, embedded in the binary and readable by the
//! agent.
//!
//! Every file in `docs/reference/` is compiled in with `include_str!`, so the
//! model can read how to operate kelir from inside a session that has no repo
//! on disk. Three of those files carry a *generated* block (the CLI from clap,
//! the verbs and event types from [`tables`], the environment from
//! [`crate::envs::VARS`]) between markers; `kelir docs --generate` rewrites
//! those blocks, `--check` fails when one is stale, and the drift test
//! ([`tests`]) also reads the *code* so a flag, verb, key, variable or event
//! that exists in one and not the other is an error, in both directions.
//!
//! The hand-written prose stays where it is: only what a machine can know for
//! certain is generated.

pub mod tables;

#[cfg(test)]
mod tests;

use std::fmt::Write as _;

/// One reference topic: the file, its title, and the words a session needs to
/// find it.
#[derive(Debug, Clone, Copy)]
pub struct Topic {
    /// The name `kelir docs <topic>` takes (the file name without `.md`).
    pub name: &'static str,
    /// One line: what the file answers, for the list and the search.
    pub summary: &'static str,
    /// The shipped text, embedded (`docs/reference/<name>.md`).
    pub body: &'static str,
}

/// The topics, in the order `kelir docs --list` prints them: an index, then
/// the reference files, most-read first.
pub const TOPICS: &[Topic] = &[
    Topic {
        name: "README",
        summary: "the index: what is documented where, and how to read it",
        body: include_str!("../../docs/reference/README.md"),
    },
    Topic {
        name: "config",
        summary: "every `config.toml` key: type, default, override, effect, source pointer",
        body: include_str!("../../docs/reference/config.md"),
    },
    Topic {
        name: "cli",
        summary: "every subcommand and flag, generated from clap",
        body: include_str!("../../docs/reference/cli.md"),
    },
    Topic {
        name: "env",
        summary: "every environment variable kelir reads or sets, generated from `src/envs.rs`",
        body: include_str!("../../docs/reference/env.md"),
    },
    Topic {
        name: "verbs",
        summary: "every `kelir ctl` verb: arguments, when it applies, what it writes",
        body: include_str!("../../docs/reference/verbs.md"),
    },
    Topic {
        name: "mcp",
        summary: "`mcp.toml` and `kelir mcp`: servers, commands, exit codes, the session broker, channels",
        body: include_str!("../../docs/reference/mcp.md"),
    },
    Topic {
        name: "children",
        summary: "`kelir run --child`, `brief`, `ls`, `kill`: delegating to other sessions, the caps, the tree",
        body: include_str!("../../docs/reference/children.md"),
    },
    Topic {
        name: "providers",
        summary: "the two providers: keys, retries, limits, model metadata",
        body: include_str!("../../docs/reference/providers.md"),
    },
    Topic {
        name: "sources-and-skills",
        summary: "instruction files, skills directories, hints, memory, attachments",
        body: include_str!("../../docs/reference/sources-and-skills.md"),
    },
    Topic {
        name: "files",
        summary: "the state dir, a session dir, the event log, blobs, bg, locks, config search",
        body: include_str!("../../docs/reference/files.md"),
    },
    Topic {
        name: "events",
        summary: "every event type in `events.jsonl` and the fields a reader can count on",
        body: include_str!("../../docs/reference/events.md"),
    },
    Topic {
        name: "tui",
        summary: "the interactive session: rows, keys, `/` commands, status states",
        body: include_str!("../../docs/reference/tui.md"),
    },
    Topic {
        name: "troubleshooting",
        summary: "429s, a paused turn, a stale socket, a lost job, Windows console quirks",
        body: include_str!("../../docs/reference/troubleshooting.md"),
    },
    Topic {
        name: "source-map",
        summary: "which source file owns what, for reading the code next",
        body: include_str!("../../docs/reference/source-map.md"),
    },
];

/// The topic named `name` (a file name, with or without `.md`).
pub fn topic(name: &str) -> Option<&'static Topic> {
    let name = name.strip_suffix(".md").unwrap_or(name);
    TOPICS
        .iter()
        .find(|topic| topic.name.eq_ignore_ascii_case(name))
}

/// The closest topic names for an unknown one, for the suggestion line. Two
/// signals, cheapest first: a name that starts with what was typed, then a
/// shared prefix; failing both, the terms that appear most often in the
/// reference's own bodies.
pub fn suggestions(name: &str) -> Vec<&'static str> {
    let wanted = name.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        return Vec::new();
    }
    let mut ranked: Vec<(u32, &'static str)> = TOPICS
        .iter()
        .map(|topic| {
            let topic_name = topic.name.to_ascii_lowercase();
            let score = if topic_name.starts_with(&wanted) || wanted.starts_with(&topic_name) {
                3
            } else if topic_name.contains(&wanted) || wanted.contains(&topic_name) {
                2
            } else {
                common_prefix(&topic_name, &wanted)
            };
            (score, topic.name)
        })
        .filter(|(score, _)| *score > 1)
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    if ranked.is_empty() {
        // No name is close: the word may still be *in* a body, which is the
        // last signal (`blobs` -> `files`).
        return by_occurrences(&wanted);
    }
    ranked.into_iter().map(|(_, name)| name).take(3).collect()
}

/// The topics whose bodies hold `word` most often, best first.
fn by_occurrences(word: &str) -> Vec<&'static str> {
    let mut ranked: Vec<(usize, &'static str)> = TOPICS
        .iter()
        .map(|topic| (occurrences(word, topic.body), topic.name))
        .filter(|(count, _)| *count > 0)
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    ranked.into_iter().map(|(_, name)| name).take(2).collect()
}

fn common_prefix(a: &str, b: &str) -> u32 {
    a.chars().zip(b.chars()).take_while(|(a, b)| a == b).count() as u32
}

/// How many times `word` occurs in the topic's body (case-insensitive): the
/// last signal [`suggestions`] falls back on, so `blobs` finds `files`.
pub fn occurrences(word: &str, body: &str) -> usize {
    if word.is_empty() {
        return 0;
    }
    body.to_ascii_lowercase()
        .matches(&word.to_ascii_lowercase())
        .count()
}

/// One search hit: the topic, and the lines of its body that matched most.
#[derive(Debug, Clone)]
pub struct Hit {
    pub topic: &'static str,
    pub summary: &'static str,
    /// `(1-based line number, the line)` for the best few matches.
    pub lines: Vec<(usize, String)>,
    /// The score the ranking used.
    pub score: usize,
}

/// Ranked search over the reference: a hit is a body line holding a term, and
/// the score is the number of terms it holds (word boundaries count double,
/// because `key` in prose is weaker than `key_cmd`).
///
/// Deliberately simple, and documented as such: this is not full-text search,
/// it is "which file do I open".
pub fn find(query: &str) -> Vec<Hit> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|term| term.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-'))
        .filter(|term| !term.is_empty())
        .map(|term| term.to_ascii_lowercase())
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<Hit> = TOPICS
        .iter()
        .filter_map(|topic| {
            let body = topic.body.to_ascii_lowercase();
            let mut lines: Vec<(usize, String, usize)> = Vec::new();
            for (index, line) in body.lines().enumerate() {
                let mut score = 0;
                for term in &terms {
                    if line.contains(term) {
                        // A heading or a table row is worth more than prose.
                        score += if line.trim_start().starts_with('#')
                            || line.trim_start().starts_with('|')
                        {
                            3
                        } else {
                            1
                        };
                    }
                }
                if score > 0 {
                    lines.push((index + 1, line.trim().to_string(), score));
                }
            }
            if lines.is_empty() {
                return None;
            }
            // A topic that mentions every term beats one that repeats a single
            // term: coverage first, then the sum of the line scores.
            let covered = terms.iter().filter(|term| body.contains(*term)).count();
            if covered == 0 {
                return None;
            }
            let total: usize = lines.iter().map(|(_, _, score)| score).sum();
            let score = covered * 100 + total.min(99);
            let summary_score = terms
                .iter()
                .filter(|term| topic.summary.to_ascii_lowercase().contains(*term))
                .count();
            let score = score + summary_score * 50;
            lines.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
            lines.truncate(3);
            Some(Hit {
                topic: topic.name,
                summary: topic.summary,
                lines: lines
                    .into_iter()
                    .map(|(number, line, _)| (number, line))
                    .collect(),
                score,
            })
        })
        .collect();
    hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.topic.cmp(b.topic)));
    hits
}

// ------------------------------------------------------------ generated blocks

/// The marker that opens a generated block. The line after it names the
/// generator (`cli`, `verbs`, `env`); the block ends at [`END_MARKER`].
pub const START_MARKER: &str = "<!-- generated:";
/// The marker that closes a generated block.
pub const END_MARKER: &str = "<!-- /generated -->";

/// The generators, by the name a marker carries.
pub fn generate(kind: &str) -> Option<String> {
    match kind {
        "cli" => Some(render_cli()),
        "verbs" => Some(render_verbs()),
        "env" => Some(render_env()),
        "events" => Some(render_events()),
        "config" => Some(render_config()),
        _ => None,
    }
}

/// Every generator's name, in the order the docs mention them.
pub const GENERATORS: [&str; 5] = ["config", "cli", "env", "events", "verbs"];

/// Rewrite every generated block in `text`, and say which ones changed.
///
/// The prose outside the markers is untouched byte for byte, so a hand edit
/// survives a `--generate`; a block whose marker names no generator is an
/// error rather than a silent no-op.
pub fn regenerate(text: &str) -> Result<(String, Vec<String>), String> {
    let mut out = String::with_capacity(text.len());
    let mut changed: Vec<String> = Vec::new();
    let mut lines = text.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        // A marker is exactly `<!-- generated:<kind> -->` on its own line: any
        // other mention of the marker in prose (the index explains the format)
        // is text, not a block.
        if let Some(kind) = marker_kind(trimmed) {
            let Some(generated) = generate(kind) else {
                return Err(format!("unknown generator `{kind}` in {START_MARKER}"));
            };
            out.push_str(line);
            // Skip the old block: everything up to and including the end marker.
            let mut closed = false;
            for inner in lines.by_ref() {
                if inner.trim() == END_MARKER {
                    closed = true;
                    break;
                }
            }
            if !closed {
                return Err(format!("generated block `{kind}` has no {END_MARKER}"));
            }
            if !generated.is_empty() {
                out.push_str(&generated);
                if !generated.ends_with('\n') {
                    out.push('\n');
                }
            }
            out.push_str(END_MARKER);
            out.push('\n');
            changed.push(kind.to_string());
            continue;
        }
        out.push_str(line);
    }
    Ok((out, changed))
}

/// Everything wrong with the reference's generated blocks, empty when the
/// files on disk are what the generators produce. This is `--check`: CI, the
/// drift test and a pre-commit read it the same way.
pub fn check() -> Vec<String> {
    let mut problems = Vec::new();
    let Some(root) = docs_root() else {
        return vec![
            "no `docs/reference` directory here: run this from the kelir checkout, or set \
                    KELIR_DOCS_ROOT"
                .to_string(),
        ];
    };
    for topic in TOPICS {
        let path = root.join(format!("{}.md", topic.name));
        let Ok(text) = std::fs::read_to_string(&path) else {
            problems.push(format!("{}: missing", path.display()));
            continue;
        };
        match regenerate(&text) {
            Ok((expected, kinds)) => {
                if expected == text {
                    continue;
                }
                let stale: Vec<String> = kinds
                    .into_iter()
                    .filter(|kind| block(&text, kind) != block(&expected, kind))
                    .collect();
                problems.push(format!(
                    "{}: the generated block(s) {} are stale; run `kelir docs --generate`",
                    path.display(),
                    stale.join(", ")
                ));
            }
            Err(error) => problems.push(format!("{}: {error}", path.display())),
        }
    }
    problems
}

/// Where the reference files live when a developer runs `--check` or
/// `--generate`: `KELIR_DOCS_ROOT`, else `docs/reference` under the current
/// directory. `None` when neither exists, which is the normal case for a
/// binary away from its checkout (the embedded copy is what a session reads).
pub fn docs_root() -> Option<std::path::PathBuf> {
    if let Some(root) = crate::envs::var("KELIR_DOCS_ROOT") {
        let path = std::path::PathBuf::from(root);
        return path.is_dir().then_some(path);
    }
    let path = std::path::Path::new("docs").join("reference");
    path.is_dir().then_some(path)
}

/// The generator a line names, when the line is a start marker: exactly
/// `<!-- generated:<kind> -->`, with `<kind>` one word.
fn marker_kind(line: &str) -> Option<&str> {
    let rest = line.strip_prefix(START_MARKER)?;
    let kind = rest.strip_suffix("-->")?.trim();
    let plain = !kind.is_empty()
        && kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    plain.then_some(kind)
}

fn block(text: &str, kind: &str) -> String {
    let start = format!("{START_MARKER}{kind}");
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if !inside && trimmed.starts_with(&start) {
            inside = true;
            continue;
        }
        if inside {
            if trimmed == END_MARKER {
                break;
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

// --------------------------------------------------------------- the tables

fn table_header(out: &mut String, columns: &[&str]) {
    out.push('|');
    for column in columns {
        out.push(' ');
        out.push_str(column);
        out.push_str(" |");
    }
    out.push('\n');
    out.push('|');
    for _ in columns {
        out.push_str("---|");
    }
    out.push('\n');
}

fn table_row(out: &mut String, cells: &[&str]) {
    out.push('|');
    for cell in cells {
        out.push(' ');
        // A pipe inside a cell would split the row.
        out.push_str(&cell.replace('|', "\\|"));
        out.push_str(" |");
    }
    out.push('\n');
}

/// `cli.md`'s block: every subcommand and flag, walked from clap's own model
/// (`Command`/`Arg`), so a flag cannot be added without appearing here.
pub fn render_cli() -> String {
    use clap::CommandFactory as _;

    let mut out = String::new();
    let cli = crate::cli::Cli::command();
    for sub in cli.get_subcommands() {
        // A hidden command (`bg-shim`) is not for humans, and the docs are.
        if sub.is_hide_set() {
            continue;
        }
        let _ = writeln!(out, "### `kelir {}`\n", sub.get_name());
        let about = sub
            .get_about()
            .map(|about| about.to_string())
            .unwrap_or_default();
        if !about.is_empty() {
            let _ = writeln!(out, "{about}\n");
        }
        table_header(&mut out, &["flag", "takes", "what it does"]);
        for arg in sub.get_arguments() {
            // A help flag is clap's, not kelir's: `kelir <cmd> --help` is
            // clap's own output and is the same thing one screen away.
            let id = arg.get_id().as_str();
            if id == "help" || id == "version" || arg.is_hide_set() {
                continue;
            }
            let mut names = Vec::new();
            if let Some(short) = arg.get_short() {
                names.push(format!("`-{short}`"));
            }
            if let Some(long) = arg.get_long() {
                names.push(format!("`--{long}`"));
            }
            if names.is_empty() {
                // A positional, named by its value name.
                let value = arg
                    .get_value_names()
                    .and_then(|names| names.first().map(|name| name.to_string()))
                    .unwrap_or_else(|| id.to_string());
                names.push(format!("`<{value}>`"));
            }
            let help = arg
                .get_help()
                .map(|help| help.to_string())
                .unwrap_or_default()
                .replace('\n', " ");
            let takes = if arg.get_action().takes_values() {
                arg.get_value_names()
                    .and_then(|names| names.first().map(|name| name.to_string()))
                    .map(|name| format!("`<{name}>`"))
                    .unwrap_or_else(|| "a value".to_string())
            } else {
                "nothing".to_string()
            };
            table_row(&mut out, &[&names.join(", "), &takes, &help]);
        }
        out.push('\n');
    }
    out
}

/// `verbs.md`'s block: the verb table.
pub fn render_verbs() -> String {
    let mut out = String::new();
    table_header(
        &mut out,
        &["verb", "arguments", "applies at", "what it does"],
    );
    for verb in tables::VERBS {
        table_row(
            &mut out,
            &[
                &format!("`{}`", verb.name),
                verb.args,
                verb.applies,
                verb.effect,
            ],
        );
    }
    out
}

/// `env.md`'s block: the three tables, from [`crate::envs::VARS`].
pub fn render_env() -> String {
    let mut out = String::new();
    for (dir, heading) in [
        (crate::envs::Dir::Read, "Read by kelir"),
        (crate::envs::Dir::Set, "Set by kelir for its children"),
        (crate::envs::Dir::TestHook, "Set by the test suite only"),
    ] {
        let _ = writeln!(out, "### {heading}\n");
        table_header(&mut out, &["variable", "effect"]);
        for var in crate::envs::with_dir(dir) {
            table_row(&mut out, &[&format!("`{}`", var.name), var.effect]);
        }
        out.push('\n');
    }
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

/// `events.md`'s block: the event table.
pub fn render_events() -> String {
    let mut out = String::new();
    table_header(&mut out, &["type", "fields", "written by"]);
    for event in tables::EVENTS {
        table_row(
            &mut out,
            &[&format!("`{}`", event.name), event.fields, event.written_by],
        );
    }
    out
}

/// `config.md`'s block: every key, in two tables (top level, then the
/// `[providers.<name>]` fields), with the function that reads it.
pub fn render_config() -> String {
    let mut out = String::new();
    for (scope, heading) in [
        (tables::KeyScope::Top, "Top level"),
        (
            tables::KeyScope::Provider,
            "Per provider (`[providers.<name>]`)",
        ),
    ] {
        let _ = writeln!(out, "### {heading}\n");
        table_header(
            &mut out,
            &["key", "type", "default", "override", "effect", "source"],
        );
        for key in tables::CONFIG_KEYS.iter().filter(|key| key.scope == scope) {
            table_row(
                &mut out,
                &[
                    &format!("`{}`", key.name),
                    key.kind,
                    key.default,
                    key.overrides,
                    key.effect,
                    &format!("`src/config.rs:{}`", key.field_fn),
                ],
            );
        }
        out.push('\n');
    }
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

// ------------------------------------------------------------------ the CLI

/// `kelir docs [topic]`, `--list`, `--find <words>`, `--generate`, `--check`.
///
/// Exit 0 on a successful read or a clean `--check`; 1 for an unknown topic, a
/// `--find` with no hit and a stale block; 2 for a usage error.
pub fn cli(args: &crate::cli::DocsArgs) -> u8 {
    if args.generate && args.check {
        eprintln!("kelir docs: `--generate` and `--check` are different jobs; pick one");
        return 2;
    }
    if args.generate {
        return write_generated();
    }
    if args.check {
        let problems = check();
        if problems.is_empty() {
            println!("docs: up to date ({} topics)", TOPICS.len());
            return 0;
        }
        for problem in problems {
            eprintln!("kelir docs: {problem}");
        }
        return 1;
    }
    if let Some(words) = &args.find {
        return print_find(words);
    }
    if args.list {
        print_list();
        return 0;
    }
    let Some(name) = args.topic.as_deref() else {
        print_list();
        println!();
        println!(
            "run `kelir docs <topic>` for one of the files above (the summary is its first line)."
        );
        return 0;
    };
    match topic(name) {
        Some(topic) => {
            print!("{}", topic.body);
            if !topic.body.ends_with('\n') {
                println!();
            }
            0
        }
        None => {
            eprintln!("kelir docs: no topic `{name}`");
            let close = suggestions(name);
            if !close.is_empty() {
                eprintln!("  did you mean: {}?", close.join(", "));
            }
            eprintln!("  `kelir docs --list` names them all");
            1
        }
    }
}

fn print_list() {
    let width = TOPICS
        .iter()
        .map(|topic| topic.name.len())
        .max()
        .unwrap_or(0);
    for topic in TOPICS {
        println!("  {:width$}  {}", topic.name, topic.summary);
    }
}

fn print_find(words: &str) -> u8 {
    let hits = find(words);
    if hits.is_empty() {
        eprintln!("kelir docs: nothing in the reference mentions `{words}`");
        eprintln!("  `kelir docs --list` names every topic");
        return 1;
    }
    for hit in &hits {
        println!("{} — {}", hit.topic, hit.summary);
        for (number, line) in &hit.lines {
            println!("  {number}: {line}");
        }
    }
    0
}

/// Rewrite the generated blocks in `docs/reference/` in place.
///
/// An unchanged file is not rewritten, so running it twice changes nothing
/// (and a build tool watching the directory is not woken for nothing).
fn write_generated() -> u8 {
    let Some(root) = docs_root() else {
        eprintln!(
            "kelir docs: no `docs/reference` directory here: run this from the kelir checkout, \
             or set KELIR_DOCS_ROOT"
        );
        return 1;
    };
    let mut changed = 0u32;
    for topic in TOPICS {
        let path = root.join(format!("{}.md", topic.name));
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!("kelir docs: cannot read {}", path.display());
            return 1;
        };
        let (expected, kinds) = match regenerate(&text) {
            Ok(result) => result,
            Err(error) => {
                eprintln!("kelir docs: {}: {error}", path.display());
                return 1;
            }
        };
        if expected == text {
            continue;
        }
        let moved: Vec<String> = kinds
            .into_iter()
            .filter(|kind| block(&text, kind) != block(&expected, kind))
            .collect();
        if let Err(error) = std::fs::write(&path, &expected) {
            eprintln!("kelir docs: cannot write {}: {error}", path.display());
            return 1;
        }
        println!("docs: {}: wrote {}", path.display(), moved.join(", "));
        changed += 1;
    }
    if changed == 0 {
        println!("docs: nothing to generate");
    }
    0
}
