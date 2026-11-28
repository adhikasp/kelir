//! Boot: process options, the main run loop and preparation.
// These private loop modules expose crate-visible helpers to the feature modules.
#![allow(clippy::redundant_pub_crate)]

use super::types::*;
use super::*;

pub(crate) async fn drive(opts: Options) -> u8 {
    // One interrupt listener for the process, installed before *anything* observable: before `prepare`
    // (which records `session_started`) and so before any client can see the session and signal it.
    crate::sys::signals::install();
    let mut prepared = match prepare(&opts).await {
        Ok(prepared) => prepared,
        Err(failure) => {
            eprintln!("kelir: {}", failure.message());
            if let Some((_, sources)) = &opts.run_options {
                let _ = sources;
            }
            // A scripted caller reads the result line, even for a usage error.
            if opts.json {
                print_result_event(
                    "error",
                    failure.code(),
                    "",
                    0,
                    None,
                    Some(failure.message()),
                );
            }
            return failure.code();
        }
    };

    // One session is one writer.
    let _lock = match ipc::lock(&prepared.dir) {
        Ok(lock) => lock,
        Err(message) => {
            let message = format!("session {} is already running ({message})", prepared.id);
            eprintln!("kelir: {message}");
            if opts.json {
                print_result_event("error", 1, "", 0, None, Some(&message));
            }
            return 1;
        }
    };

    crate::features::names::write_owner(&prepared.dir);

    // Bind before `session_started` is written, so a client that sees the event
    // can connect.
    let listener = match ipc::bind(&prepared.dir, &prepared.id) {
        Ok(listener) => listener,
        Err(message) => {
            eprintln!("kelir: {message}");
            if opts.json {
                print_result_event("error", 1, "", 0, None, Some(&message));
            }
            return 1;
        }
    };
    // On Unix a session dir whose path does not fit in `sun_path` binds a shorter socket and
    // records it; take the address that was really bound for `KELIR_SOCK`.
    prepared.sock = ipc::value(&prepared.dir, &prepared.id);

    // The trust boundary: a token for this session, and the registry of the jobs it may be told about.
    if let Err(error) = crate::features::bg::harden_process() {
        eprintln!("kelir: warning: {error}");
    }
    let trust = match crate::sys::trust::Trust::create(prepared.dir.clone()) {
        Ok(trust) => std::sync::Arc::new(trust),
        Err(message) => {
            eprintln!("kelir: {message}");
            if opts.json {
                print_result_event("error", 1, "", 0, None, Some(&message));
            }
            return 1;
        }
    };

    let mut session = match Session::open(prepared, trust.clone()) {
        Ok(session) => session,
        Err(message) => {
            eprintln!("kelir: {message}");
            if opts.json {
                print_result_event("error", 1, "", 0, None, Some(&message));
            }
            return 1;
        }
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<Incoming>();
    ipc::serve(listener, trust.clone(), tx.clone());
    // Background jobs: `kelir bg run` reports itself over the socket, and this watcher is the safety net
    // that also catches a promoted call settling while the session runs.
    watch_bg(crate::paths::calls_dir(&session.log.dir), tx.clone());
    // MCP: broker tasks report back through the same channel, and channel
    // servers connect now.
    session.inbox = Some(tx.clone());
    session
        .features
        .mcp
        .as_mut()
        .expect("mcp feature")
        .attach(tx.clone());
    // The root looks for children whose process is gone once a second, so a
    // crashed one frees its slot and a killed subtree is recorded.
    if session.kids.is_root() {
        crate::features::children::watch_children(tx.clone(), session.kids.key().to_string());
    }
    // A child leaves with its parent: a hard-killed parent cannot signal it.
    if let Some(handoff) = opts.child.clone() {
        crate::features::children::watch_parent(handoff, tx.clone(), trust.token().to_string());
    }

    // A job's wall clock: at the deadline the turn is told to stop; if the loop
    // is still there after a grace period, the process is forced out.
    if let Some((options, _)) = &opts.run_options
        && let Some(deadline) = options.deadline
        && let Some(state) = options.state.clone()
    {
        watch_deadline(deadline, tx.clone(), trust.token().to_string(), state);
    }

    // The interactive session: the TUI runs as a task beside the loop, and all
    // it can do is send verbs into the same channel the socket feeds.
    let mut tui = None;
    if opts.tui {
        let (to_tui, from_session) = mpsc::unbounded_channel::<crate::tui::View>();
        let mut banner = vec![
            tui::BannerLine::plain(format!(
                "kelir · {} · {}",
                session.provider.provider_name, session.provider.model
            )),
            tui::BannerLine::plain(format!(
                "cwd {}",
                crate::util::path_string(&session.ctx.cwd)
            )),
        ];
        // The window is not ambient: the number is for `state`, not the banner.
        if let Some(line) = session.features.models.banner_line() {
            banner.push(tui::BannerLine::dim(line));
        }
        banner.push(tui::BannerLine::plain(
            "Enter sends · /help shows the commands",
        ));
        // The cap is worth a banner line only when one is set: the status line
        // carries the count ("step 3/5") while a turn runs.
        if let Some(cap) = session.cfg.max_steps {
            banner.push(tui::BannerLine::plain(format!(
                "step cap {cap} per turn · reaching it pauses the turn (say continue)"
            )));
        }
        if session.run.resumed {
            banner.push(tui::BannerLine::plain(format!(
                "resumed from the event log · {} turn(s) recorded{}",
                session.run.turn,
                if session.run.in_flight {
                    " · continuing the turn that was cut off"
                } else {
                    ""
                }
            )));
        }
        banner.push(tui::BannerLine::plain(String::new()));
        let token = session.ctx.token.clone();
        match tui::start(
            from_session,
            to_tui.clone(),
            tx.clone(),
            &banner,
            session.cfg.max_steps,
            token.clone(),
            crate::tui::theme::Theme::from_env(),
        ) {
            Ok(mut handle) => {
                // The session dir is where a pasted image is saved, and where
                // `attachments/…` in a prompt resolves.
                handle.app.set_session_dir(session.log.dir.clone());
                handle.app.set_session_id(session.log.id.clone());
                // A resumed session's title was written when its first prompt arrived; show it from the
                // first frame instead of waiting for a new prompt (which may never come).
                if let Some(title) = session
                    .log
                    .events
                    .iter()
                    .find_map(|event| match &event.body {
                        crate::log::event::EventBody::SessionTitle { title } => Some(title.clone()),
                        _ => None,
                    })
                {
                    handle.app.set_title(title);
                }
                session.out.ui = Some(to_tui.clone());
                // `/new` ends this session and asks for a fresh process; the
                // flag comes back out of the TUI task once it has restored the
                // terminal, so nothing restarts while the screen is half-owned.
                let restart = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let flag = restart.clone();
                let task = tokio::spawn(async move {
                    let ended = handle.run().await;
                    flag.store(ended.unwrap_or(false), Ordering::SeqCst);
                });
                // If the UI dies (a panic, a terminal error) the session ends the same way `/quit` ends it,
                // instead of running forever with nobody watching.
                let verbs = tx.clone();
                let watchdog = tokio::spawn(async move {
                    let _ = task.await;
                    let (reply, _answer) = oneshot::channel();
                    let _ = verbs.send(Incoming {
                        // The UI runs inside the session, so its verb carries
                        // the token from memory like the TUI's own dispatch.
                        request: Request::new("end", json!({}), "tui")
                            .with_token(Some(token.clone())),
                        reply,
                        action: None,
                    });
                });
                tui = Some((watchdog, to_tui, restart));
            }
            Err(error) => {
                // Raw mode may already be on; leave the terminal usable.
                let _ = ratatui::try_restore();
                eprintln!("kelir: cannot start the terminal UI: {error}");
                return 1;
            }
        }
    }

    let code = session.run(&mut rx).await;
    if let Some(mcp) = session.features.mcp.as_ref() {
        mcp.shutdown().await;
    }
    let mut restart = false;
    if let Some((watchdog, to_tui, flag)) = tui {
        // One last paint, then the terminal goes back to the shell.
        let _ = to_tui.send(crate::tui::View::Live(crate::tui::Live::Quit));
        let _ = watchdog.await;
        restart = flag.load(Ordering::SeqCst);
    }
    if opts.ask && !session.run.final_text.is_empty() {
        println!("{}", session.run.final_text);
    }
    session.print_result(code);
    if restart {
        return crate::core::session::EXIT_RESTART;
    }
    code
}

/// A job's timeout: stop the turn at the deadline, force the process out if the
/// loop does not end by itself.
fn watch_deadline(
    deadline: Duration,
    tx: mpsc::UnboundedSender<Incoming>,
    token: String,
    state: std::sync::Arc<crate::features::names::namedrun::RunState>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(deadline).await;
        state.note_timeout();
        let (reply, _answer) = oneshot::channel();
        let _ = tx.send(Incoming {
            request: Request::new("stop", json!({"reason": "job timeout"}), "job-timeout")
                .with_token(Some(token.clone())),
            reply,
            action: None,
        });
        // Then end the session: a stop only ends the turn.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let (reply, _answer) = oneshot::channel();
        let _ = tx.send(Incoming {
            request: Request::new("end", json!({}), "job-timeout").with_token(Some(token)),
            reply,
            action: None,
        });
        tokio::time::sleep(crate::features::names::namedrun::FORCE_AFTER).await;
        crate::features::names::namedrun::force_timeout(state).await;
    });
}

/// Watch this session's `bg/` for jobs that start or settle, and push each change into the same channel
/// the socket feeds.
fn watch_bg(dir: PathBuf, tx: mpsc::UnboundedSender<Incoming>) {
    tokio::spawn(async move {
        let mut seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        loop {
            tokio::time::sleep(crate::features::bg::WATCH).await;
            for status in crate::features::bg::list(&dir) {
                if !status.is_job() {
                    continue;
                }
                if !seen.insert((status.id.clone(), status.state.clone())) {
                    continue;
                }
                let (reply, _answer) = oneshot::channel();
                let action = crate::ipc::Action::Note(status);
                let incoming = Incoming {
                    request: Request::new("bg-watch", serde_json::Value::Null, "job"),
                    reply,
                    action: Some(action),
                };
                if tx.send(incoming).is_err() {
                    return;
                }
            }
        }
    });
}

pub(crate) async fn prepare(opts: &Options) -> Result<Prepared, Failure> {
    let common = &opts.common;
    let mut cfg = config::load(common.config.as_deref())
        .map_err(|error| Failure::Usage(error.to_string()))?;
    // `--max-steps N` overrides the file, and `0` there means "no cap" as it does in
    // `config.toml`: a stopgap config must never be able to cap a turn at zero steps.
    if let Some(steps) = common.max_steps {
        cfg.max_steps = (steps > 0).then_some(steps);
    }

    // A resume takes its id, cwd, model and tool list from the log; only the
    // config (windows, prompts, provider keys) comes from today's machine.
    if let Some(spec) = &opts.resume {
        let cache_id = opts.child.as_ref().and_then(|child| child.cache_id.clone());
        let reopened = reopen(spec, &cfg, cache_id).await?;
        let task = if opts.task.is_empty() {
            String::new()
        } else {
            task_text(&opts.task).await.map_err(Failure::Usage)?
        };
        // Only the root reports the Herdr status: a child never writes OSC.
        let herdr = if opts.child.is_some() {
            None
        } else {
            Herdr::detect(opts.json)
        };
        let base_url = reopened.client.base_url();
        let (list, models_fetch) =
            begin_models(&reopened.client, &reopened.provider_name, &base_url).await;
        let over = cfg.override_window(&reopened.model);
        let (window, window_source) = crate::provider::models::resolve(
            over,
            list.cache.as_ref(),
            list.fetched,
            &reopened.model,
        );
        let window_note = models::window_note(over, list.why.as_deref(), window_source);
        return Ok(Prepared {
            run_sources: opts
                .run_options
                .as_ref()
                .map(|(_, sources)| sources.iter().map(|(_, body)| body.clone()).collect())
                .unwrap_or_default(),
            task_from: opts
                .run_options
                .as_ref()
                .and_then(|(options, _)| options.task_from)
                .unwrap_or("cli"),
            step_cap: opts
                .run_options
                .as_ref()
                .and_then(|(options, _)| options.step_cap),
            json: opts.json,
            ask: opts.ask,
            keep: opts.keep,
            tui: opts.tui,
            herdr,
            system: crate::features::sources::source::Rendered {
                hash: crate::features::sources::source::hash_of(&reopened.reopened.system_text),
                text: reopened.reopened.system_text.clone(),
                sources: reopened.reopened.sources.clone(),
                // A resume renders nothing: the frozen prefix is the log's, and
                // what it says about its sources was recorded when it was made.
                warnings: Vec::new(),
            },
            cwd: reopened.reopened.cwd.clone(),
            config_path: common.config.clone(),
            cfg,
            provider_name: reopened.provider_name,
            model: reopened.model,
            client: reopened.client,
            provider_cfg: reopened.cfg_provider.clone(),
            // A resume keeps the origin the log recorded.
            key_origin: reopened.key_origin,
            id: reopened.id,
            dir: reopened.dir,
            task,
            // `drive` binds the socket and takes the address it really bound,
            // which also replaces a stale `sock` left by the dead process.
            sock: String::new(),
            resume: Some(reopened.reopened),
            child: opts.child.clone(),
            window,
            window_source,
            window_note,
            models: list.cache,
            models_fetch: Some(models_fetch),
            governor: governor_for(&reopened.cfg_provider),
        });
    }

    let provider_name = common
        .provider
        .clone()
        .unwrap_or_else(|| cfg.provider.clone());
    let model = common.model.clone().unwrap_or_else(|| cfg.model.clone());
    let provider_cfg = cfg
        .provider(&provider_name)
        .map_err(|error| Failure::Usage(error.to_string()))?;

    let cwd = match &common.cwd {
        Some(path) if path.is_absolute() => path.clone(),
        Some(path) => std::env::current_dir()
            .map_err(|error| Failure::Run(format!("cannot read the current directory: {error}")))?
            .join(path),
        None => std::env::current_dir()
            .map_err(|error| Failure::Run(format!("cannot read the current directory: {error}")))?,
    };
    if !cwd.is_dir() {
        return Err(Failure::Usage(format!(
            "cwd is not a directory: {}",
            cwd.display()
        )));
    }

    // The TUI has no task: the first turn starts when a human sends a message.
    let task = if opts.tui {
        String::new()
    } else {
        task_text(&opts.task).await.map_err(Failure::Usage)?
    };
    // A child's id was minted by the root, which already named it in its log.
    let id = match &opts.child {
        Some(child) => child.session.clone(),
        None => opts
            .id
            .clone()
            .or_else(|| {
                opts.run_options
                    .as_ref()
                    .and_then(|(options, _)| options.planned_id.clone())
            })
            .or_else(crate::features::channel::next_id)
            .unwrap_or_else(new_session_id),
    };
    // The key is resolved here, at session start, and held in memory only. `config::load` stays pure: reading opencode's store is a
    // process read, and a config load happens in tests and in `doctor` too.
    let mut provider_cfg = provider_cfg;
    let credential = keys::resolve(&mut provider_cfg).map_err(Failure::Usage)?;
    let client = Client::new(provider_cfg.clone(), &id, cfg.cache_ttl.clone())
        .map_err(|error| Failure::Usage(error.message))?;
    // Model metadata from the provider, cached beside the session state and fetched beside the session
    // (never on the first request's critical path); a `[model_windows]`/`window` setting still wins over
    let base_url = client.base_url();
    let (list, models_fetch) = begin_models(&client, &provider_name, &base_url).await;
    let over = cfg.override_window(&model);
    let (window, window_source) =
        crate::provider::models::resolve(over, list.cache.as_ref(), list.fetched, &model);
    let window_note = models::window_note(over, list.why.as_deref(), window_source);
    let dir = crate::paths::session_dir(&id);
    crate::paths::create_private_dir(&crate::paths::calls_dir(&dir)).map_err(|error| {
        Failure::Run(format!(
            "cannot create the session directory {}: {error}",
            dir.display()
        ))
    })?;
    // The one Herdr line: asked once, here, with a deadline; a pane that cannot answer is a note, never a
    // failed start.
    let probe = if opts.child.is_some() {
        crate::features::herdr::ctx::Probe::default()
    } else {
        tokio::task::spawn_blocking(crate::features::herdr::ctx::probe)
            .await
            .unwrap_or_default()
    };
    let mut system = render_system(&cfg, &cwd, probe.line.as_deref(), &id);
    system.warnings.extend(probe.note);
    // The address this session is expected to use; `drive` re-reads it after
    // `ipc::bind`, because Unix may fall back to a shorter socket path.
    let sock = ipc::value(&dir, &id);
    let herdr = if opts.child.is_some() {
        None
    } else {
        Herdr::detect(opts.json)
    };

    Ok(Prepared {
        run_sources: opts
            .run_options
            .as_ref()
            .map(|(_, sources)| sources.iter().map(|(_, body)| body.clone()).collect())
            .unwrap_or_default(),
        task_from: opts
            .run_options
            .as_ref()
            .and_then(|(options, _)| options.task_from)
            .unwrap_or("cli"),
        step_cap: opts
            .run_options
            .as_ref()
            .and_then(|(options, _)| options.step_cap),
        json: opts.json,
        ask: opts.ask,
        keep: opts.keep,
        tui: opts.tui,
        herdr,
        cfg,
        provider_name,
        model,
        client,
        provider_cfg: provider_cfg.clone(),
        key_origin: Some(credential.describe()),
        id,
        dir,
        task,
        cwd,
        config_path: common.config.clone(),
        system,
        sock,
        resume: None,
        child: opts.child.clone(),
        window,
        window_source,
        window_note,
        models: list.cache,
        models_fetch: Some(models_fetch),
        governor: governor_for(&provider_cfg),
    })
}

/// The opt-in governor for one provider config: `max_concurrent_requests` slots, advisory
/// file locks under the state root. `None` (the default) is no limit at all.
fn governor_for(
    provider: &crate::config::ProviderConfig,
) -> Option<crate::provider::governor::Governor> {
    provider
        .max_concurrent_requests
        .map(|slots| crate::provider::governor::Governor::new(&provider_base(provider), slots))
}

/// The governor's identity for a provider: its base URL, which is what "one provider" means
/// on the wire (two config names pointing at one endpoint share the endpoint's limit).
pub fn provider_base(provider: &crate::config::ProviderConfig) -> String {
    provider
        .base_url
        .split("://")
        .nth(1)
        .unwrap_or(&provider.base_url)
        .trim_end_matches('/')
        .to_string()
}

/// What [`reopen`] hands back: the session's identity, provider and log state.
pub(crate) struct ReopenedSession {
    id: String,
    dir: PathBuf,
    provider_name: String,
    model: String,
    client: Client,
    /// Where the log's session said its key came from.
    key_origin: Option<String>,
    /// The config of the provider the log is on: the governor is built from it.
    cfg_provider: crate::config::ProviderConfig,
    reopened: Reopened,
}

/// Read a session's log and prove it is not already running.
pub(crate) async fn reopen(
    spec: &str,
    cfg: &Config,
    cache_id: Option<String>,
) -> Result<ReopenedSession, Failure> {
    let id = crate::paths::resolve_session(spec).map_err(Failure::Run)?;
    let dir = crate::paths::session_dir(&id);
    let events_path = dir.join("events.jsonl");
    if !events_path.is_file() {
        return Err(Failure::Run(format!(
            "session {id} has no event log ({})",
            crate::util::path_string(&events_path)
        )));
    }
    let read = read_events(&events_path).map_err(|error| {
        Failure::Run(format!(
            "cannot read {}: {error}",
            crate::util::path_string(&events_path)
        ))
    })?;
    // A live session owns its log; a second writer would fork the sequence.
    let closed = read
        .events
        .last()
        .map(|event| matches!(event.body, EventBody::SessionEnded { .. }))
        .unwrap_or(false);
    let deadline = Instant::now() + Duration::from_secs(2);
    while ipc::connect(&dir, &id).await.is_ok() {
        if !closed || Instant::now() >= deadline {
            return Err(Failure::Run(format!(
                "session {id} is already running ({}); use `kelir ctl` or stop it first",
                ipc::value(&dir, &id)
            )));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let events = read.events.clone();
    let (mut provider_name, mut model, tools, cwd) = events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::SessionStarted {
                format: 1,
                provider,
                model,
                tools,
                cwd,
                ..
            } => Some((
                provider.clone(),
                model.clone(),
                tools.clone(),
                PathBuf::from(cwd),
            )),
            _ => None,
        })
        .ok_or_else(|| {
            Failure::Run(format!(
                "session {id} has no session_started event; its log is not a kelir session"
            ))
        })?;
    // A `model` verb's swap is recorded by the next `step_started`.
    for event in &events {
        if let EventBody::StepStarted {
            provider,
            model: step_model,
            ..
        } = &event.body
        {
            provider_name = provider.clone();
            model = step_model.clone();
        }
    }
    let provider_cfg = cfg
        .provider(&provider_name)
        .map_err(|error| Failure::Usage(error.to_string()))?;
    let mut provider_cfg = provider_cfg;
    keys::resolve(&mut provider_cfg).map_err(Failure::Usage)?;
    // A brief shares its parent's provider cache: the snapshot is the parent's
    // own prefix, so the parent's session key is what makes it hit.
    let client = Client::new(
        provider_cfg.clone(),
        cache_id.as_deref().unwrap_or(&id),
        cfg.cache_ttl.clone(),
    )
    .map_err(|error| Failure::Usage(error.message))?;
    let system_text = events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::SessionStarted { system, .. } => Some(system.text.resolve_text(&dir)),
            _ => None,
        })
        .unwrap_or_else(|| Ok(String::new()))
        .unwrap_or_default();
    // The sources `session_started` recorded: the watcher re-checks exactly
    // these, so a resumed session reports the same edits it would have live.
    let sources = events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::SessionStarted { sources, .. } => Some(sources.clone()),
            _ => None,
        })
        .unwrap_or_default();

    Ok(ReopenedSession {
        id: id.clone(),
        dir: dir.clone(),
        provider_name,
        model,
        client,
        key_origin: events.iter().rev().find_map(|event| match &event.body {
            EventBody::SessionStarted { key_origin, .. } => key_origin.clone(),
            _ => None,
        }),
        cfg_provider: provider_cfg,
        reopened: Reopened {
            in_flight: resume::interrupted(&events),
            unsettled: resume::unsettled(&events),
            damage: resume::log_damage(&read),
            step: resume::last_step(&events),
            turns: resume::turns_done(&events),
            tools: if tools.is_empty() {
                tools::names()
            } else {
                tools
            },
            system_text,
            sources,
            cwd,
            events,
        },
    })
}
