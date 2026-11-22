//! The session side of children: the verbs `kelir run --child`, `kelir brief` and `kelir
//! kill` send, the root's registry operations, and the task that starts a child host, waits
//! for it and settles it.
//! One verb (`child`) with an `op`:

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::child::{
    self, Admission, Caps, ChildNote, Entry, Handoff, KIND_BRIEF, KIND_CHILD, Lineage, NewChild,
    Registry, RootChannel,
};

use crate::core::session::types::Session;
use crate::core::session::util::new_session_id;
use crate::event::{EventBody, LogEvent, Payload};
use crate::features::children_ops::{run_child, wake_text};
use crate::ipc::{self, Incoming, Request, Response};
use crate::tui::{View, note_event};
use crate::verbs::{Pending, Verb};

/// What the session keeps about the family it is in.
pub(crate) struct Kids {
    pub lineage: Lineage,
    /// The registry: only the root's has entries.
    pub registry: Registry,
    /// How a non-root session reaches the root. Never exported, never logged.
    pub root: Option<RootChannel>,
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
            root: handoff.map(|handoff| RootChannel {
                sock: handoff.root_sock.clone(),
                token: handoff.root_token.clone(),
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
pub(crate) struct Plan {
    pub(crate) kind: &'static str,
    pub(crate) task: String,
    pub(crate) background: bool,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) cwd: String,
    pub(crate) me: String,
    pub(crate) inbox: mpsc::UnboundedSender<Incoming>,
    pub(crate) key: String,
    /// How to reach the root: the session's own socket and token when it is
    /// the root, the root's socket and token otherwise. The caller's own session
    /// id identifies it to the root; no per-child secret is needed.
    pub(crate) root: String,
    pub(crate) root_sock: String,
    pub(crate) root_token: String,
    pub(crate) groups: crate::proc::SessionGroups,
    /// A brief: the by-value copy of the log, and the cache id it shares.
    pub(crate) snapshot: Option<(PathBuf, Vec<LogEvent>)>,
}

/// How long a child process seen gone may wait to be settled by its starter
/// with its real outcome before the root's reaper calls it `lost`.
const DEAD_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// The most task or result text one request to the root carries.
pub(crate) const REMOTE_TEXT: usize = 16 * 1024;

pub(crate) fn internal(key: &str, mut args: Value) -> Request {
    args["key"] = json!(key);
    Request::new(child::VERB, args, child::FROM_INTERNAL)
}

pub(crate) async fn ask_loop(
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

pub(crate) async fn ask_root(plan: &Plan, mut args: Value) -> Result<Response, String> {
    args["session"] = json!(plan.me);
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

pub(crate) fn state_of(response: Response) -> Result<Value, String> {
    if response.ok {
        Ok(response.state.unwrap_or(Value::Null))
    } else {
        Err(response.error.unwrap_or_else(|| "refused".into()))
    }
}

impl Session {
    // ------------------------------------------------------------- entry

    /// The `child` verb, and `brief` from `kelir ctl`.
    pub(crate) fn child_verb(&mut self, request: Request, reply: oneshot::Sender<Response>) {
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

    pub(crate) fn child_start(&mut self, op: &str, args: &Value, reply: oneshot::Sender<Response>) {
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
        let Some(inbox) = self.inbox.clone() else {
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
            .unwrap_or_else(|| self.provider.provider_name.clone());
        let model = Some(text("model"))
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| self.provider.model.clone());
        let snapshot = brief.then(|| {
            let cut = child::snapshot_cut(&self.log.events);
            (self.log.dir.clone(), self.log.events[..cut].to_vec())
        });
        let (root_sock, root_token) = match &self.kids.root {
            Some(link) => (link.sock.clone(), link.token.clone()),
            None => (self.ctx.sock.clone(), self.ctx.token.clone()),
        };
        let plan = Plan {
            kind: if brief { KIND_BRIEF } else { KIND_CHILD },
            task,
            background: args.get("background").and_then(Value::as_bool) == Some(true) && !brief,
            provider,
            model,
            cwd,
            me: self.log.id.clone(),
            inbox,
            key: self.kids.key.clone(),
            root: self.kids.lineage.root.clone(),
            root_sock,
            root_token,
            groups: self.ctx.groups.clone(),
            snapshot,
        };
        tokio::spawn(run_child(plan, reply));
    }

    // ---------------------------------------------------- the root's registry

    /// Settle the children whose process is gone (a crashed child must not keep
    pub(crate) fn reap_children(&mut self) {
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

    pub(crate) fn child_internal(&mut self, op: &str, args: &Value) -> Response {
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        match op {
            "admit-local" => {
                let me = self.log.id.clone();
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
            "up-local" => self.registry_up(&self.log.id.clone(), args),
            "settle-local" => self.registry_settle(&self.log.id.clone(), args),
            "note-started" => {
                let child_id = text("child_id");
                let session = text("session");
                let depth = args.get("depth").and_then(Value::as_u64).unwrap_or(0) as u32;
                let kind = text("kind");
                let task = Payload::text(text("task"));
                let background = args.get("background").and_then(Value::as_bool) == Some(true);
                let model = text("model");
                let body = crate::event::call_start(
                    &child_id,
                    "child",
                    crate::event::CallInfo::child(
                        task,
                        &session,
                        &self.log.id,
                        depth,
                        &kind,
                        background,
                        &model,
                    ),
                );
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
                    parent: self.log.id.clone(),
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

    /// An operation a descendant sent over this (root) session's socket: the token got it here,
    /// and the caller's own session id says what it may touch. Only a descendant (live or settled)
    /// of this root is allowed; same-user processes are trusted by design.
    pub(crate) fn child_remote(&mut self, op: &str, args: &Value) -> Response {
        if !self.kids.is_root() {
            return Response::error("child: only the root session keeps the registry");
        }
        let who = args
            .get("session")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !self.kids.registry.is_descendant(&self.log.id, &who) {
            return Response::error("refused: this session is not a child of this root");
        }
        match op {
            "admit" => self.registry_admit(&who, args),
            "up" => self.registry_up(&who, args),
            "settle" => self.registry_settle(&who, args),
            _ => Response::error("child: unknown operation"),
        }
    }

    pub(crate) fn registry_admit(&mut self, parent: &str, args: &Value) -> Response {
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        self.reap_children();
        let session = new_session_id();
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
        let body = crate::event::call_start(
            &admission.child_id,
            "child",
            crate::event::CallInfo::child(
                Payload::text(task),
                &admission.session,
                parent,
                admission.depth,
                kind,
                background,
                &model,
            ),
        );
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
            }),
        )
    }

    pub(crate) fn registry_up(&mut self, who: &str, args: &Value) -> Response {
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

    pub(crate) fn registry_settle(&mut self, who: &str, args: &Value) -> Response {
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

    pub(crate) fn log_child_settled(
        &mut self,
        entry: &Entry,
        outcome: &str,
        exit_code: Option<i32>,
        result: &str,
    ) {
        let body = crate::event::call_settle(
            &entry.child_id,
            "child",
            outcome,
            crate::event::CallSettle::default()
                .exit_code(exit_code)
                .result(Payload::text(result)),
        );
        if self.record(body).is_ok() {
            self.sync();
        }
    }

    pub(crate) fn queue_child_note(
        &mut self,
        entry: &Entry,
        outcome: &str,
        exit_code: Option<i32>,
        result: &str,
    ) {
        if entry.parent != self.log.id {
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

    /// and, for a background child, the message that wakes the model.
    pub(crate) fn note_child(&mut self, note: &ChildNote) -> Result<bool, String> {
        let settled = self.log.events.iter().any(|event| {
            matches!(&event.body, EventBody::CallSettled(settle)
                if settle.kind == "child" && settle.id == note.child_id)
        });
        if !settled {
            self.record(crate::event::call_settle(
                &note.child_id,
                "child",
                &note.outcome,
                crate::event::CallSettle::default()
                    .exit_code(note.exit_code)
                    .result(Payload::text(note.result.clone())),
            ))?;
            self.notify("child_settled", &note.child_id);
        }
        if !note.background || note.parent != self.log.id {
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
        self.emit(View::Log(note_event(format!(
            "» {}",
            crate::util::one_line_visible(&text, 200)
        ))));
        self.sync();
        Ok(true)
    }

    // -------------------------------------------------------------- kill

    /// `kelir kill`: stop descendants of the asker. In the root it is decided here; elsewhere it
    /// is forwarded with the session's token and its own session id (the root decides what is
    /// below whom).
    pub(crate) fn child_kill(&mut self, args: &Value, reply: oneshot::Sender<Response>) {
        let target = args
            .get("target")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .map(str::to_string);
        let mut tree = args.get("tree").and_then(Value::as_bool) == Some(true);
        // A human naming the root they are steering means everything below it.
        if args.get("human").and_then(Value::as_bool) == Some(true)
            && target.as_deref() == Some(self.log.id.as_str())
        {
            tree = true;
        }
        if target.is_none() && !tree {
            let _ = reply.send(Response::error("kill: name a session, or pass --tree"));
            return;
        }
        if self.kids.is_root() {
            // A local caller (`kelir children kill` in the root's own pane) has no session field;
            // it is the root. A forwarded call always carries its caller's id.
            let asker = args
                .get("session")
                .and_then(Value::as_str)
                .unwrap_or(self.log.id.as_str())
                .to_string();
            if asker != self.log.id && !self.kids.registry.is_descendant(&self.log.id, &asker) {
                let _ = reply.send(Response::error(
                    "refused: this session is not a child of this root",
                ));
                return;
            }
            let answer = self.kill_below(&asker, target.as_deref(), tree);
            let _ = reply.send(answer);
            return;
        }
        let Some(link) = self.kids.root.clone() else {
            let _ = reply.send(Response::error(
                "kill: this session has no route to its root",
            ));
            return;
        };
        let forwarded =
            json!({"op": "kill", "target": target, "tree": tree, "session": self.log.id.clone()});
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
}

/// Ask the root's own loop to reap dead children, every second.
pub(crate) fn watch_children(tx: mpsc::UnboundedSender<Incoming>, key: String) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let (reply, _answer) = oneshot::channel();
            let request = Request::new(
                crate::child::VERB,
                json!({"op": "reap", "key": key}),
                crate::child::FROM_INTERNAL,
            );
            if tx
                .send(Incoming {
                    request,
                    reply,
                    action: None,
                })
                .is_err()
            {
                return;
            }
        }
    });
}

/// A child session ends when its parent does.
pub(crate) fn watch_parent(
    handoff: crate::child::Handoff,
    tx: mpsc::UnboundedSender<Incoming>,
    token: String,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if crate::child::parent_alive(&handoff) {
                continue;
            }
            let (reply, _answer) = oneshot::channel();
            let _ = tx.send(Incoming {
                request: Request::new("end", json!({}), "parent-gone").with_token(Some(token)),
                reply,
                action: None,
            });
            // The end verb stops the session cleanly; if something is stuck,
            // leave anyway.
            tokio::time::sleep(Duration::from_secs(5)).await;
            std::process::exit(143);
        }
    });
}
