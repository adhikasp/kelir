//! The command-line side of children: `kelir run --child`, `kelir brief`,
//! `kelir kill` and `kelir ls`. Each runs in a tool call (or a terminal), holds
//! only that session's token, and asks the session to do the work; none of them
//! starts a process or reads a secret.

use std::path::Path;

use serde_json::{Value, json};

use crate::child::{self, EXIT_REFUSED, FOREGROUND_WAIT};
use crate::cli::{BriefArgs, KillArgs, LsArgs, RunArgs};
use crate::event::{EventBody, LogEvent};
use crate::ipc::{self, Request, Response};

fn session_address() -> Option<String> {
    crate::envs::var_os("KELIR_SOCK").map(|value| value.to_string_lossy().into_owned())
}

/// `kelir run --child "<task>" [--background] [--model M]`.
pub async fn run(args: RunArgs) -> u8 {
    let Some(address) = session_address() else {
        eprintln!(
            "kelir: --child: no session: run it from a kelir tool call (KELIR_SOCK is not set)"
        );
        return 2;
    };
    let task = match crate::session::task_text(&args.task).await {
        Ok(task) => task,
        Err(message) => {
            eprintln!("kelir: {message}");
            return 2;
        }
    };
    let body = json!({
        "op": "run",
        "task": task,
        "background": args.background,
        "model": args.common.model,
        "provider": args.common.provider,
        "cwd": args.common.cwd.as_ref().map(|path| crate::util::path_string(path)),
        "call_id": crate::envs::var("KELIR_CALL_ID"),
    });
    let wait = if args.background {
        std::time::Duration::from_secs(60)
    } else {
        FOREGROUND_WAIT
    };
    let response = call(&address, body, wait).await;
    report(response, args.background)
}

/// `kelir brief "<question>"`.
pub async fn brief(args: BriefArgs) -> u8 {
    let Some(address) = session_address() else {
        eprintln!(
            "kelir: brief: no session: run it from a kelir tool call (KELIR_SOCK is not set)"
        );
        return 2;
    };
    let question = match crate::session::task_text(&args.question).await {
        Ok(question) => question,
        Err(message) => {
            eprintln!("kelir: {message}");
            return 2;
        }
    };
    let body = json!({
        "op": "brief",
        "task": question,
        "background": false,
        "call_id": crate::envs::var("KELIR_CALL_ID"),
    });
    let response = call(&address, body, FOREGROUND_WAIT).await;
    report(response, false)
}

async fn call(address: &str, body: Value, wait: std::time::Duration) -> Result<Response, String> {
    let request = Request::new(child::VERB, body, "tool");
    ipc::request_value_within(address, &request, wait)
        .await
        .map_err(|error| error.message().to_string())
}

/// Print an answer and give the exit code. A cap refusal is exit 3.
fn report(response: Result<Response, String>, background: bool) -> u8 {
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            eprintln!("kelir: cannot reach the session: {error}");
            return 1;
        }
    };
    if !response.ok {
        let message = response
            .error
            .unwrap_or_else(|| "the session refused".into());
        eprintln!("kelir: {message}");
        return if message.starts_with("refused:") {
            EXIT_REFUSED
        } else {
            1
        };
    }
    let state = response.state.unwrap_or(Value::Null);
    let text = |key: &str| state[key].as_str().unwrap_or("").to_string();
    if background {
        println!(
            "child {} started in the background (session {}); its completion arrives as a message. \
             Stop it with: kelir kill {}",
            text("child_id"),
            text("session"),
            text("session")
        );
        return 0;
    }
    let result = text("result");
    if !result.is_empty() {
        println!("{result}");
    }
    let outcome = text("outcome");
    if state["truncated"].as_bool() == Some(true) {
        eprintln!(
            "kelir: the result was cut; the child's whole log is in session {}",
            text("session")
        );
    }
    if child::succeeded(&outcome) {
        0
    } else {
        eprintln!("kelir: child {} ended {outcome}", text("session"));
        1
    }
}

// ------------------------------------------------------------------ kill

/// `kelir kill <session>` / `kelir kill --tree`.
pub async fn kill(args: KillArgs) -> u8 {
    if args.session.is_none() && !args.tree {
        eprintln!("kelir: kill: name a session, or pass --tree for every descendant of this one");
        return 2;
    }
    let body = json!({
        "op": "kill",
        "target": args.session,
        "tree": args.tree,
    });
    // Inside a session: this session decides (it may only kill what is below
    // it). From a terminal: the human reads the root's token file, like ctl.
    let response = if let Some(address) = session_address() {
        call(&address, body, std::time::Duration::from_secs(30)).await
    } else {
        match from_terminal(&args, body).await {
            Ok(response) => Ok(response),
            Err(message) => {
                eprintln!("kelir: {message}");
                return 1;
            }
        }
    };
    match response {
        Ok(response) if response.ok => {
            let state = response.state.unwrap_or(Value::Null);
            println!("{}", state["message"].as_str().unwrap_or("done"));
            0
        }
        Ok(response) => {
            eprintln!(
                "kelir: {}",
                response.error.unwrap_or_else(|| "refused".into())
            );
            1
        }
        Err(error) => {
            eprintln!("kelir: cannot reach the session: {error}");
            1
        }
    }
}

/// A human's `kill`: find the root of the named session on disk and ask it with
/// its own token file.
async fn from_terminal(args: &KillArgs, mut body: Value) -> Result<Response, String> {
    let spec = args
        .session
        .as_deref()
        .ok_or("outside a session, name the session to kill")?;
    let id = crate::paths::resolve_session(spec)?;
    let (root, _) = root_of(&id);
    body["target"] = json!(id);
    body["human"] = json!(true);
    let dir = crate::paths::session_dir(&root);
    let request = Request::new(child::VERB, body, "ctl");
    ipc::request_with_token_file(&dir, &root, &request)
        .await
        .map_err(|error| format!("the root session {root} is not running ({error})"))
}

/// The root of `id` (following `parent` links on disk) and its depth.
pub fn root_of(id: &str) -> (String, u32) {
    let mut current = id.to_string();
    let mut depth = 0;
    for _ in 0..16 {
        match head(&crate::paths::session_dir(&current)).and_then(|head| head.parent) {
            Some(parent) => {
                current = parent;
                depth += 1;
            }
            None => break,
        }
    }
    (current, depth)
}

// -------------------------------------------------------------------- ls

/// One row of `kelir ls`.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    pub parent: Option<String>,
    pub depth: u32,
    pub kind: String,
    pub state: String,
    pub provider: String,
    pub model: String,
    pub cost: Option<f64>,
    pub hit_ratio: Option<f64>,
    pub task: String,
    pub children: Vec<Node>,
}

struct Head {
    parent: Option<String>,
}

fn head(dir: &Path) -> Option<Head> {
    use std::io::BufRead as _;
    let file = std::fs::File::open(dir.join("events.jsonl")).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(file).read_line(&mut line).ok()?;
    let event: LogEvent = serde_json::from_str(line.trim()).ok()?;
    match event.body {
        EventBody::SessionStarted { parent, .. } => Some(Head { parent }),
        _ => None,
    }
}

/// What `ls` shows for one session, from its log. `alive` says whether its
/// socket answers.
pub fn node_from(id: &str, events: &[LogEvent], alive: bool) -> Option<Node> {
    let (parent, depth, provider, model) = events.iter().find_map(|event| match &event.body {
        EventBody::SessionStarted {
            parent,
            depth,
            provider,
            model,
            ..
        } => Some((parent.clone(), *depth, provider.clone(), model.clone())),
        _ => None,
    })?;
    // A `model` verb swaps the model; the last step says what it is now.
    let model = events
        .iter()
        .rev()
        .find_map(|event| match &event.body {
            EventBody::StepStarted { model, .. } => Some(model.clone()),
            _ => None,
        })
        .unwrap_or(model);
    let (kind, task) = events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::User { text, from, .. } => Some((
                match from.as_str() {
                    "brief" => child::KIND_BRIEF,
                    "parent" => child::KIND_CHILD,
                    _ => "session",
                }
                .to_string(),
                text.inline_str().unwrap_or("").to_string(),
            )),
            _ => None,
        })
        .unwrap_or_else(|| ("session".into(), String::new()));
    let tally = crate::spend::Tally::from_events(events, Default::default());
    let ended = matches!(
        events.last().map(|event| &event.body),
        Some(EventBody::SessionEnded { .. })
    );
    let state = if alive {
        match tally.state.as_str() {
            "" => "idle".to_string(),
            other => other.to_string(),
        }
    } else if ended {
        "ended".to_string()
    } else {
        "crashed".to_string()
    };
    let hit_ratio = (tally.prompt > 0).then(|| tally.cached as f64 / tally.prompt as f64);
    Some(Node {
        id: id.to_string(),
        parent,
        depth,
        kind,
        state,
        provider,
        model,
        cost: (tally.by_provider + tally.by_table > 0).then_some(tally.cost),
        hit_ratio,
        task: child::one_line(&task, 80),
        children: Vec::new(),
    })
}

/// Arrange flat nodes into trees. A node whose parent is not in the set is a
/// root of its own tree. Children keep the order they were given in.
pub fn build_tree(nodes: Vec<Node>) -> Vec<Node> {
    let ids: std::collections::HashSet<String> = nodes.iter().map(|node| node.id.clone()).collect();
    let mut by_parent: std::collections::HashMap<String, Vec<Node>> =
        std::collections::HashMap::new();
    let mut roots = Vec::new();
    for node in nodes {
        match node.parent.clone().filter(|parent| ids.contains(parent)) {
            Some(parent) => by_parent.entry(parent).or_default().push(node),
            None => roots.push(node),
        }
    }
    fn attach(node: &mut Node, by_parent: &mut std::collections::HashMap<String, Vec<Node>>) {
        let mut children = by_parent.remove(&node.id).unwrap_or_default();
        for child in &mut children {
            attach(child, by_parent);
        }
        node.children = children;
    }
    for root in &mut roots {
        attach(root, &mut by_parent);
    }
    roots
}

fn subtree_cost(node: &Node) -> Option<f64> {
    let mut total = node.cost;
    for child in &node.children {
        if let Some(cost) = subtree_cost(child) {
            total = Some(total.unwrap_or(0.0) + cost);
        }
    }
    total
}

fn money(cost: Option<f64>) -> String {
    match cost {
        Some(value) => format!("${value:.4}"),
        None => "-".into(),
    }
}

/// The tree as text: one line per session, children indented.
pub fn render(roots: &[Node]) -> String {
    let mut out = String::new();
    fn walk(node: &Node, level: usize, out: &mut String) {
        let hit = node
            .hit_ratio
            .map_or("-".to_string(), |ratio| format!("{:.0}%", ratio * 100.0));
        out.push_str(&format!(
            "{indent}{id}  {state:<8} d{depth} {kind:<7} {model}  {cost} cache {hit}  {task}\n",
            indent = "  ".repeat(level),
            id = node.id,
            state = node.state,
            depth = node.depth,
            kind = node.kind,
            model = node.model,
            cost = money(node.cost),
            task = node.task,
        ));
        for child in &node.children {
            walk(child, level + 1, out);
        }
    }
    for root in roots {
        walk(root, 0, &mut out);
        if !root.children.is_empty() {
            out.push_str(&format!(
                "{}  tree cost {}\n",
                " ".repeat(2),
                money(subtree_cost(root))
            ));
        }
    }
    if out.is_empty() {
        out.push_str("no sessions\n");
    }
    out
}

pub fn to_json(nodes: &[Node]) -> Value {
    fn one(node: &Node) -> Value {
        json!({
            "id": node.id,
            "parent": node.parent,
            "depth": node.depth,
            "kind": node.kind,
            "state": node.state,
            "provider": node.provider,
            "model": node.model,
            "cost_usd": node.cost,
            "tree_cost_usd": subtree_cost(node),
            "cache_hit_ratio": node.hit_ratio,
            "task": node.task,
            "children": node.children.iter().map(one).collect::<Vec<_>>(),
        })
    }
    Value::Array(nodes.iter().map(one).collect())
}

/// How many of the newest roots `kelir ls` shows outside a session.
const NEWEST_ROOTS: usize = 10;

/// `kelir ls [--json] [--all]`.
pub async fn ls(args: LsArgs) -> u8 {
    let ids = crate::paths::sessions_newest_first();
    // The parent of every session, from its first line only.
    let heads: Vec<(String, Option<String>)> = ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                head(&crate::paths::session_dir(id)).and_then(|head| head.parent),
            )
        })
        .collect();
    let in_tree = crate::envs::var("KELIR_ROOT").filter(|root| !root.is_empty());
    let wanted = select(&heads, in_tree.as_deref(), args.all);
    let mut nodes = Vec::new();
    let mut settled: Vec<(String, String)> = Vec::new();
    for id in &wanted {
        let dir = crate::paths::session_dir(id);
        let Ok(read) = crate::event::read_events(&dir.join("events.jsonl")) else {
            continue;
        };
        let ended = matches!(
            read.events.last().map(|event| &event.body),
            Some(EventBody::SessionEnded { .. })
        );
        for event in &read.events {
            if let EventBody::ChildSettled {
                session, outcome, ..
            } = &event.body
            {
                settled.push((session.clone(), outcome.clone()));
            }
        }
        let alive = !ended && ipc::connect(&dir, id).await.is_ok();
        if let Some(node) = node_from(id, &read.events, alive) {
            nodes.push(node);
        }
    }
    // A session that is not running ended however its parent's registry says it
    // did (a kill leaves no `session_ended` behind).
    let outcomes: std::collections::HashMap<String, String> = settled.into_iter().collect();
    for node in &mut nodes {
        if matches!(node.state.as_str(), "crashed" | "ended")
            && let Some(outcome) = outcomes.get(&node.id)
        {
            node.state = outcome.clone();
        }
    }
    // Oldest first inside a tree so a parent reads before what it started.
    nodes.reverse();
    let mut roots = build_tree(nodes);
    roots.reverse();
    if args.json {
        println!("{}", to_json(&roots));
    } else {
        print!("{}", render(&roots));
    }
    0
}

/// Which sessions to show: the tree under `root` (inside a session), every
/// session (`--all`), or the newest roots and what is below them. `heads` is
/// newest first.
pub fn select(heads: &[(String, Option<String>)], root: Option<&str>, all: bool) -> Vec<String> {
    let below = |top: &[String]| -> Vec<String> {
        let mut set: Vec<String> = top.to_vec();
        loop {
            let before = set.len();
            for (id, parent) in heads {
                if !set.contains(id) && parent.as_ref().is_some_and(|parent| set.contains(parent)) {
                    set.push(id.clone());
                }
            }
            if set.len() == before {
                break;
            }
        }
        heads
            .iter()
            .filter(|(id, _)| set.contains(id))
            .map(|(id, _)| id.clone())
            .collect()
    };
    if let Some(root) = root {
        return below(&[root.to_string()]);
    }
    if all {
        return heads.iter().map(|(id, _)| id.clone()).collect();
    }
    let roots: Vec<String> = heads
        .iter()
        .filter(|(_, parent)| parent.is_none())
        .take(NEWEST_ROOTS)
        .map(|(id, _)| id.clone())
        .collect();
    below(&roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent: Option<&str>, depth: u32) -> Node {
        Node {
            id: id.into(),
            parent: parent.map(str::to_string),
            depth,
            kind: "session".into(),
            state: "running".into(),
            provider: "p".into(),
            model: "m".into(),
            cost: Some(0.01),
            hit_ratio: Some(0.5),
            task: "t".into(),
            children: Vec::new(),
        }
    }

    #[test]
    fn a_tree_is_built_from_parent_links_and_orphans_are_roots() {
        let tree = build_tree(vec![
            node("a", None, 0),
            node("b", Some("a"), 1),
            node("c", Some("b"), 2),
            node("d", Some("a"), 1),
            node("lost", Some("gone"), 1),
        ]);
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].id, "a");
        assert_eq!(tree[0].children.len(), 2);
        assert_eq!(tree[0].children[0].children[0].id, "c");
        assert_eq!(tree[1].id, "lost");
    }

    #[test]
    fn rendering_indents_children_and_totals_the_tree() {
        let text = render(&build_tree(vec![
            node("a", None, 0),
            node("b", Some("a"), 1),
        ]));
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("a  running  d0"), "{text}");
        assert!(lines[1].starts_with("  b  running  d1"), "{text}");
        assert!(lines[1].contains("cache 50%"), "{text}");
        assert!(lines[2].contains("tree cost $0.0200"), "{text}");
        assert_eq!(render(&[]), "no sessions\n");
    }

    #[test]
    fn the_json_tree_nests_children_and_carries_every_column() {
        let json = to_json(&build_tree(vec![
            node("a", None, 0),
            node("b", Some("a"), 1),
        ]));
        assert_eq!(json[0]["children"][0]["id"], "b");
        for key in [
            "id",
            "parent",
            "depth",
            "kind",
            "state",
            "model",
            "cost_usd",
            "tree_cost_usd",
            "cache_hit_ratio",
            "task",
            "children",
        ] {
            assert!(json[0].get(key).is_some(), "missing {key}");
        }
        assert_eq!(json[0]["tree_cost_usd"], json!(0.02));
    }

    #[test]
    fn selection_follows_the_tree_under_a_root() {
        let heads: Vec<(String, Option<String>)> =
            [("d", Some("c")), ("c", Some("a")), ("b", None), ("a", None)]
                .into_iter()
                .map(|(id, parent)| (id.to_string(), parent.map(str::to_string)))
                .collect();
        assert_eq!(select(&heads, Some("a"), false), vec!["d", "c", "a"]);
        assert_eq!(select(&heads, None, false).len(), 4);
        assert_eq!(select(&heads, None, true).len(), 4);
    }
}
