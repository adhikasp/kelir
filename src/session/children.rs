//! The session side of children (M7): the verbs `kelir run --child`,
//! `kelir brief` and `kelir kill` send, the root's registry operations, and the
//! task that starts a child host, waits for it and settles it.
//!
//! One verb (`child`) with an `op`:
//!
//! | op | from | does |
//! |---|---|---|
//! | `run`, `brief`, `kill` | a tool child or a human (this session's token) | the request |
//! | `admit`, `up`, `settle`, `kill` | a descendant session (the root's token **and** its link) | the registry |
//! | `admit-local`, `up-local`, `settle-local`, `note-started`, `note-settled` | this session's own tasks (a private key) | the same, without a socket |
//!
//! The root is the only writer of the registry and of its own log. A parent
//! that is not the root also records its own children in its own log, because
//! that is where it wakes from and resumes.

use std::collections::HashMap;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::*;
use crate::child::{
    self, Admission, Caps, ChildNote, Entry, Handoff, KIND_BRIEF, KIND_CHILD, Lineage, NewChild,
    Registry, RootLink,
};

/// What the session keeps about the family it is in.
pub(super) struct Kids {
    pub lineage: Lineage,
    /// The registry: only the root's has entries.
    pub registry: Registry,
    /// How a non-root session reaches the root. Never exported, never logged.
    pub link: Option<RootLink>,
    /// The nonce this session's own tasks sign their internal requests with.
    pub key: String,
    /// This session is a brief: `edit` and `compact` are refused by result.
    pub brief: bool,
    /// Children this session started that are still running: child id to
    /// session id.
    pub running: HashMap<String, String>,
}

impl Kids {
    pub fn new(id: &str, caps: Caps, handoff: Option<&Handoff>) -> Kids {
        let lineage = match handoff {
            Some(handoff) => Lineage {
                root: handoff.root.clone(),
                parent: Some(handoff.parent.clone()),
                depth: handoff.depth,
                me: id.to_string(),
            },
            None => Lineage {
                root: id.to_string(),
                parent: None,
                depth: 0,
                me: id.to_string(),
            },
        };
        Kids {
            registry: Registry::new(&lineage.root, caps),
            link: handoff.map(|handoff| RootLink {
                sock: handoff.root_sock.clone(),
                token: handoff.root_token.clone(),
                link: handoff.link.clone(),
            }),
            key: crate::trust::random_hex(16),
            brief: handoff.is_some_and(|handoff| handoff.kind == KIND_BRIEF),
            lineage,
            running: HashMap::new(),
        }
    }

    pub fn is_root(&self) -> bool {
        self.lineage.parent.is_none()
    }

    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Everything the spawning task needs, copied out of the session so the task
/// owns it.
struct Plan {
    kind: &'static str,
    task: String,
    background: bool,
    provider: String,
    model: String,
    cwd: String,
    me: String,
    inbox: mpsc::UnboundedSender<Incoming>,
    key: String,
    /// How to reach the root: the session's own socket and token when it is
    /// the root, the link it was handed otherwise.
    root: String,
    root_sock: String,
    root_token: String,
    link: Option<String>,
    groups: crate::proc::SessionGroups,
    /// A brief: the by-value copy of the log, and the cache id it shares.
    snapshot: Option<(PathBuf, Vec<LogEvent>)>,
}

/// How long a child process seen gone may wait to be settled by its starter
/// with its real outcome before the root's reaper calls it `lost`.
const DEAD_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// The most task or result text one request to the root carries.
const REMOTE_TEXT: usize = 16 * 1024;

fn internal(key: &str, mut args: Value) -> Request {
    args["key"] = json!(key);
    Request::new(child::VERB, args, child::FROM_INTERNAL)
}

async fn ask_loop(
    inbox: &mpsc::UnboundedSender<Incoming>,
    request: Request,
) -> Result<Response, String> {
    let (reply, answer) = oneshot::channel();
    inbox
        .send(Incoming {
            request,
            reply,
            action: None,
        })
        .map_err(|_| "the session is ending".to_string())?;
    answer
        .await
        .map_err(|_| "the session did not answer".to_string())
}

async fn ask_root(plan: &Plan, mut args: Value) -> Result<Response, String> {
    args["link"] = json!(plan.link.clone().unwrap_or_default());
    let request =
        Request::new(child::VERB, args, "child").with_token(Some(plan.root_token.clone()));
    ipc::request_value_within(
        &plan.root_sock,
        &request,
        std::time::Duration::from_secs(30),
    )
    .await
    .map_err(|error| format!("cannot reach the root session: {}", error.message()))
}

fn state_of(response: Response) -> Result<Value, String> {
    if response.ok {
        Ok(response.state.unwrap_or(Value::Null))
    } else {
        Err(response.error.unwrap_or_else(|| "refused".into()))
    }
}

impl Session {
    // ------------------------------------------------------------- entry

    /// The `child` verb, and `brief` from `kelir ctl`.
    pub(super) fn child_verb(&mut self, request: Request, reply: oneshot::Sender<Response>) {
        let op = request
            .args
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let own = request.from == child::FROM_INTERNAL
            && request
                .args
                .get("key")
                .and_then(Value::as_str)
                .is_some_and(|key| crate::trust::same(key, &self.kids.key));
        match op.as_str() {
            "admit-local" | "up-local" | "settle-local" | "note-started" | "note-settled"
            | "running" | "reap" => {
                if !own {
                    let _ = reply.send(Response::error("child: an internal operation"));
                    return;
                }
                let answer = self.child_internal(&op, &request.args);
                let _ = reply.send(answer);
            }
            "admit" | "up" | "settle" => {
                let answer = self.child_remote(&op, &request.args);
                let _ = reply.send(answer);
            }
            "kill" => self.child_kill(&request.args, reply),
            "run" | "brief" => self.child_start(&op, &request.args, reply),
            other => {
                let _ = reply.send(Response::error(format!(
                    "child: unknown operation `{other}`"
                )));
            }
        }
    }

    // ------------------------------------------------------------- start

    fn child_start(&mut self, op: &str, args: &Value, reply: oneshot::Sender<Response>) {
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let task = text("task");
        if task.trim().is_empty() {
            let _ = reply.send(Response::error("child: the task is empty"));
            return;
        }
        let Some(inbox) = self.mcp_inbox.clone() else {
            let _ = reply.send(Response::error(
                "child: the session is not serving children",
            ));
            return;
        };
        let brief = op == "brief";
        if self.kids.brief {
            // A side question reads; it does not start work of its own (the
            // snapshot may carry an old instruction to delegate).
            let _ = reply.send(Response::error(
                "refused: this is a brief, a read-only side question; it cannot start children or briefs. Answer from what you can read with bash",
            ));
            return;
        }
        let cwd = Some(text("cwd"))
            .filter(|cwd| !cwd.trim().is_empty())
            .unwrap_or_else(|| crate::util::path_string(&self.ctx.cwd));
        if !Path::new(&cwd).is_dir() {
            let _ = reply.send(Response::error(format!(
                "child: `{cwd}` is not a directory"
            )));
            return;
        }
        let provider = Some(text("provider"))
            .filter(|provider| !provider.trim().is_empty())
            .unwrap_or_else(|| self.provider_name.clone());
        let model = Some(text("model"))
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| self.model.clone());
        let snapshot = brief.then(|| {
            let cut = child::snapshot_cut(&self.events);
            (self.dir.clone(), self.events[..cut].to_vec())
        });
        let (root_sock, root_token, link) = match &self.kids.link {
            Some(link) => (
                link.sock.clone(),
                link.token.clone(),
                Some(link.link.clone()),
            ),
            None => (self.ctx.sock.clone(), self.ctx.token.clone(), None),
        };
        let plan = Plan {
            kind: if brief { KIND_BRIEF } else { KIND_CHILD },
            task,
            background: args.get("background").and_then(Value::as_bool) == Some(true) && !brief,
            provider,
            model,
            cwd,
            me: self.id.clone(),
            inbox,
            key: self.kids.key.clone(),
            root: self.kids.lineage.root.clone(),
            root_sock,
            root_token,
            link,
            groups: self.ctx.groups.clone(),
            snapshot,
        };
        tokio::spawn(run_child(plan, reply));
    }

    // ---------------------------------------------------- the root's registry

    /// Settle the children whose process is gone (a crashed child must not keep
    /// its slot) and write `child_settled` for each.
    pub(super) fn reap_children(&mut self) {
        let reaped = self.kids.registry.reap(
            &|entry| child::process_alive(entry.pid.unwrap_or(0), entry.started_ms),
            DEAD_GRACE,
        );
        for entry in reaped {
            let outcome = entry.outcome.clone().unwrap_or_else(|| "lost".into());
            let why = if outcome == "killed" {
                "stopped by `kelir kill`"
            } else {
                "the child process is gone (it crashed or was killed from outside)"
            };
            self.log_child_settled(&entry, &outcome, None, why);
            self.queue_child_note(&entry, &outcome, None, why);
        }
    }

    fn child_internal(&mut self, op: &str, args: &Value) -> Response {
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        match op {
            "admit-local" => {
                let me = self.id.clone();
                self.registry_admit(&me, args)
            }
            "reap" => {
                self.reap_children();
                Response::ok(child::VERB, 0, "now", json!({}))
            }
            "running" => {
                self.kids.running.insert(text("child_id"), text("session"));
                Response::ok(child::VERB, 0, "now", json!({}))
            }
            "up-local" => self.registry_up(&self.id.clone(), args),
            "settle-local" => self.registry_settle(&self.id.clone(), args),
            // A non-root parent writes its own `child_started`.
            "note-started" => {
                let body = EventBody::ChildStarted {
                    child_id: text("child_id"),
                    session: text("session"),
                    parent: self.id.clone(),
                    depth: args.get("depth").and_then(Value::as_u64).unwrap_or(0) as u32,
                    kind: text("kind"),
                    task: Payload::text(text("task")),
                    background: args.get("background").and_then(Value::as_bool) == Some(true),
                    model: text("model"),
                };
                match self.record(body) {
                    Ok(_) => {
                        self.sync();
                        Response::ok(child::VERB, 0, "now", json!({}))
                    }
                    Err(error) => Response::error(error),
                }
            }
            // A non-root parent's own settlement: queued like the root's.
            "note-settled" => {
                let note = ChildNote {
                    child_id: text("child_id"),
                    session: text("session"),
                    parent: self.id.clone(),
                    depth: args.get("depth").and_then(Value::as_u64).unwrap_or(0) as u32,
                    background: args.get("background").and_then(Value::as_bool) == Some(true),
                    outcome: text("outcome"),
                    exit_code: args
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .map(|c| c as i32),
                    result: text("result"),
                };
                self.kids.running.remove(&note.child_id);
                self.scheduler.push(Pending {
                    seq: 0,
                    verb: Verb::Child(Box::new(note)),
                    from: "child".into(),
                });
                Response::ok(child::VERB, 0, "now", json!({}))
            }
            _ => Response::error("child: unknown internal operation"),
        }
    }

    /// An operation a descendant sent over this (root) session's socket: the
    /// token got it here, and the link says which child it is and so what it
    /// may touch.
    fn child_remote(&mut self, op: &str, args: &Value) -> Response {
        if !self.kids.is_root() {
            return Response::error("child: only the root session keeps the registry");
        }
        let link = args.get("link").and_then(Value::as_str).unwrap_or("");
        let Some(who) = self
            .kids
            .registry
            .by_link(link)
            .map(|entry| entry.session.clone())
        else {
            return Response::error("refused: this is not the link of a live child");
        };
        match op {
            "admit" => self.registry_admit(&who, args),
            "up" => self.registry_up(&who, args),
            "settle" => self.registry_settle(&who, args),
            _ => Response::error("child: unknown operation"),
        }
    }

    fn registry_admit(&mut self, parent: &str, args: &Value) -> Response {
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        self.reap_children();
        let session = super::new_session_id();
        let kind = if text("kind") == KIND_BRIEF {
            KIND_BRIEF
        } else {
            KIND_CHILD
        };
        let background = args.get("background").and_then(Value::as_bool) == Some(true);
        let task = text("task");
        let model = text("model");
        let admitted = self.kids.registry.admit(NewChild {
            parent,
            kind,
            background,
            task: &task,
            model: &model,
            session: session.clone(),
        });
        let admission: Admission = match admitted {
            Ok(admission) => admission,
            Err(refusal) => return Response::error(refusal.message()),
        };
        let body = EventBody::ChildStarted {
            child_id: admission.child_id.clone(),
            session: admission.session.clone(),
            parent: parent.to_string(),
            depth: admission.depth,
            kind: kind.to_string(),
            task: Payload::text(task),
            background,
            model,
        };
        if let Err(error) = self.record(body) {
            // The slot was taken for a child that will never run.
            self.kids.registry.settle(&admission.child_id, "error");
            return Response::error(error);
        }
        self.sync();
        Response::ok(
            child::VERB,
            0,
            "now",
            json!({
                "child_id": admission.child_id,
                "session": admission.session,
                "depth": admission.depth,
                "link": admission.link,
            }),
        )
    }

    fn registry_up(&mut self, who: &str, args: &Value) -> Response {
        let child_id = args.get("child_id").and_then(Value::as_str).unwrap_or("");
        let pid = args.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32;
        let started = args.get("started_ms").and_then(Value::as_u64);
        let owned = self
            .kids
            .registry
            .get(child_id)
            .is_some_and(|entry| entry.parent == who);
        if !owned || pid == 0 {
            return Response::error("refused: not a child of the asker");
        }
        self.kids.registry.up(child_id, pid, started);
        let killed = self
            .kids
            .registry
            .get(child_id)
            .is_some_and(|entry| entry.killed);
        Response::ok(child::VERB, 0, "now", json!({"kill": killed}))
    }

    fn registry_settle(&mut self, who: &str, args: &Value) -> Response {
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let child_id = text("child_id");
        let owned = self
            .kids
            .registry
            .get(&child_id)
            .is_some_and(|entry| entry.parent == who);
        if !owned {
            return Response::error("refused: not a child of the asker");
        }
        let exit_code = args
            .get("exit_code")
            .and_then(Value::as_i64)
            .map(|c| c as i32);
        let result = text("result");
        match self.kids.registry.settle(&child_id, &text("outcome")) {
            Some(entry) => {
                let outcome = entry.outcome.clone().unwrap_or_default();
                self.log_child_settled(&entry, &outcome, exit_code, &result);
                self.queue_child_note(&entry, &outcome, exit_code, &result);
                Response::ok(child::VERB, 0, "now", json!({"outcome": outcome}))
            }
            // Already settled (reaped, or reported twice): the first word stands.
            None => {
                let outcome = self
                    .kids
                    .registry
                    .get(&child_id)
                    .and_then(|entry| entry.outcome.clone())
                    .unwrap_or_default();
                Response::ok(child::VERB, 0, "now", json!({"outcome": outcome}))
            }
        }
    }

    fn log_child_settled(
        &mut self,
        entry: &Entry,
        outcome: &str,
        exit_code: Option<i32>,
        result: &str,
    ) {
        let body = EventBody::ChildSettled {
            child_id: entry.child_id.clone(),
            session: entry.session.clone(),
            parent: entry.parent.clone(),
            depth: entry.depth,
            outcome: outcome.to_string(),
            exit_code,
            result: Payload::text(result),
        };
        if self.record(body).is_ok() {
            self.sync();
        }
    }

    /// The root's own children wake the root; a deeper descendant's settlement
    /// is the registry's business alone.
    fn queue_child_note(
        &mut self,
        entry: &Entry,
        outcome: &str,
        exit_code: Option<i32>,
        result: &str,
    ) {
        if entry.parent != self.id {
            return;
        }
        self.kids.running.remove(&entry.child_id);
        self.scheduler.push(Pending {
            seq: 0,
            verb: Verb::Child(Box::new(ChildNote {
                child_id: entry.child_id.clone(),
                session: entry.session.clone(),
                parent: entry.parent.clone(),
                depth: entry.depth,
                background: entry.background,
                outcome: outcome.to_string(),
                exit_code,
                result: result.to_string(),
            })),
            from: "child".into(),
        });
    }

    /// Apply a settlement at a boundary: the `child_settled` event (unless the
    /// registry already wrote it) and, for a background child, the message that
    /// wakes the model. Returns whether a message was written.
    pub(super) fn note_child(&mut self, note: &ChildNote) -> Result<bool, String> {
        let settled = self.events.iter().any(|event| {
            matches!(&event.body, EventBody::ChildSettled { child_id, .. } if child_id == &note.child_id)
        });
        if !settled {
            self.record(EventBody::ChildSettled {
                child_id: note.child_id.clone(),
                session: note.session.clone(),
                parent: note.parent.clone(),
                depth: note.depth,
                outcome: note.outcome.clone(),
                exit_code: note.exit_code,
                result: Payload::text(note.result.clone()),
            })?;
        }
        if !note.background || note.parent != self.id {
            self.sync();
            return Ok(false);
        }
        let text = wake_text(note);
        self.record(EventBody::User {
            text: Payload::text(text.clone()),
            from: "child".into(),
            delivery: "next".into(),
            attachments: Vec::new(),
        })?;
        self.emit(UiEvent::Info(format!("» {}", one_line(&text))));
        self.sync();
        Ok(true)
    }

    // -------------------------------------------------------------- kill

    /// `kelir kill`: stop descendants of the asker. In the root it is decided
    /// here; elsewhere it is forwarded with the link (the root decides what is
    /// below whom).
    fn child_kill(&mut self, args: &Value, reply: oneshot::Sender<Response>) {
        let target = args
            .get("target")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .map(str::to_string);
        let mut tree = args.get("tree").and_then(Value::as_bool) == Some(true);
        // A human naming the root they are steering means everything below it.
        if args.get("human").and_then(Value::as_bool) == Some(true)
            && target.as_deref() == Some(self.id.as_str())
        {
            tree = true;
        }
        if target.is_none() && !tree {
            let _ = reply.send(Response::error("kill: name a session, or pass --tree"));
            return;
        }
        if self.kids.is_root() {
            // A link means a descendant is asking through the root's socket.
            let link = args.get("link").and_then(Value::as_str).unwrap_or("");
            let asker = if link.is_empty() {
                self.id.clone()
            } else {
                match self
                    .kids
                    .registry
                    .by_link(link)
                    .map(|entry| entry.session.clone())
                {
                    Some(asker) => asker,
                    None => {
                        let _ = reply.send(Response::error(
                            "refused: this is not the link of a live child",
                        ));
                        return;
                    }
                }
            };
            let answer = self.kill_below(&asker, target.as_deref(), tree);
            let _ = reply.send(answer);
            return;
        }
        let Some(link) = self.kids.link.clone() else {
            let _ = reply.send(Response::error(
                "kill: this session has no link to its root",
            ));
            return;
        };
        let forwarded = json!({"op": "kill", "target": target, "tree": tree, "link": link.link});
        tokio::spawn(async move {
            let request =
                Request::new(child::VERB, forwarded, "child").with_token(Some(link.token));
            let answer = match ipc::request_value_within(
                &link.sock,
                &request,
                std::time::Duration::from_secs(30),
            )
            .await
            {
                Ok(answer) => answer,
                Err(error) => Response::error(format!(
                    "cannot reach the root session: {}",
                    error.message()
                )),
            };
            let _ = reply.send(answer);
        });
    }

    /// Kill the live descendants of `asker` that `target` (or `tree`) names.
    fn kill_below(&mut self, asker: &str, target: Option<&str>, tree: bool) -> Response {
        self.reap_children();
        let below: Vec<Entry> = self
            .kids
            .registry
            .descendants_of(asker)
            .into_iter()
            .filter(|entry| entry.live())
            .cloned()
            .collect();
        let victims: Vec<Entry> = match target {
            Some(target) if !tree => {
                let mut hits = self
                    .kids
                    .registry
                    .descendants_of(asker)
                    .into_iter()
                    .filter(|entry| {
                        entry.session == target
                            || entry.child_id == target
                            || (target.len() >= 8 && entry.session.starts_with(target))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if hits.is_empty() {
                    if target == asker {
                        return Response::error(
                            "refused: a session does not kill itself; end it with `kelir ctl <session> end`",
                        );
                    }
                    return Response::error(format!(
                        "refused: `{target}` is not a descendant of this session"
                    ));
                }
                if hits.len() > 1 {
                    return Response::error(format!("`{target}` matches {} children", hits.len()));
                }
                let hit = hits.remove(0);
                // The target and everything below it.
                let mut set = vec![hit.clone()];
                for entry in &below {
                    if self
                        .kids
                        .registry
                        .is_descendant(&hit.session, &entry.session)
                    {
                        set.push(entry.clone());
                    }
                }
                set.into_iter().filter(|entry| entry.live()).collect()
            }
            _ => below,
        };
        let mut stopped = Vec::new();
        for entry in victims {
            if self.kids.registry.mark_killed(&entry.child_id).is_none() {
                continue;
            }
            if let Some(pid) = entry.pid {
                child::kill_registered(pid, entry.started_ms);
            }
            stopped.push(entry.session);
        }
        let message = if stopped.is_empty() {
            "nothing to stop".to_string()
        } else {
            format!("stopped {}: {}", stopped.len(), stopped.join(", "))
        };
        Response::ok(
            child::VERB,
            0,
            "now",
            json!({"message": message, "stopped": stopped}),
        )
    }

    /// The live children below this session, for `status`.
    pub(super) fn live_children(&self) -> usize {
        if self.kids.is_root() {
            self.kids.registry.live_descendants()
        } else {
            self.kids.running.len()
        }
    }
}

/// The message a background child's completion becomes.
pub(super) fn wake_text(note: &ChildNote) -> String {
    let head = format!(
        "[child {} {}{}]",
        note.session,
        note.outcome,
        note.exit_code
            .filter(|code| *code != 0)
            .map(|code| format!(" exit {code}"))
            .unwrap_or_default()
    );
    let result = note.result.trim();
    if result.is_empty() {
        return format!("{head} (no final text)");
    }
    const INLINE: usize = 8_000;
    if result.len() <= INLINE {
        return format!("{head}\n{result}");
    }
    let mut end = INLINE;
    while !result.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{head}\n{}\n… [cut: {} bytes in all; the child's whole answer is in session {}'s log (`turn_ended.final_text`)]",
        &result[..end],
        result.len(),
        note.session
    )
}

// ------------------------------------------------------------- the task

/// Admit, start, wait for and settle one child. Runs on its own task; every
/// change to the log goes through the session's loop.
async fn run_child(plan: Plan, reply: oneshot::Sender<Response>) {
    let mut reply = Some(reply);
    let refuse = |reply: &mut Option<oneshot::Sender<Response>>, message: String| {
        if let Some(reply) = reply.take() {
            let _ = reply.send(Response::error(message));
        }
    };

    // 1. A slot, from the root's registry.
    // The root's line is at most 64 KB: its registry keeps a prefix of a long
    // task (this session's own log keeps all of it).
    let admit_args = json!({
        "kind": plan.kind,
        "background": plan.background,
        "task": child::clip(&plan.task, REMOTE_TEXT),
        "model": plan.model,
    });
    let root_is_me = plan.link.is_none();
    let admitted = if root_is_me {
        ask_loop(
            &plan.inbox,
            internal(&plan.key, json!({"op": "admit-local"}).merge(admit_args)),
        )
        .await
    } else {
        ask_root(&plan, json!({"op": "admit"}).merge(admit_args)).await
    };
    let admitted = match admitted.map(state_of) {
        Ok(Ok(state)) => state,
        Ok(Err(message)) | Err(message) => return refuse(&mut reply, message),
    };
    let text = |key: &str| admitted[key].as_str().unwrap_or("").to_string();
    let (child_id, session, link) = (text("child_id"), text("session"), text("link"));
    let depth = admitted["depth"].as_u64().unwrap_or(0) as u32;
    if !root_is_me {
        let _ = ask_loop(
            &plan.inbox,
            internal(
                &plan.key,
                json!({
                    "op": "note-started", "child_id": child_id, "session": session,
                    "depth": depth, "kind": plan.kind, "task": plan.task,
                    "background": plan.background, "model": plan.model,
                }),
            ),
        )
        .await;
    }

    // From here every way out settles the child.
    let settle = |outcome: String, exit_code: Option<i32>, result: String| {
        let (child_id, session) = (child_id.clone(), session.clone());
        let plan = &plan;
        async move {
            settle_child(
                plan, &child_id, &session, depth, &outcome, exit_code, &result,
            )
            .await
        }
    };

    // 2. A brief's snapshot.
    let dir = child::session_dir(&session);
    if let Some((source, events)) = &plan.snapshot
        && let Err(message) = child::seed_snapshot(source, &dir, events, &session, &plan.me, depth)
    {
        let outcome = settle("error".into(), None, message.clone()).await;
        let _ = outcome;
        return refuse(&mut reply, message);
    }

    // 3. The process.
    let handoff = Handoff {
        kind: plan.kind.to_string(),
        child_id: child_id.clone(),
        session: session.clone(),
        task: plan.task.clone(),
        model: Some(plan.model.clone()),
        provider: Some(plan.provider.clone()),
        cwd: plan.cwd.clone(),
        parent: plan.me.clone(),
        parent_pid: std::process::id(),
        parent_start_ms: child::start_ms(std::process::id()),
        depth,
        root: plan.root.clone(),
        root_sock: plan.root_sock.clone(),
        root_token: plan.root_token.clone(),
        link,
        cache_id: (plan.kind == KIND_BRIEF).then(|| plan.me.clone()),
    };
    let _ = crate::paths::create_private_dir(&dir);
    let mut host = match child::spawn_host(&handoff, &dir.join("host.err")).await {
        Ok(host) => host,
        Err(message) => {
            settle("error".into(), None, message.clone()).await;
            return refuse(&mut reply, message);
        }
    };
    plan.groups.keep(host.group.clone());
    let up = json!({
        "child_id": child_id, "pid": host.pid,
        "started_ms": child::start_ms(host.pid),
    });
    let upped = if root_is_me {
        ask_loop(
            &plan.inbox,
            internal(&plan.key, json!({"op": "up-local"}).merge(up)),
        )
        .await
    } else {
        ask_root(&plan, json!({"op": "up"}).merge(up)).await
    };
    // A kill that landed between the admission and now.
    if let Ok(Ok(state)) = upped.map(state_of)
        && state["kill"].as_bool() == Some(true)
    {
        host.group.kill();
    }
    let _ = ask_loop(
        &plan.inbox,
        internal(
            &plan.key,
            json!({"op": "running", "child_id": child_id, "session": session}),
        ),
    )
    .await;

    // 4. The answer a background caller waits for is a handle.
    if plan.background
        && let Some(reply) = reply.take()
    {
        let _ = reply.send(Response::ok(
            child::VERB,
            0,
            "now",
            json!({"child_id": child_id, "session": session, "depth": depth}),
        ));
    }

    // 5. Wait for it, or for the caller to go away (a stopped bash call).
    let waited = async {
        let mut out = String::new();
        if let Some(stdout) = host.child.stdout.take() {
            use tokio::io::AsyncReadExt as _;
            let mut bytes = Vec::new();
            let _ = stdout
                .take(child::RESULT_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .await;
            out = String::from_utf8_lossy(&bytes).into_owned();
        }
        let status = host.child.wait().await;
        (out, status)
    };
    let finished = match reply.as_mut() {
        Some(waiting) => {
            tokio::select! {
                done = waited => Some(done),
                _ = waiting.closed() => None,
            }
        }
        None => Some(waited.await),
    };
    let (outcome, exit_code, result) = match finished {
        Some((out, status)) => {
            let (code, signalled) = match &status {
                Ok(status) => (status.code().unwrap_or(-1), signalled(status)),
                Err(_) => (-1, false),
            };
            let result = out.trim_end().to_string();
            let result = if result.is_empty() && code != 0 {
                host_error(&dir)
            } else {
                result
            };
            (
                child::outcome_of(code, signalled).to_string(),
                Some(code),
                result,
            )
        }
        None => {
            // The caller is gone: nobody will read this child's answer.
            host.group.kill();
            let _ = host.child.wait().await;
            (
                "stopped".to_string(),
                None,
                "the call that started this child was stopped".into(),
            )
        }
    };
    let settled = settle(outcome, exit_code, result.clone()).await;
    if let Some(reply) = reply.take() {
        let exit = exit_code.unwrap_or(-1);
        let _ = reply.send(Response::ok(
            child::VERB,
            0,
            "now",
            child::answer(&child_id, &session, &settled, exit, &result),
        ));
    }
}

/// `{...}.merge({...})`: the second object's keys over the first's.
trait Merge {
    fn merge(self, other: Value) -> Value;
}

impl Merge for Value {
    fn merge(mut self, other: Value) -> Value {
        if let (Some(into), Value::Object(from)) = (self.as_object_mut(), other) {
            for (key, value) in from {
                into.insert(key, value);
            }
        }
        self
    }
}

#[cfg(unix)]
fn signalled(status: &std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal().is_some()
}

#[cfg(not(unix))]
fn signalled(_status: &std::process::ExitStatus) -> bool {
    false
}

/// The last lines of the child host's stderr, for a child that died without
/// saying why.
fn host_error(dir: &Path) -> String {
    let text = std::fs::read_to_string(dir.join("host.err")).unwrap_or_default();
    let tail: Vec<&str> = text.lines().rev().take(5).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    if tail.is_empty() {
        "the child ended without an answer".into()
    } else {
        format!("the child ended without an answer: {}", tail.join(" | "))
    }
}

/// Tell the registry (and this session's own loop) the child is over. Returns
/// the outcome the registry settled it with (a kill wins over the exit).
async fn settle_child(
    plan: &Plan,
    child_id: &str,
    session: &str,
    depth: u32,
    outcome: &str,
    exit_code: Option<i32>,
    result: &str,
) -> String {
    let body = json!({
        "child_id": child_id, "outcome": outcome,
        "exit_code": exit_code, "result": child::clip(result, REMOTE_TEXT),
    });
    let root_is_me = plan.link.is_none();
    let answered = if root_is_me {
        ask_loop(
            &plan.inbox,
            internal(&plan.key, json!({"op": "settle-local"}).merge(body)),
        )
        .await
    } else {
        ask_root(plan, json!({"op": "settle"}).merge(body)).await
    };
    let outcome = match answered.map(state_of) {
        Ok(Ok(state)) => state["outcome"].as_str().unwrap_or(outcome).to_string(),
        _ => outcome.to_string(),
    };
    if !root_is_me {
        let _ = ask_loop(
            &plan.inbox,
            internal(
                &plan.key,
                json!({
                    "op": "note-settled", "child_id": child_id, "session": session,
                    "depth": depth, "background": plan.background, "outcome": outcome,
                    "exit_code": exit_code, "result": result,
                }),
            ),
        )
        .await;
    }
    outcome
}

impl Session {
    /// Who a task's `user` event says it came from: the command line, the
    /// parent that started this child, or the parent asking a brief.
    pub(super) fn task_from(&self) -> &'static str {
        if self.kids.link.is_none() {
            "cli"
        } else if self.kids.brief {
            "brief"
        } else {
            "parent"
        }
    }

    /// The `child` verb, or `brief` as `kelir ctl <session> brief "<question>"`
    /// (validated by the verb parser, then run like `kelir brief`).
    pub(super) fn child_or_brief(&mut self, request: Request, reply: oneshot::Sender<Response>) {
        if request.verb == "brief" {
            return match crate::verbs::parse("brief", &request.args) {
                Ok(Verb::Brief { question }) => {
                    let _ = self.log_verb("brief", &request.args, &request.from, "now", None);
                    self.sync();
                    let args = json!({"op": "brief", "task": question});
                    self.child_start("brief", &args, reply);
                }
                Ok(_) => {}
                Err(message) => {
                    let _ = reply.send(Response::error(message));
                }
            };
        }
        self.child_verb(request, reply);
    }
}

impl Session {
    /// A resumed root cannot reach what an earlier process started (those
    /// children leave with their parent), so the log's unsettled children are
    /// settled `lost` and the registry starts empty.
    pub(super) fn settle_lost_children(&mut self) {
        if !self.kids.is_root() {
            return;
        }
        let (registry, lost) = Registry::from_log(&self.id, self.cfg.children, &self.events);
        self.kids.registry = registry;
        for entry in lost {
            self.log_child_settled(
                &entry,
                "lost",
                None,
                "the session was resumed; this child did not survive its parent",
            );
        }
    }
}

impl Session {
    /// The root's registry as `status` shows it: who, where in the tree, what
    /// state, which pid. Nothing secret (no link, no token).
    pub(super) fn child_list(&self) -> Value {
        Value::Array(
            self.kids
                .registry
                .entries()
                .iter()
                .map(|entry| {
                    json!({
                        "child_id": entry.child_id,
                        "session": entry.session,
                        "parent": entry.parent,
                        "depth": entry.depth,
                        "kind": entry.kind,
                        "background": entry.background,
                        "state": entry.outcome.clone().unwrap_or_else(|| "running".into()),
                        "pid": entry.pid,
                    })
                })
                .collect(),
        )
    }
}
