//! Root registry operations and child settling.

use serde_json::{Value, json};
use std::path::Path;
use tokio::sync::oneshot;

use crate::child::{self, ChildNote, Entry, Handoff, KIND_BRIEF, Registry};
use crate::core::session::types::Session;
use crate::features::children::{Plan, REMOTE_TEXT, ask_loop, ask_root, internal, state_of};
use crate::ipc::{Request, Response};
use crate::verbs::Verb;

impl Session {
    /// Kill the live descendants of `asker` that `target` (or `tree`) names.
    pub(crate) fn kill_below(&mut self, asker: &str, target: Option<&str>, tree: bool) -> Response {
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
    pub(crate) fn live_children(&self) -> usize {
        if self.kids.is_root() {
            self.kids.registry.live_descendants()
        } else {
            self.kids.running.len()
        }
    }
}

/// The message a background child's completion becomes.
pub(crate) fn wake_text(note: &ChildNote) -> String {
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
pub(crate) async fn run_child(plan: Plan, reply: oneshot::Sender<Response>) {
    let mut reply = Some(reply);
    let refuse = |reply: &mut Option<oneshot::Sender<Response>>, message: String| {
        if let Some(reply) = reply.take() {
            let _ = reply.send(Response::error(message));
        }
    };

    // 1. A slot, from the root's registry.
    let admit_args = json!({
        "kind": plan.kind,
        "background": plan.background,
        "task": child::clip(&plan.task, REMOTE_TEXT),
        "model": plan.model,
    });
    let root_is_me = plan.me == plan.root;
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
    let (child_id, session) = (text("child_id"), text("session"));
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
pub(crate) fn signalled(status: &std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal().is_some()
}

#[cfg(not(unix))]
pub(crate) fn signalled(_status: &std::process::ExitStatus) -> bool {
    false
}

/// The last lines of the child host's stderr, for a child that died without
/// saying why.
pub(crate) fn host_error(dir: &Path) -> String {
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
pub(crate) async fn settle_child(
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
    let root_is_me = plan.me == plan.root;
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
    pub(crate) fn task_from(&self) -> &'static str {
        if self.kids.root.is_none() {
            if crate::channel::is_channel_mode() {
                return "channel";
            }
            self.run.task_from
        } else if self.kids.brief {
            "brief"
        } else {
            "parent"
        }
    }

    /// The `child` verb, or `brief` as `kelir ctl <session> brief "<question>"`
    /// (validated by the verb parser, then run like `kelir brief`).
    pub(crate) fn child_or_brief(&mut self, request: Request, reply: oneshot::Sender<Response>) {
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
    /// A resumed root cannot reach what an earlier process started (those children leave with their
    /// parent), so the log's unsettled children are settled `lost` and the registry starts empty.
    pub(crate) fn settle_lost_children(&mut self) {
        if !self.kids.is_root() {
            return;
        }
        let (registry, lost) =
            Registry::from_log(&self.log.id, self.cfg.children, &self.log.events);
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
    pub(crate) fn child_list(&self) -> Value {
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
