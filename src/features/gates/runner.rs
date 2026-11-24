use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::config::{DRAIN, STDERR_TAIL};
use super::decision::split_answer;
#[cfg(windows)]
use super::limits::JOBS;
use super::limits::{Slot, run_token};
use super::*;
#[cfg(windows)]
use std::sync::atomic::Ordering;

/// A built-in implemented in Rust. `run` is bounded by the gate timeout by the
/// runner, just like an external gate.
pub trait BuiltinGate: Send + Sync {
    fn events(&self) -> &[Event];
    fn run<'a>(
        &'a mut self,
        input: &'a GateInput,
        ctx: &'a GateCtx<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<GateAnswer, String>> + Send + 'a>>;
}

// ------------------------------------------------------------------- runner

/// The session's gates and what a run needs to know about the session.
pub struct Runner {
    defs: Vec<GateDef>,
    /// One stateful instance per built-in gate, kept across events.
    builtins: tokio::sync::Mutex<Vec<(String, Box<dyn BuiltinGate>)>>,
    /// Notes a built-in wrote while the session was borrowed.
    notes: std::sync::Mutex<Vec<String>>,
    shell: Vec<String>,
    cwd: PathBuf,
    session_id: String,
    session_dir: PathBuf,
    timeout: Duration,
}

impl Runner {
    /// Take the notes a built-in left for the session to record.
    pub fn take_notes(&self) -> Vec<String> {
        std::mem::take(&mut self.notes.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn new(
        defs: Vec<GateDef>,
        shell: Vec<String>,
        cwd: PathBuf,
        session_id: String,
        session_dir: PathBuf,
    ) -> Runner {
        Runner {
            defs,
            builtins: tokio::sync::Mutex::new(Vec::new()),
            notes: std::sync::Mutex::new(Vec::new()),
            shell,
            cwd,
            session_id,
            session_dir,
            timeout: timeout(),
        }
    }

    /// A runner with another timeout (the unit tests; the env hook is for the
    /// binary).
    pub fn with_timeout(mut self, timeout: Duration) -> Runner {
        self.timeout = timeout;
        self
    }

    pub fn defs(&self) -> &[GateDef] {
        &self.defs
    }

    /// Whether any gate would run at `event` for `tool`.
    pub fn has(&self, event: Event, tool: Option<&str>) -> bool {
        self.defs
            .iter()
            .any(|def| def.event == event && def.matches(tool))
    }

    /// Run every gate bound to `event` (for `tool`), in declaration order. A rewrite is seen by
    /// the gates after it; the first `deny` ends the chain (where `deny` means something).
    /// External and built-in gates meet here: both produce the same decision object.
    pub async fn run_event(
        &self,
        event: Event,
        tool: Option<&str>,
        turn: u32,
        mut payload: Value,
        ctx: Option<&GateCtx<'_>>,
    ) -> Chain {
        let mut chain = Chain::allow();
        let mut rewritten: Option<Value> = None;
        for (index, def) in self.defs.iter().enumerate() {
            if def.event != event || !def.matches(tool) {
                continue;
            }
            let input = GateInput::new(
                event,
                turn,
                &payload,
                &self.session_id,
                &self.session_dir,
                &self.cwd,
            );
            let started = Instant::now();
            let outcome = if let Some(name) = def.builtin.as_deref() {
                let slot = match Slot::take() {
                    Ok(slot) => slot,
                    Err(error) => {
                        let mut run = Self::empty_run(def, event, started, 0);
                        run.error = Some(error);
                        chain.runs.push(run);
                        continue;
                    }
                };
                let answer = {
                    let mut slots = self.builtins.lock().await;
                    let index = if let Some(index) = slots.iter().position(|(n, _)| n == name) {
                        index
                    } else {
                        let gate: Box<dyn BuiltinGate> = match name {
                            "hint" => match builtin::hint::Hint::new(def) {
                                Ok(hint) => Box::new(hint),
                                Err(why) => {
                                    drop(slots);
                                    let mut run = Self::empty_run(def, event, started, 0);
                                    run.error = Some(why);
                                    chain.runs.push(run);
                                    continue;
                                }
                            },
                            _ => {
                                drop(slots);
                                let mut run = Self::empty_run(def, event, started, 0);
                                run.error = Some(format!("unknown built-in `{name}`"));
                                chain.runs.push(run);
                                continue;
                            }
                        };
                        slots.push((name.to_string(), gate));
                        slots.len() - 1
                    };
                    let gate = &mut slots[index].1;
                    let note_sink = &self.notes;
                    let note: &(dyn Fn(&str) + Sync) = &|message| {
                        note_sink
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(message.to_string());
                    };
                    let ctx_with_notes =
                        ctx.map(|ctx| crate::features::gates::GateCtx { note, ..*ctx });
                    match ctx_with_notes.as_ref() {
                        Some(ctx) => {
                            tokio::time::timeout(self.timeout, gate.run(&input, ctx)).await
                        }
                        None => {
                            drop(slots);
                            drop(slot);
                            let mut run = Self::empty_run(def, event, started, 0);
                            run.error = Some("a built-in gate needs a session context".into());
                            chain.runs.push(run);
                            continue;
                        }
                    }
                };
                drop(slot);
                match answer {
                    Ok(Ok(answer)) => Ok((builtin::to_stdout(&answer), None)),
                    Ok(Err(why)) => Err((why, None)),
                    Err(_) => Err((
                        format!("timed out after {} ms", self.timeout.as_millis()),
                        None,
                    )),
                }
            } else {
                match self.execute(index, def, &input.0.to_string()).await {
                    Ok(stdout) => Ok((stdout, None)),
                    Err(error) => Err((error, None)),
                }
            };
            let ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            let mut run = Self::empty_run(def, event, started, ms);
            let (stdout, usage) = match outcome {
                Ok(value) => value,
                Err((error, usage)) => {
                    if let Some(usage) = usage {
                        chain.usages.push((def.name.clone(), usage));
                    }
                    run.error = Some(error);
                    chain.runs.push(run);
                    continue;
                }
            };
            let mut parsed = match parse_decision(&stdout) {
                Ok(parsed) => parsed,
                Err(error) => {
                    run.error = Some(error);
                    chain.runs.push(run);
                    continue;
                }
            };
            if let Some(usage) = usage {
                chain.usages.push((def.name.clone(), usage));
            }
            let (context, parsed_usage) = split_answer(&mut parsed);
            if let Some(context) = context {
                chain.contexts.push(context);
            }
            if let Some(usage) = parsed_usage {
                chain.usages.push((def.name.clone(), usage));
            }
            match parsed {
                Parsed::Allow { reason, .. } => run.reason = reason,
                Parsed::Deny { reason, .. } => {
                    run.decision = "deny".into();
                    run.reason = reason.clone();
                    if event.can_deny() {
                        chain.verdict = Verdict::Deny {
                            gate: def.name.clone(),
                            reason,
                        };
                        chain.runs.push(run);
                        return chain;
                    }
                    run.reason =
                        format!("{reason} (ignored: deny means nothing at {})", event.name());
                }
                Parsed::Rewrite { reason, input, .. } => {
                    run.reason = reason;
                    if !event.can_rewrite() {
                        run.error = Some(format!("rewrite means nothing at {}", event.name()));
                    } else {
                        match apply_rewrite(event, &def.name, &mut payload, &input) {
                            Ok(value) => {
                                run.decision = "rewrite".into();
                                run.rewritten = true;
                                rewritten = Some(value);
                            }
                            Err(error) => run.error = Some(error),
                        }
                    }
                }
            }
            chain.runs.push(run);
        }
        if let Some(value) = rewritten {
            chain.verdict = Verdict::Rewrite(value);
        }
        chain
    }

    fn empty_run(def: &GateDef, event: Event, _started: Instant, ms: u64) -> Run {
        Run {
            gate: def.name.clone(),
            event: event.name().into(),
            decision: "allow".into(),
            reason: String::new(),
            ms,
            rewritten: false,
            error: None,
        }
    }

    /// The JSON a gate reads on stdin: the session's fields, then the event's.
    pub fn stdin_for(&self, event: Event, turn: u32, payload: &Value) -> String {
        let mut object = serde_json::Map::new();
        object.insert("event".into(), json!(event.name()));
        object.insert("session".into(), json!(self.session_id));
        object.insert(
            "session_dir".into(),
            json!(crate::util::path_string(&self.session_dir)),
        );
        object.insert(
            "log".into(),
            json!(crate::util::path_string(
                &self.session_dir.join("events.jsonl")
            )),
        );
        object.insert("cwd".into(), json!(crate::util::path_string(&self.cwd)));
        object.insert("turn".into(), json!(turn));
        if let Some(fields) = payload.as_object() {
            for (key, value) in fields {
                object.insert(key.clone(), value.clone());
            }
        }
        Value::Object(object).to_string()
    }

    /// Start the gate, feed it `stdin`, and return its stdout, or why that
    /// failed (every `Err` is a fail-open).
    async fn execute(&self, index: usize, def: &GateDef, stdin: &str) -> Result<String, String> {
        let Some(program) = self.shell.first() else {
            return Err("no shell configured".into());
        };
        // Held until the gate and its tree are gone.
        let _slot = Slot::take()?;
        let shell_args: Vec<String> = self.shell.iter().skip(1).cloned().collect();
        let dir = self.session_dir.join("gates");
        let line = adapt_for_shell(&def.command, program);
        let input = crate::sys::shell::input(&line, &shell_args, &dir, &format!("g{index}"))?;
        let mut gate_args = shell_args.clone();
        match &input {
            crate::sys::shell::ShellInput::Argument(text) => {
                gate_args.push(text.clone());
            }
            crate::sys::shell::ShellInput::Script { path, keep_switch } => {
                let mut script_args =
                    crate::sys::shell::script_args(&shell_args, keep_switch.as_deref(), path);
                gate_args.append(&mut script_args);
            }
        }
        let token = run_token();
        let mut extra: BTreeMap<String, String> = BTreeMap::new();
        extra.insert(
            "KELIR_SESSION_DIR".into(),
            crate::util::path_string(&self.session_dir),
        );
        extra.insert("KELIR_GATE_EVENT".into(), def.event.name().into());
        extra.insert("KELIR_GATE_NAME".into(), def.name.clone());
        extra.insert(RUN_ENV.into(), token.clone());
        extra.extend(def.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        #[cfg(windows)]
        let job_name = {
            let n = JOBS.fetch_add(1, Ordering::SeqCst);
            Some(crate::features::bg::job_name(
                &self.session_id,
                &format!("gate-{}-{n}", std::process::id()),
            ))
        };
        #[cfg(not(windows))]
        let job_name = None;
        let spec = crate::sys::runner::Spec {
            cwd: Some(self.cwd.clone()),
            env: crate::sys::runner::Env::Scrubbed { extra },
            stdin: crate::sys::runner::Stdin::Bytes(stdin.as_bytes().to_vec()),
            timeout: Some(self.timeout),
            output_cap: OUTPUT_MAX,
            stderr_cap: 8 * 1024,
            no_window: true,
            job_name,
            unix_guard: None,
            ..crate::sys::runner::Spec::new(program, gate_args)
        };
        let (mut command, group) = crate::sys::runner::command(&spec)?;
        let mut child = crate::sys::runner::start(&mut command, &group)?;
        crate::sys::runner::feed_stdin(&spec, &mut child);
        #[cfg(unix)]
        let mut reaper = reap::Reaper::start(
            child.id().ok_or("the gate exited at once")?,
            &token,
            self.timeout + Duration::from_secs(15),
        );
        let stdout = child.stdout.take().ok_or("no stdout pipe")?;
        let stderr = child.stderr.take().ok_or("no stderr pipe")?;
        let out_task = tokio::spawn(crate::sys::runner::read_capped(stdout, OUTPUT_MAX));
        let err_task = tokio::spawn(crate::sys::runner::read_capped(stderr, 8 * 1024));

        let limit = self.timeout;
        let waited = tokio::time::timeout(limit, child.wait()).await;
        // The tree dies with the gate whatever happened: a gate that left a
        // background process behind leaves nothing.
        group.kill();
        // Whatever left the group (setsid, a double fork) goes too; the guard
        // that would have done it for a killed session is retired.
        #[cfg(unix)]
        reaper.reap(false);
        let status = match waited {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => return Err(format!("cannot wait for the gate: {error}")),
            Err(_) => {
                let _ = tokio::time::timeout(DRAIN, child.wait()).await;
                out_task.abort();
                err_task.abort();
                return Err(format!("timed out after {} ms", limit.as_millis()));
            }
        };
        let (out, overflow) = out_task.await.unwrap_or_default();
        let (err, _) = err_task.await.unwrap_or_default();
        if !status.success() {
            let tail = String::from_utf8_lossy(&err);
            let tail = tail.trim();
            let code = status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "a signal".into());
            return Err(if tail.is_empty() {
                format!("exited with {code}")
            } else {
                format!(
                    "exited with {code}: {}",
                    crate::util::truncate_chars(&tail.replace(['\r', '\n'], " "), STDERR_TAIL)
                )
            });
        }
        if overflow {
            return Err(format!("output over {OUTPUT_MAX} bytes"));
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

// ----------------------------------------------------------------- describe

/// The effective gate configuration as `observe` prints it: one line per
/// gate, never an environment value (the `env` table is named by key only).
pub fn describe(defs: &[GateDef]) -> Vec<Value> {
    defs.iter()
        .map(|def| {
            json!({
                "name": def.name,
                "event": def.event.name(),
                "origin": def.origin,
                "builtin": def.builtin,
                "command": crate::util::truncate_chars(&def.command, 200),
                "tools": def.tools,
                "env": def.env.keys().collect::<Vec<_>>(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::time::Duration;
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct Quiet(
        #[expect(dead_code, reason = "the guard is held for its Drop, never read")]
        tokio::sync::MutexGuard<'static, ()>,
    );

    impl Drop for Quiet {
        fn drop(&mut self) {
            if !std::thread::panicking() {
                assert_eq!(live(), 0, "a test left a gate process counted as alive");
            }
        }
    }

    fn def(event: Event, command: &str) -> GateDef {
        GateDef {
            name: name_from_command(command),
            event,
            command: command.to_string(),
            builtin: None,
            options: toml::Table::new(),
            tools: Vec::new(),
            env: BTreeMap::new(),
            origin: "test".into(),
        }
    }

    fn runner(dir: &Path, defs: Vec<GateDef>) -> Runner {
        std::fs::create_dir_all(dir).unwrap();
        Runner::new(
            defs,
            crate::config::default_shell(),
            dir.to_path_buf(),
            "ses_test".into(),
            dir.to_path_buf(),
        )
        .with_timeout(Duration::from_secs(20))
    }

    // --------------------------------------------------- real gate processes

    fn answers(temp: &Path, name: &str, body: &str) -> String {
        let path = temp.join(name);
        std::fs::write(&path, body).unwrap();
        format!(
            "bash \"{}\"",
            crate::util::path_string(&path).replace('\\', "/")
        )
    }

    async fn run_one(body: &str, event: Event, timeout: Duration) -> Chain {
        let _serial = Quiet(SERIAL.lock().await);
        let temp = tempfile::tempdir().unwrap();
        let command = answers(temp.path(), "gate.sh", body);
        runner(temp.path(), vec![def(event, &command)])
            .with_timeout(timeout)
            .run_event(event, None, 1, json!({}), None)
            .await
    }

    #[tokio::test]
    async fn a_chain_runs_in_order_and_a_deny_stops_it() {
        let _serial = Quiet(SERIAL.lock().await);
        let temp = tempfile::tempdir().unwrap();
        let rewrite = answers(
            temp.path(),
            "rewrite.sh",
            "cat > seen1.json\necho '{\"decision\":\"rewrite\",\"input\":{\"text\":\"second\"}}'\n",
        );
        let seen = answers(
            temp.path(),
            "seen.sh",
            "cat > seen2.json\necho '{\"decision\":\"deny\",\"reason\":\"stop here\"}'\n",
        );
        let never = answers(temp.path(), "never.sh", "echo ran > never.txt\n");
        let runner = runner(
            temp.path(),
            vec![
                def(Event::PromptSubmit, &rewrite),
                def(Event::PromptSubmit, &seen),
                def(Event::PromptSubmit, &never),
            ],
        );
        let chain = runner
            .run_event(Event::PromptSubmit, None, 1, json!({"text": "first"}), None)
            .await;
        assert_eq!(chain.runs.len(), 2, "{:?}", chain.runs);
        assert_eq!(chain.runs[0].decision, "rewrite");
        assert!(chain.runs[0].rewritten);
        assert_eq!(chain.runs[1].decision, "deny");
        assert_eq!(
            chain.verdict,
            Verdict::Deny {
                gate: "seen".into(),
                reason: "stop here".into()
            }
        );
        let second: Value =
            serde_json::from_str(&std::fs::read_to_string(temp.path().join("seen2.json")).unwrap())
                .unwrap();
        assert_eq!(second["text"], "second", "the next gate sees the rewrite");
        assert!(!temp.path().join("never.txt").exists());
    }

    #[tokio::test]
    async fn failing_gates_fail_open_but_report_how() {
        let _serial = Quiet(SERIAL.lock().await);
        let temp = tempfile::tempdir().unwrap();
        let scripts = [
            ("crash.sh", "echo boom >&2\nexit 3\n"),
            ("garbage.sh", "echo not json\n"),
            ("silent.sh", "true\n"),
            ("missing", "definitely-not-a-command-kelir-test"),
        ];
        let defs = scripts
            .iter()
            .map(|(name, body)| {
                if *name == "missing" {
                    def(Event::BeforeTool, body)
                } else {
                    def(Event::BeforeTool, &answers(temp.path(), name, body))
                }
            })
            .collect();
        let runner = runner(temp.path(), defs);
        let chain = runner
            .run_event(
                Event::BeforeTool,
                Some("bash"),
                1,
                json!({"tool":"bash","input":{}}),
                None,
            )
            .await;
        assert_eq!(chain.verdict, Verdict::Allow);
        assert_eq!(chain.runs.len(), 4);
        assert!(chain.runs.iter().all(|run| run.decision == "allow"));
        let errors: Vec<_> = chain.runs.iter().map(|run| run.error.clone()).collect();
        assert!(errors[0].as_deref().unwrap().contains("exited with 3"));
        assert!(errors[1].as_deref().unwrap().contains("not JSON"));
        assert_eq!(errors[2], None, "silence is not an error");
        assert!(errors[3].is_some(), "a missing command is a failure");
    }

    #[tokio::test]
    async fn an_ignored_denial_does_not_change_the_verdict() {
        let chain = run_one(
            "echo '{\"decision\":\"deny\",\"reason\":\"x\"}'\n",
            Event::SessionEnd,
            Duration::from_secs(20),
        )
        .await;
        assert_eq!(chain.verdict, Verdict::Allow);
        assert!(chain.runs[0].error.is_none());
    }

    #[tokio::test]
    async fn a_hanging_gate_is_killed_with_its_tree() {
        let _serial = Quiet(SERIAL.lock().await);
        let temp = tempfile::tempdir().unwrap();
        let hang = answers(
            temp.path(),
            "hang.sh",
            "(sleep 3; echo alive > flag.txt) &\nsleep 60\n",
        );
        let runner = runner(temp.path(), vec![def(Event::TurnEnd, &hang)])
            .with_timeout(Duration::from_millis(800));
        let started = Instant::now();
        let chain = runner
            .run_event(Event::TurnEnd, None, 1, json!({"final_text": "x"}), None)
            .await;
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(chain.verdict, Verdict::Allow);
        assert!(
            chain.runs[0]
                .error
                .as_deref()
                .unwrap()
                .contains("timed out")
        );
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            !temp.path().join("flag.txt").exists(),
            "the gate's tree outlived its timeout"
        );
    }

    #[tokio::test]
    async fn gates_leave_no_background_process() {
        for (event, body) in [
            (
                Event::SessionEnd,
                "(sleep 3; echo alive > flag.txt) >/dev/null 2>&1 &\necho '{\"decision\":\"allow\"}'\n",
            ),
            (
                Event::SessionStart,
                "i=0\nwhile [ $i -lt 40000 ]; do echo 0123456789012345678901234567890123456789; i=$((i+1)); done\n",
            ),
        ] {
            let _serial = Quiet(SERIAL.lock().await);
            let temp = tempfile::tempdir().unwrap();
            let command = answers(temp.path(), "gate.sh", body);
            let chain = runner(temp.path(), vec![def(event, &command)])
                .run_event(event, None, 1, json!({}), None)
                .await;
            if event == Event::SessionStart {
                assert!(
                    chain.runs[0]
                        .error
                        .as_deref()
                        .unwrap()
                        .contains("output over")
                );
            } else {
                assert_eq!(chain.runs[0].error, None, "{:?}", chain.runs[0]);
            }
            assert_eq!(chain.verdict, Verdict::Allow);
            tokio::time::sleep(Duration::from_secs(4)).await;
            assert!(
                !temp.path().join("flag.txt").exists(),
                "a leftover child survived the gate"
            );
        }
    }

    #[test]
    fn descriptions_name_environment_variables_but_never_their_values() {
        let mut gate = def(Event::TurnEnd, "sh x.sh");
        gate.env
            .insert("SECRET_THING".into(), "hunter2-canary".into());
        let text = serde_json::to_string(&describe(&[gate])).unwrap();
        assert!(text.contains("SECRET_THING"));
        assert!(!text.contains("hunter2-canary"), "{text}");
    }

    #[test]
    fn a_kelir_started_by_a_gate_runs_no_gates() {
        assert!(started_by_a_gate(Some("turn_end")));
        assert!(!started_by_a_gate(Some("  ")));
        assert!(!started_by_a_gate(None));
    }

    #[test]
    fn the_gate_processes_of_a_whole_family_are_bounded() {
        // The cap is per process; the family's total follows from the child caps.
        let live_children = crate::features::children::DEFAULT_MAX_LIVE as usize;
        assert_eq!(root_bound(live_children), 8 + 16 * 2);
        assert!(root_bound(live_children) <= 40);
        const { assert!(MAX_LIVE_CHILD >= 1 && MAX_LIVE_CHILD < MAX_LIVE) };
    }

    #[tokio::test]
    async fn the_live_cap_holds_when_many_runs_pile_up() {
        let _serial = Quiet(SERIAL.lock().await);
        let temp = tempfile::tempdir().unwrap();
        let slow = answers(temp.path(), "slow.sh", "sleep 2\n");
        let runner = std::sync::Arc::new(runner(temp.path(), vec![def(Event::SessionEnd, &slow)]));
        let mut peak = 0usize;
        let mut tasks = Vec::new();
        for _ in 0..(MAX_LIVE * 3) {
            let runner = runner.clone();
            tasks.push(tokio::spawn(async move {
                runner
                    .run_event(Event::SessionEnd, None, 1, json!({}), None)
                    .await
            }));
        }
        tokio::time::sleep(Duration::from_millis(1200)).await;
        peak = peak.max(live());
        let mut skipped = 0;
        for task in tasks {
            let chain = task.await.unwrap();
            if chain.runs[0]
                .error
                .as_deref()
                .is_some_and(|e| e.contains("already running"))
            {
                skipped += 1;
            }
            assert_eq!(chain.runs[0].decision, "allow");
        }
        assert!(peak <= MAX_LIVE, "{peak} gates were alive at once");
        assert!(skipped >= MAX_LIVE, "only {skipped} runs were skipped");
        assert_eq!(live(), 0, "a gate slot leaked");
    }

    #[tokio::test]
    async fn a_gate_that_spawns_itself_is_bounded_and_leaves_nothing() {
        let _serial = Quiet(SERIAL.lock().await);
        let temp = tempfile::tempdir().unwrap();
        // Each level starts two more, three levels deep at most (the script counts its own depth), and then
        // sleeps: 15 processes if nothing stopped it.
        let script = answers(
            temp.path(),
            "selfish.sh",
            "d=${DEPTH:-0}
if [ $d -lt 3 ]; then DEPTH=$((d+1)) bash \"$0\" & DEPTH=$((d+1)) bash \"$0\" & fi
(sleep 3; echo alive > flag.txt) &
sleep 1
",
        );
        let mut gate = def(Event::SessionEnd, &script);
        gate.env.insert("DEPTH".into(), "0".into());
        let runner = runner(temp.path(), vec![gate]).with_timeout(Duration::from_millis(600));
        let chain = runner
            .run_event(Event::SessionEnd, None, 1, json!({}), None)
            .await;
        assert_eq!(chain.runs.len(), 1);
        assert_eq!(live(), 0);
        // The script's own sleeps determine when a survivor would write `flag.txt`: 1 s in the gate and 3 s in the detached child.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            !temp.path().join("flag.txt").exists(),
            "a descendant of the gate survived"
        );
    }
}
