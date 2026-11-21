//! `kelir docs`: the reference, embedded in the binary and readable by the agent.
//! Every topic is a file under `docs/reference/` compiled in with `include_str!`; its one-line
//! summary is taken from the `**Summary:**` paragraph of that same file.

#[cfg(test)]
mod tests;

/// One reference topic: the file name and its embedded text.
#[derive(Debug, Clone, Copy)]
pub struct Topic {
    /// The name `kelir docs <topic>` takes (the file name without `.md`).
    pub name: &'static str,
    /// The shipped text (`docs/reference/<name>.md`).
    pub body: &'static str,
}

impl Topic {
    /// The first line of the `**Summary:**` paragraph, taken from this file.
    pub fn summary(&self) -> &'static str {
        std::sync::LazyLock::force(&SUMMARY)
            .iter()
            .find(|(name, _)| *name == self.name)
            .map(|(_, text)| text.as_str())
            .expect("every topic has a summary")
    }
}

static SUMMARY: std::sync::LazyLock<Vec<(&'static str, String)>> = std::sync::LazyLock::new(|| {
    TOPICS
        .iter()
        .map(|entry| (entry.name, summary_of(entry.body)))
        .collect()
});

fn summary_of(body: &'static str) -> String {
    let rest = body
        .split_once("**Summary:**")
        .expect("a reference topic needs a **Summary:** line")
        .1;
    // The summary runs until a blank line or the next bold heading: fold its
    // wrapped lines into the one line the topic list shows.
    let line: String = rest
        .lines()
        .take_while(|line| !line.trim().is_empty() && !line.starts_with("**"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    line.trim().to_string()
}

/// The topics, in the order a bare `kelir docs` prints them.
pub const TOPICS: &[Topic] = &[
    topic("README", include_str!("../../docs/reference/README.md")),
    topic("config", include_str!("../../docs/reference/config.md")),
    topic("cli", include_str!("../../docs/reference/cli.md")),
    topic("env", include_str!("../../docs/reference/env.md")),
    topic("verbs", include_str!("../../docs/reference/verbs.md")),
    topic("mcp", include_str!("../../docs/reference/mcp.md")),
    topic("channels", include_str!("../../docs/reference/channels.md")),
    topic("gates", include_str!("../../docs/reference/gates.md")),
    topic("children", include_str!("../../docs/reference/children.md")),
    topic("jobs", include_str!("../../docs/reference/jobs.md")),
    topic(
        "providers",
        include_str!("../../docs/reference/providers.md"),
    ),
    topic(
        "sources-and-skills",
        include_str!("../../docs/reference/sources-and-skills.md"),
    ),
    topic("files", include_str!("../../docs/reference/files.md")),
    topic("events", include_str!("../../docs/reference/events.md")),
    topic("attach", include_str!("../../docs/reference/attach.md")),
    topic("notify", include_str!("../../docs/reference/notify.md")),
    topic("tui", include_str!("../../docs/reference/tui.md")),
    topic(
        "troubleshooting",
        include_str!("../../docs/reference/troubleshooting.md"),
    ),
    topic(
        "source-map",
        include_str!("../../docs/reference/source-map.md"),
    ),
];

const fn topic(name: &'static str, body: &'static str) -> Topic {
    Topic { name, body }
}

/// The topic named `name`, with or without `.md`.
pub fn topic_by_name(name: &str) -> Option<&'static Topic> {
    let name = name.strip_suffix(".md").unwrap_or(name);
    TOPICS
        .iter()
        .find(|topic| topic.name.eq_ignore_ascii_case(name))
}

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

/// One `topic:line: text` search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub topic: &'static str,
    pub number: usize,
    pub line: String,
    pub score: usize,
}

/// Case-insensitive search over all topics. A topic's rank is its number of
/// matching terms in the summary plus the score of its matching lines; lines
/// are printed with their topic's rank, best topic first.
pub fn search(query: &str) -> Vec<Hit> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|term| term.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-'))
        .filter(|term| !term.is_empty())
        .map(|term| term.to_ascii_lowercase())
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    let mut ranks: Vec<(usize, &'static str)> = Vec::new();
    for entry in TOPICS {
        let body = entry.body.to_ascii_lowercase();
        let mut topic_score = 0;
        for (index, line) in entry.body.lines().enumerate() {
            let lower = body.lines().nth(index).unwrap_or_default();
            let mut score = 0;
            for term in &terms {
                if lower.contains(term) {
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
                topic_score += score;
                hits.push(Hit {
                    topic: entry.name,
                    number: index + 1,
                    line: line.trim().to_string(),
                    score,
                });
            }
        }
        if topic_score > 0 {
            for term in &terms {
                if entry.summary().to_ascii_lowercase().contains(term) {
                    topic_score += 50;
                }
            }
            ranks.push((topic_score, entry.name));
        }
    }
    ranks.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    let rank: std::collections::HashMap<&'static str, usize> = ranks
        .into_iter()
        .enumerate()
        .map(|(index, (_, name))| (name, index))
        .collect();
    // Print in topic order; within one topic, the strongest lines first.
    hits.sort_by(|a, b| {
        rank.get(a.topic)
            .cmp(&rank.get(b.topic))
            .then(b.score.cmp(&a.score))
            .then(a.number.cmp(&b.number))
    });
    hits
}

/// `kelir docs [topic]`, `--list`, `--search <words>`, `--skill <name>`.
pub fn cli(args: &crate::cli::DocsArgs) -> u8 {
    if let Some(name) = &args.skill {
        return print_skill(name);
    }
    if let Some(words) = &args.search {
        return print_search(words);
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
    match topic_by_name(name) {
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
        println!("  {:width$}  {}", topic.name, topic.summary());
    }
}

fn print_search(words: &str) -> u8 {
    let hits = search(words);
    if hits.is_empty() {
        eprintln!("kelir docs: nothing in the reference mentions `{words}`");
        eprintln!("  `kelir docs --list` names every topic");
        return 1;
    }
    for hit in &hits {
        println!("{}:{}: {}", hit.topic, hit.number, hit.line);
    }
    0
}

/// `kelir docs --skill <name>`: a builtin skill's whole text (it has no file the
/// model could read with `cat`).
fn print_skill(name: &str) -> u8 {
    match crate::sources::BUILTIN_SKILLS
        .iter()
        .find(|(skill, _)| *skill == name)
    {
        Some((_, text)) => {
            print!("{text}");
            if !text.ends_with('\n') {
                println!();
            }
            0
        }
        None => {
            let names: Vec<&str> = crate::sources::BUILTIN_SKILLS
                .iter()
                .map(|(skill, _)| *skill)
                .collect();
            eprintln!(
                "kelir docs: no builtin skill `{name}` (they are: {})",
                names.join(", ")
            );
            1
        }
    }
}
