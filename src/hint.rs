//! The pushed hint: "might be relevant: name - description".
//! Skills are *pulled*: the base prompt names the directories and carries no listing.
//! The pushed hint is the other half.
//! When a human prompt arrives and a scorer is configured, kelir makes **one** cheap scoring call
//! that is shown the prompt and a catalogue of `name - description` lines (skills now, MCP servers

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Value, json};

pub const MAX_NAMES: usize = 3;
/// The most characters of a description shown to the scorer and in the hint.
pub const MAX_DESCRIPTION: usize = 200;
pub const MAX_PROMPT: usize = 2000;
pub const MAX_CANDIDATES: usize = 64;
/// The `name` of the `source` event.
pub const SOURCE_NAME: &str = "hint";
/// The `kind` of the `source` event.
pub const SOURCE_KIND: &str = "hint";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// `skill` (or, from, `mcp`).
    pub kind: &'static str,
    pub name: String,
    pub description: String,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Scorer: Send + Sync {
    fn score<'a>(
        &'a self,
        prompt: &'a str,
        candidates: &'a [Candidate],
    ) -> BoxFuture<'a, Result<Vec<String>, String>>;

    /// The usage the last call reported, taken once. A scorer that
    /// reports none (a test double) returns `None`.
    fn take_usage(&self) -> Option<crate::stream::Usage> {
        None
    }
}

/// The catalogue: the skills a session can see, as `name - description`.
pub fn catalogue(places: &crate::sources::Places) -> Vec<Candidate> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for found in crate::sources::discover(places) {
        if found.kind != crate::sources::SourceKind::Skill {
            continue;
        }
        let Ok(skill) = crate::sources::parse_skill(&found.text) else {
            continue;
        };
        let name = if skill.name.trim().is_empty() {
            found.name.clone()
        } else {
            skill.name.trim().to_string()
        };
        if !seen.insert(name.clone()) {
            continue;
        }
        out.push(Candidate {
            kind: "skill",
            name,
            description: flat(&skill.description, MAX_DESCRIPTION),
        });
        if out.len() >= MAX_CANDIDATES {
            break;
        }
    }
    out
}

fn flat(text: &str, max: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::util::truncate_chars(&joined, max)
}

pub fn parse_reply(reply: &str) -> Option<Vec<String>> {
    if let Ok(value) = serde_json::from_str::<Value>(reply.trim())
        && let Some(names) = names_of(&value)
    {
        return Some(names);
    }
    for (open, close) in [('[', ']'), ('{', '}')] {
        let (Some(start), Some(end)) = (reply.find(open), reply.rfind(close)) else {
            continue;
        };
        if end <= start {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(&reply[start..=end])
            && let Some(names) = names_of(&value)
        {
            return Some(names);
        }
    }
    None
}

fn names_of(value: &Value) -> Option<Vec<String>> {
    let array = match value {
        Value::Array(items) => items,
        Value::Object(map) => ["names", "skills", "relevant", "result"]
            .iter()
            .find_map(|key| map.get(*key).and_then(Value::as_array))?,
        _ => return None,
    };
    let mut out = Vec::new();
    for item in array {
        match item {
            Value::String(name) => out.push(name.clone()),
            // `{"name": "x", ...}` is a fair way to answer too.
            Value::Object(map) => {
                if let Some(Value::String(name)) = map.get("name") {
                    out.push(name.clone());
                }
            }
            _ => {}
        }
    }
    Some(out)
}

/// Keep only the names the catalogue offered, once each, at most
/// [`MAX_NAMES`], in the scorer's order.
pub fn select<'a>(names: &[String], candidates: &'a [Candidate]) -> Vec<&'a Candidate> {
    let mut out: Vec<&Candidate> = Vec::new();
    for name in names {
        let name = name.trim();
        if let Some(candidate) = candidates.iter().find(|candidate| candidate.name == name)
            && !out.iter().any(|seen| seen.name == candidate.name)
        {
            out.push(candidate);
            if out.len() >= MAX_NAMES {
                break;
            }
        }
    }
    out
}

/// The hint's text: one `might be relevant: name - description` line per name.
/// Never a body.
pub fn render(chosen: &[&Candidate]) -> Option<String> {
    if chosen.is_empty() {
        return None;
    }
    let lines: Vec<String> = chosen
        .iter()
        .map(|candidate| {
            if candidate.description.is_empty() {
                format!("might be relevant: {}", candidate.name)
            } else {
                format!(
                    "might be relevant: {} - {}",
                    candidate.name, candidate.description
                )
            }
        })
        .collect();
    Some(lines.join("\n"))
}

/// Run a scorer under the deadline and turn its answer into the hint text.
pub async fn hint_for(
    scorer: &dyn Scorer,
    prompt: &str,
    candidates: &[Candidate],
    timeout: Duration,
) -> Result<Option<String>, String> {
    if candidates.is_empty() || prompt.trim().is_empty() {
        return Ok(None);
    }
    let prompt = crate::util::truncate_chars(prompt.trim(), MAX_PROMPT);
    let names = match tokio::time::timeout(timeout, scorer.score(&prompt, candidates)).await {
        Ok(Ok(names)) => names,
        Ok(Err(why)) => return Err(why),
        Err(_) => return Err(format!("timed out after {} ms", timeout.as_millis())),
    };
    Ok(render(&select(&names, candidates)))
}

/// Whether two base URLs name the same scheme, host and port. A key that was
/// issued for one origin is only ever sent there.
pub fn same_origin(a: &str, b: &str) -> bool {
    let (Ok(a), Ok(b)) = (reqwest::Url::parse(a), reqwest::Url::parse(b)) else {
        return false;
    };
    a.scheme() == b.scheme()
        && a.host_str().map(str::to_ascii_lowercase) == b.host_str().map(str::to_ascii_lowercase)
        && a.port_or_known_default() == b.port_or_known_default()
}

// ---------------------------------------------------------------- the real one

pub struct Chat {
    http: reqwest::Client,
    url: String,
    key: Option<String>,
    model: String,
    usage: std::sync::Mutex<Option<crate::stream::Usage>>,
}

/// Room for a reasoning model to think and still answer: the answer itself is a few names,
/// but a reasoning scorer spent 39-111 tokens on thinking against the old cap of 120.
pub const SCORER_MAX_TOKENS: u64 = 600;

impl Chat {
    /// `base_url` is the provider's (or the configured `endpoint`); `key` is
    /// sent as a bearer token, in memory only.
    pub fn new(base_url: &str, key: Option<String>, model: &str) -> Result<Chat, String> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|error| format!("http client: {error}"))?;
        Ok(Chat {
            http,
            url: crate::provider::chat_completions_url(base_url),
            key: key.filter(|key| !key.is_empty()),
            model: model.to_string(),
            usage: std::sync::Mutex::new(None),
        })
    }

    /// The request body. Public so a test can see exactly what the scorer is
    /// shown: the prompt and `name - description` lines, never a skill body.
    pub fn body(model: &str, prompt: &str, candidates: &[Candidate]) -> Value {
        let mut catalogue = String::new();
        for candidate in candidates {
            catalogue.push_str(&format!(
                "- {} ({}): {}\n",
                candidate.name, candidate.kind, candidate.description
            ));
        }
        json!({
            "model": model,
            "stream": false,
            "temperature": 0,
            "max_tokens": SCORER_MAX_TOKENS,
            "messages": [
                {
                    "role": "system",
                    "content": "You pick which of the listed skills could help with the user's message. \
                                Answer with a JSON array of at most 3 names from the list, best first, \
                                and nothing else. Answer [] when none clearly helps."
                },
                {
                    "role": "user",
                    "content": format!("Skills:\n{catalogue}\nMessage:\n{prompt}")
                }
            ]
        })
    }
}

impl Scorer for Chat {
    fn score<'a>(
        &'a self,
        prompt: &'a str,
        candidates: &'a [Candidate],
    ) -> BoxFuture<'a, Result<Vec<String>, String>> {
        Box::pin(async move {
            let mut request = self
                .http
                .post(&self.url)
                .header("content-type", "application/json")
                .header("user-agent", format!("kelir/{}", crate::VERSION))
                .json(&Self::body(&self.model, prompt, candidates));
            if let Some(key) = &self.key {
                request = request.bearer_auth(key);
            }
            let response = request
                .send()
                .await
                .map_err(|_| "the scorer could not be reached".to_string())?;
            let status = response.status();
            if !status.is_success() {
                // The body is not read into a message: a gateway may quote the
                // request, and the request carried a key.
                return Err(format!("the scorer answered http {}", status.as_u16()));
            }
            let text = response
                .text()
                .await
                .map_err(|_| "the scorer's answer was cut off".to_string())?;
            let value: Value = serde_json::from_str(&text)
                .map_err(|_| "the scorer's answer was not JSON".to_string())?;
            if let Some(usage) = value.get("usage").filter(|usage| usage.is_object())
                && let Ok(mut slot) = self.usage.lock()
            {
                *slot = Some(crate::stream::parse_usage(usage));
            }
            let content = value["choices"][0]["message"]["content"]
                .as_str()
                .ok_or_else(|| "the scorer's answer had no text".to_string())?;
            parse_reply(content)
                .ok_or_else(|| "the scorer's answer was not a list of names".to_string())
        })
    }

    fn take_usage(&self) -> Option<crate::stream::Usage> {
        self.usage.lock().ok().and_then(|mut slot| slot.take())
    }
}

// --------------------------------------------------------------- the test one

/// A scorer for tests: answers from a fixed script, optionally slowly or with an error. 10
/// held-out prompts through it prove the wiring (the quality check is a live one).
pub struct Scripted {
    pub answers: Vec<(String, Result<Vec<String>, String>)>,
    pub delay: Option<Duration>,
}

impl Scorer for Scripted {
    fn score<'a>(
        &'a self,
        prompt: &'a str,
        _candidates: &'a [Candidate],
    ) -> BoxFuture<'a, Result<Vec<String>, String>> {
        Box::pin(async move {
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            self.answers
                .iter()
                .find(|(needle, _)| prompt.contains(needle.as_str()))
                .map(|(_, answer)| answer.clone())
                .unwrap_or(Ok(Vec::new()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(name: &str, description: &str) -> Candidate {
        Candidate {
            kind: "skill",
            name: name.into(),
            description: description.into(),
        }
    }

    fn catalogue_of_three() -> Vec<Candidate> {
        vec![
            cand("sift", "find the needle in a haystack of logs"),
            cand("lathe", "turn a rough draft into a polished one"),
            cand("kelir", "operate or configure kelir itself"),
        ]
    }

    #[test]
    fn replies_parse_from_arrays_objects_fences_and_prose() {
        for (reply, want) in [
            (r#"["sift","lathe"]"#, Some(vec!["sift", "lathe"])),
            (r#"{"names":["sift"]}"#, Some(vec!["sift"])),
            (r#"{"skills":[{"name":"sift"}]}"#, Some(vec!["sift"])),
            ("```json\n[\"sift\"]\n```", Some(vec!["sift"])),
            ("Sure! [\"lathe\"] is the one.", Some(vec!["lathe"])),
            ("[]", Some(vec![])),
            ("", None),
            ("sift", None),
            ("{\"a\":1}", None),
            ("[1,2,3]", Some(vec![])),
            ("[\"x\"", None),
        ] {
            let got = parse_reply(reply);
            let want = want.map(|names| names.into_iter().map(String::from).collect::<Vec<_>>());
            assert_eq!(got, want, "{reply:?}");
        }
    }

    #[test]
    fn only_offered_names_survive_once_each_and_at_most_three() {
        let candidates = catalogue_of_three();
        let names: Vec<String> = ["ghost", "sift", " sift ", "lathe", "kelir", "sift"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        let chosen = select(&names, &candidates);
        let got: Vec<&str> = chosen.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(got, ["sift", "lathe", "kelir"]);
        let many: Vec<Candidate> = (0..10).map(|i| cand(&format!("s{i}"), "d")).collect();
        let all: Vec<String> = many.iter().map(|c| c.name.clone()).collect();
        assert_eq!(select(&all, &many).len(), MAX_NAMES);
    }

    #[test]
    fn the_hint_is_name_and_description_never_a_body() {
        let candidates = catalogue_of_three();
        let chosen = select(&["sift".to_string()], &candidates);
        assert_eq!(
            render(&chosen).unwrap(),
            "might be relevant: sift - find the needle in a haystack of logs"
        );
        assert_eq!(render(&[]), None);
        let bare = [cand("x", "")];
        assert_eq!(render(&[&bare[0]]).unwrap(), "might be relevant: x");
    }

    #[test]
    fn the_scorer_is_shown_names_and_descriptions_only() {
        let body = Chat::body("m", "find the error in my log", &catalogue_of_three());
        let user = body["messages"][1]["content"].as_str().unwrap();
        assert!(user.contains("- sift (skill): find the needle"));
        assert!(user.contains("find the error in my log"));
        assert_eq!(body["stream"], false);
        assert_eq!(body["model"], "m");
    }

    #[tokio::test]
    async fn a_scripted_scorer_names_the_right_skill_for_held_out_prompts() {
        let scorer = Scripted {
            answers: vec![
                ("log".into(), Ok(vec!["sift".into()])),
                ("draft".into(), Ok(vec!["lathe".into()])),
                ("config".into(), Ok(vec!["kelir".into()])),
                ("boom".into(), Err("the scorer answered http 500".into())),
            ],
            delay: None,
        };
        let candidates = catalogue_of_three();
        let timeout = Duration::from_secs(2);
        for (prompt, want) in [
            ("grep the log for the failing request", Some("sift")),
            ("polish this draft of my essay", Some("lathe")),
            ("how do I change the config key", Some("kelir")),
            ("what is two plus two", None),
        ] {
            let got = hint_for(&scorer, prompt, &candidates, timeout)
                .await
                .unwrap();
            match want {
                Some(name) => assert!(
                    got.as_deref()
                        .unwrap()
                        .contains(&format!("might be relevant: {name} - ")),
                    "{prompt}: {got:?}"
                ),
                None => assert_eq!(got, None, "{prompt}"),
            }
        }
        assert!(
            hint_for(&scorer, "boom", &candidates, timeout)
                .await
                .is_err()
        );
    }

    #[test]
    fn same_origin_compares_scheme_host_and_port() {
        assert!(same_origin(
            "https://openrouter.ai/api/v1",
            "https://OpenRouter.ai:443/other"
        ));
        assert!(!same_origin("https://a.example/v1", "https://b.example/v1"));
        assert!(!same_origin("http://a.example/v1", "https://a.example/v1"));
        assert!(!same_origin(
            "http://127.0.0.1:8080/v1",
            "http://127.0.0.1:9/v1"
        ));
        assert!(!same_origin("not a url", "https://a.example"));
    }

    #[test]
    fn the_scorer_cap_leaves_room_to_think() {
        let body = Chat::body("m", "p", &catalogue_of_three());
        assert_eq!(body["max_tokens"], SCORER_MAX_TOKENS);
        const { assert!(SCORER_MAX_TOKENS >= 400) };
    }

    #[tokio::test]
    async fn a_slow_scorer_times_out_and_an_empty_catalogue_is_not_asked() {
        let slow = Scripted {
            answers: vec![("x".into(), Ok(vec!["sift".into()]))],
            delay: Some(Duration::from_secs(5)),
        };
        let started = std::time::Instant::now();
        let got = hint_for(&slow, "x", &catalogue_of_three(), Duration::from_millis(50)).await;
        assert!(got.unwrap_err().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
        let got = hint_for(&slow, "x", &[], Duration::from_millis(50)).await;
        assert_eq!(got, Ok(None));
        let got = hint_for(
            &slow,
            "   ",
            &catalogue_of_three(),
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(got, Ok(None));
    }

    #[test]
    fn the_catalogue_lists_parsed_skills_once_each() {
        let dir = std::env::temp_dir().join(format!("kelir-hint-{}", ulid::Ulid::generate()));
        let skills = dir.join(".kelir").join("skills");
        for (name, text) in [
            (
                "sift",
                "---\nname: sift\ndescription: find   the    needle\n---\nBODY-SECRET\n",
            ),
            ("broken", "no front matter at all"),
        ] {
            std::fs::create_dir_all(skills.join(name)).unwrap();
            std::fs::write(skills.join(name).join("SKILL.md"), text).unwrap();
        }
        let places = crate::sources::Places {
            memory_dir: None,
            cwd: dir.clone(),
            ..Default::default()
        };
        let candidates = catalogue(&places);
        let names: Vec<&str> = candidates.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"sift"), "{names:?}");
        assert!(!names.contains(&"broken"), "{names:?}");
        let sift = candidates.iter().find(|c| c.name == "sift").unwrap();
        assert_eq!(sift.description, "find the needle");
        assert!(!format!("{candidates:?}").contains("BODY-SECRET"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
