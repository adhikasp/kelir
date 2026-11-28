//! The `/` completion popup: every command and every skill, ranked by a fuzzy
//! match that scores the way a person scans a list — a name that starts with
//! what was typed beats one that merely contains it, and whole words beat
//! scattered letters — with Tab accepting the highlighted row.
//!
//! The catalogue is built once (the commands are static, the skills come from
//! the same discovery that rendered the system prompt) and the row list is
//! recomputed on each keystroke: a few dozen rows, a handful of comparisons
//! each, in front of a keystroke that already redraws the screen.

use crate::tui::commands::COMMANDS;
use crate::tui::view::PROMPT_WIDTH;

/// What a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A `/` command.
    Command,
    /// A skill, loaded with `/skill <name>`.
    Skill,
}

/// One skill this session can load (`/skill <name>`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillInfo {
    pub name: String,
    pub about: String,
    /// Where the `SKILL.md` is; empty for a skill compiled into the binary.
    pub path: String,
}

/// One row of the popup.
#[derive(Debug, Clone)]
pub struct Entry {
    /// What the popup shows: `/model <p/m>`, `/skill sift`.
    pub label: String,
    /// What the match is scored on: `model`, `sift`.
    pub name: String,
    /// The one-line description shown after the label.
    pub about: String,
    /// What Tab types in the line's first word: the command's name and a
    /// space, never the usage's placeholder, so the accepted line parses and
    /// the argument is still the person's to type.
    pub insert: String,
    pub kind: Kind,
    /// The skill's `SKILL.md`; empty for a builtin.
    pub path: String,
}

impl Entry {
    /// What accepting this row types. Standing on its own (`/sif` → `/skill
    /// sift `) the whole command goes in; as `/skill`'s argument (`/skill si`)
    /// only the name does, because the command is already typed.
    pub fn accept(&self, skill_argument: bool) -> String {
        if skill_argument && self.kind == Kind::Skill {
            return format!("{} ", self.name);
        }
        self.insert.clone()
    }

    fn from_command(usage: &str, about: &str) -> Option<Self> {
        let name = usage.trim_start_matches('/').split_whitespace().next()?;
        // The aliases `/help` lists (`/say-now <text>` also answers to `/now`)
        // are not part of the skeleton: Tab types the canonical spelling.
        Some(Self {
            label: usage.to_string(),
            name: name.to_string(),
            about: about.to_string(),
            insert: format!("/{name} "),
            kind: Kind::Command,
            path: String::new(),
        })
    }

    fn from_skill(skill: &SkillInfo) -> Self {
        Self {
            label: format!("/skill {}", skill.name),
            name: skill.name.clone(),
            about: skill.about.clone(),
            insert: format!("/skill {} ", skill.name),
            kind: Kind::Skill,
            path: skill.path.clone(),
        }
    }
}

/// Where the cursor is and what to search for; `None` from [`context`] means
/// no popup belongs on this line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// What was typed of the word being completed (`mod`, or empty after `/`).
    pub query: String,
    /// Display column where the word starts, `› ` included: the rows line up
    /// under what is being typed.
    pub columns: u16,
    /// The word is `/skill`'s first argument, so only skills are offered and
    /// accepting types just the name.
    pub skill_argument: bool,
    /// The character index where the word starts: accepting replaces exactly
    /// `start..cursor`.
    pub start: usize,
}

/// The command that takes a skill name as its first argument.
const SKILL_COMMAND: &str = "skill";

/// Read the editor line and the cursor's character index, and decide whether a
/// popup belongs there.
///
/// The popup is for the word the cursor is at the end of, on the first line,
/// when that line is a command (`/…`) that is still being named. A space after
/// a command commits it: `/model ` is an argument, not a name.
pub fn context(line: &str, cursor: usize) -> Option<Context> {
    let chars: Vec<char> = line.chars().collect();
    let cursor = cursor.min(chars.len());
    // A command is the whole first line, and starts in the first column.
    if chars.first() != Some(&'/') || chars.contains(&'\n') {
        return None;
    }
    // The word being typed ends at the cursor, so accepting replaces exactly
    // it and nothing else.
    if chars.get(cursor).is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    let start = chars[..cursor]
        .iter()
        .rposition(|c| c.is_whitespace())
        .map_or(0, |index| index + 1);
    let word: String = chars[start..cursor].iter().collect();
    let columns = (display_width(&chars[..start]) + PROMPT_WIDTH) as u16;
    let before: String = chars[..start].iter().collect();
    let command = before.trim();
    if command.is_empty() {
        // The first word: the command's own name.
        return Some(Context {
            query: word.trim_start_matches('/').to_string(),
            columns,
            skill_argument: false,
            start,
        });
    }
    if command != format!("/{SKILL_COMMAND}") {
        // Any other command's argument (`/model gpt…`) is not a name.
        return None;
    }
    Some(Context {
        query: word,
        columns,
        skill_argument: true,
        start,
    })
}

fn display_width(chars: &[char]) -> usize {
    chars
        .iter()
        .map(|c| crate::tui::scrollback::char_width(*c))
        .sum()
}

/// The catalogue the popup searches: every `/` command, then every skill.
#[derive(Debug, Clone, Default)]
pub struct Catalogue {
    entries: Vec<Entry>,
}

impl Catalogue {
    pub fn new(skills: &[SkillInfo]) -> Self {
        let mut entries: Vec<Entry> = COMMANDS
            .iter()
            .filter_map(|(usage, about)| Entry::from_command(usage, about))
            .collect();
        for skill in skills {
            entries.push(Entry::from_skill(skill));
        }
        Self { entries }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The skill row of that name, if the session found one.
    pub fn skill(&self, name: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| entry.kind == Kind::Skill && entry.name == name)
    }

    /// The rows for a query, best first. An empty query keeps catalogue order
    /// (the commands, then the skills by name).
    pub fn search(&self, query: &str, skills_only: bool) -> Vec<usize> {
        let mut scored: Vec<(i32, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| !skills_only || entry.kind == Kind::Skill)
            .filter_map(|(index, entry)| score(&entry.name, query).map(|rank| (rank, index)))
            .collect();
        // Ties keep catalogue order: an empty query lists `/stop` first, and
        // an equal score never hops about between keystrokes.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, index)| index).collect()
    }
}

/// An open popup: what it searches, its rows and the highlighted one.
#[derive(Debug, Clone)]
pub struct Completion {
    pub context: Context,
    /// Catalogue indices, best first.
    pub rows: Vec<usize>,
    /// The highlighted row's position in `rows`.
    pub selected: usize,
}

impl Completion {
    pub fn new(catalogue: &Catalogue, context: Context) -> Self {
        let rows = catalogue.search(&context.query, context.skill_argument);
        Self {
            context,
            rows,
            selected: 0,
        }
    }

    /// Move the highlight by `step`, wrapping: a short list is a ring, so Up
    /// from the first row is the last one.
    pub fn move_by(&mut self, step: isize) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as isize;
        self.selected = (self.selected as isize + step).rem_euclid(len) as usize;
    }

    /// The catalogue index of the highlighted row.
    pub fn chosen(&self) -> Option<usize> {
        self.rows.get(self.selected).copied()
    }
}

/// Score `name` against `query`, or `None` when the query's characters are not
/// all there in order. The walk is a greedy subsequence match, so `cmp` finds
/// `compact` and `mfc` finds `monthly-finance-close`.
///
/// The scale is chosen so one structural bonus outweighs a handful of gap
/// penalties, in the order a person ranks them:
///
/// - every matched character `+1`, two adjacent ones `+12` more;
/// - the first character `+40`, the first of a word `+25`, a camel-case hump
///   `+15`;
/// - a gap costs `-1` a character, capped at 8, so an indent or a long tail
///   cannot sink an otherwise good match;
/// - the whole name `+1000`, the name's prefix `+400`, the name's initials
///   `+200` (`mfc` → `monthly-finance-close`).
pub fn score(name: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let target: Vec<char> = name.to_ascii_lowercase().chars().collect();
    let needle: Vec<char> = query.to_ascii_lowercase().chars().collect();
    let starts = word_starts(name);
    let mut total = 0i32;
    let mut from = 0usize;
    let mut previous: Option<usize> = None;
    let mut whole = true;
    for (index, wanted) in needle.iter().enumerate() {
        let at = from + target[from..].iter().position(|c| c == wanted)?;
        total += MATCHED;
        match previous {
            None => {
                total -= at.min(GAP_MAX) as i32;
                if at == 0 {
                    total += FIRST;
                } else if starts[at] {
                    total += WORD;
                }
            }
            Some(before) => {
                total -= at.saturating_sub(before + 1).min(GAP_MAX) as i32;
                if at == before + 1 {
                    total += ADJACENT;
                } else if starts[at] {
                    total += WORD;
                } else if target[at - 1].is_ascii_lowercase() && target[at].is_ascii_uppercase() {
                    total += CAMEL;
                }
            }
        }
        // `whole` is "the query is the name, matched in place".
        whole &= at == index;
        previous = Some(at);
        from = at + 1;
    }
    if needle.len() == target.len() && whole {
        total += EXACT;
    } else if target.starts_with(&needle) {
        total += PREFIX;
    } else if initials(name) == query.to_ascii_lowercase() {
        total += INITIALS;
    }
    Some(total)
}

const MATCHED: i32 = 1;
const FIRST: i32 = 40;
const WORD: i32 = 25;
const CAMEL: i32 = 15;
const ADJACENT: i32 = 12;
const GAP_MAX: usize = 8;
const EXACT: i32 = 1000;
const PREFIX: i32 = 400;
const INITIALS: i32 = 200;

/// For each character of `name`: does a word begin there? At the start, after
/// a separator, at a lower-to-upper hump, and at a letter-to-digit step
/// (`tui2` has two words).
fn word_starts(name: &str) -> Vec<bool> {
    let chars: Vec<char> = name.chars().collect();
    chars
        .iter()
        .enumerate()
        .map(|(index, c)| {
            if !c.is_alphanumeric() {
                return false;
            }
            match index.checked_sub(1).map(|before| chars[before]) {
                None => true,
                Some(before) => {
                    !before.is_alphanumeric()
                        || (before.is_lowercase() && c.is_uppercase())
                        || (before.is_alphabetic() && c.is_ascii_digit())
                }
            }
        })
        .collect()
}

/// The first letter of every word, lowercased: `tools off|on` → `too`.
fn initials(name: &str) -> String {
    name.split(|c: char| !c.is_alphanumeric())
        .filter_map(|word| word.chars().next())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
#[allow(clippy::pedantic, clippy::nursery)]
mod tests {
    use super::*;

    fn skills() -> Vec<SkillInfo> {
        vec![
            SkillInfo {
                name: "sift".into(),
                about: "read the tail of a log".into(),
                path: "/skills/sift/SKILL.md".into(),
            },
            SkillInfo {
                name: "monthly-finance-close".into(),
                about: "close the month".into(),
                path: "/skills/mfc/SKILL.md".into(),
            },
            SkillInfo {
                name: "kelir".into(),
                about: "operate kelir itself".into(),
                path: String::new(),
            },
        ]
    }

    fn catalogue() -> Catalogue {
        Catalogue::new(&skills())
    }

    fn names(catalogue: &Catalogue, query: &str, skills_only: bool) -> Vec<String> {
        catalogue
            .search(query, skills_only)
            .into_iter()
            .map(|index| catalogue.entries()[index].name.clone())
            .collect()
    }

    #[test]
    fn a_bare_slash_opens_on_the_commands() {
        let ctx = context("/", 1).expect("a popup");
        assert_eq!(ctx.query, "");
        assert!(!ctx.skill_argument);
        assert_eq!(ctx.columns, 2, "`/` sits right after `› `");
        let catalogue = catalogue();
        let kinds: Vec<Kind> = catalogue
            .search("", false)
            .into_iter()
            .map(|index| catalogue.entries()[index].kind)
            .collect();
        assert_eq!(kinds.first(), Some(&Kind::Command));
        assert_eq!(kinds.last(), Some(&Kind::Skill));
    }

    #[test]
    fn the_popup_follows_the_word_being_typed() {
        let ctx = context("/mod", 4).expect("a popup");
        assert_eq!(ctx.query, "mod");
        assert_eq!(ctx.columns, 2, "`/` sits right after `› `");
        // A space commits the command: its arguments are not names.
        assert_eq!(context("/model ", 7), None);
        assert_eq!(context("/model a/b", 10), None);
        // Editing mid-word leaves the list closed.
        assert_eq!(context("/modx", 3), None);
        // A second line makes it a message, not a command.
        assert_eq!(context("/stop\nhello", 6), None);
        assert_eq!(context("hello", 5), None);
        assert_eq!(context("", 0), None);
    }

    #[test]
    fn the_skill_argument_lists_only_skills() {
        let ctx = context("/skill si", 9).expect("a popup");
        assert!(ctx.skill_argument);
        assert_eq!(ctx.query, "si");
        let catalogue = catalogue();
        assert_eq!(
            names(&catalogue, &ctx.query, ctx.skill_argument),
            vec!["sift".to_string()]
        );
        // `/skill ` with nothing typed offers every skill, so a name need not
        // be remembered.
        let ctx = context("/skill ", 7).expect("a popup");
        assert!(ctx.skill_argument);
        assert_eq!(ctx.query, "");
        assert_eq!(
            names(&catalogue, "", true),
            vec![
                "sift".to_string(),
                "monthly-finance-close".to_string(),
                "kelir".to_string()
            ]
        );
        // `/skill` itself is the command being typed.
        let ctx = context("/skill", 6).expect("a popup");
        assert!(!ctx.skill_argument);
        assert_eq!(ctx.query, "skill");
        // A finished argument is a commit, not a list.
        assert_eq!(context("/skill sift ", 12), None);
    }

    #[test]
    fn accepting_types_a_command_that_parses() {
        let catalogue = catalogue();
        for (query, want) in [("sto", "/stop "), ("mod", "/model ")] {
            let rows = catalogue.search(query, false);
            assert_eq!(
                catalogue.entries()[rows[0]].accept(false),
                want,
                "for {query}"
            );
        }
        // A command that needs nothing parses as typed.
        assert!(matches!(
            crate::tui::commands::parse("/stop"),
            Ok(crate::tui::commands::Parsed::Action(_))
        ));
        // `/sif` stands alone, so the whole command goes in; inside `/skill`
        // the command is already there and only the name does.
        let ctx = context("/sif", 4).expect("a popup");
        let rows = catalogue.search(&ctx.query, ctx.skill_argument);
        let entry = &catalogue.entries()[rows[0]];
        assert_eq!(entry.accept(ctx.skill_argument), "/skill sift ");
        assert!(crate::tui::commands::parse("/skill sift").is_ok());
        let ctx = context("/skill si", 9).expect("a popup");
        let rows = catalogue.search(&ctx.query, ctx.skill_argument);
        assert_eq!(
            catalogue.entries()[rows[0]].accept(ctx.skill_argument),
            "sift "
        );
    }

    #[test]
    fn every_command_is_listed_and_its_skeleton_parses() {
        let catalogue = Catalogue::new(&[]);
        for (usage, _) in COMMANDS {
            let name = usage
                .trim_start_matches('/')
                .split_whitespace()
                .next()
                .unwrap();
            assert!(
                catalogue.entries().iter().any(|entry| entry.name == name),
                "/{name} is missing from the catalogue"
            );
        }
        for entry in catalogue.entries() {
            assert!(entry.label.starts_with('/'), "{}", entry.label);
            if entry.kind != Kind::Command {
                continue;
            }
            // Tab types the name and a space. A command that takes an argument
            // answers with what it is missing (`/say-now needs something after
            // it`), never with `unknown command`.
            let line = entry.insert.trim_end();
            if let Err(message) = crate::tui::commands::parse(line) {
                assert!(
                    !message.contains("unknown command"),
                    "`{line}` (from {}) does not parse: {message}",
                    entry.label
                );
            }
        }
    }

    #[test]
    fn an_exact_name_outranks_a_prefix() {
        assert!(score("stop", "stop").unwrap() > score("stop", "sto").unwrap());
        assert!(score("stop", "sto").unwrap() > score("stop", "st").unwrap());
        assert_eq!(score("quit", "stop"), None);
    }

    #[test]
    fn a_prefix_beats_a_scattered_match() {
        // `cp`: `/compact` starts with `c` and has adjacent letters.
        assert!(score("compact", "cp").unwrap() > score("model", "cp").unwrap_or(i32::MIN));
        // A word start inside the name beats a letter merely present.
        assert!(score("say-now", "n").unwrap() > score("undo", "n").unwrap());
        // Adjacent letters beat a gap, even one that ends on the last letter.
        assert!(score("compact", "com").unwrap() > score("compact", "cpt").unwrap());
        // Only the whole name is an exact match, not a short query that
        // happens to end on its last letter.
        assert!(score("compact", "com").unwrap() < score("compact", "compact").unwrap());
        // A letter the name does not have is not a match at all.
        assert_eq!(score("say-now", "sto"), None);
    }

    #[test]
    fn a_subsequence_finds_the_row() {
        let catalogue = catalogue();
        assert_eq!(
            names(&catalogue, "cmp", false).first().map(String::as_str),
            Some("compact")
        );
        assert_eq!(
            names(&catalogue, "mfc", false).first().map(String::as_str),
            Some("monthly-finance-close")
        );
        // Initials of the row's own words.
        assert_eq!(
            names(&catalogue, "mdl", false).first().map(String::as_str),
            Some("model")
        );
        // Nothing in order is not a match.
        assert_eq!(score("help", "hpl"), None);
    }

    #[test]
    fn moving_the_highlight_wraps() {
        let catalogue = catalogue();
        let mut completion = Completion::new(&catalogue, context("/", 1).unwrap());
        let len = completion.rows.len();
        completion.move_by(-1);
        assert_eq!(completion.selected, len - 1);
        completion.move_by(1);
        assert_eq!(completion.selected, 0);
    }
}
