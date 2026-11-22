use super::types::*;
use super::*;
use crate::features::models::ModelList;

/// Test-facing alias: the parser now lives with the models feature.
fn split_model_target<'a>(
    target: &'a str,
    providers: &'a std::collections::BTreeMap<String, crate::config::ProviderConfig>,
) -> (Option<&'a str>, &'a str) {
    crate::features::models::test_split_model_target(target, providers)
}

#[test]
fn session_ids_are_prefixed_lowercase_ulids() {
    let id = new_session_id();
    assert!(id.starts_with("ses_"), "{id}");
    let body = &id[4..];
    assert_eq!(body.len(), 26);
    assert!(
        body.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    );
}

/// Regression: the ported hint must see the same skill layers as the old
/// core hint, including `<config>/skills` (issue #13 review).
#[test]
fn gate_ctx_offers_the_machine_config_dir() {
    let session = test_session("test/model");
    let ctx = session.gate_ctx();
    assert_eq!(ctx.config_dir, Some(crate::paths::config_dir().as_path()));
    let places =
        crate::sources::places(ctx.cwd, ctx.skills_dirs, ctx.config_dir.map(PathBuf::from));
    assert!(
        places
            .skills_directories()
            .contains(&crate::paths::config_dir().join("skills"))
    );
}

#[test]
fn a_pause_says_what_happened_and_how_to_go_on() {
    let report = PauseReport {
        reason: PauseReason::Provider,
        attempts: 7,
        waited: Duration::from_secs(30 * 60),
        detail: "rate limited (429)".into(),
    };
    assert_eq!(
        report.message(),
        "provider unavailable after 7 attempts, 30 min: rate limited (429); say continue to retry"
    );
    let one = PauseReport {
        reason: PauseReason::Provider,
        attempts: 1,
        waited: Duration::from_millis(400),
        detail: "provider error (500)".into(),
    };
    assert_eq!(
        one.message(),
        "provider unavailable after 1 attempt, 400 ms: provider error (500); say continue to retry"
    );
}

#[test]
fn a_step_request_is_rebuilt_after_a_wait() {
    // `RequestFor::Step` builds from the session's *current* model: that is
    // what makes a `model` verb during a wait apply to the retry.
    let request = RequestFor::Step.build(&test_session("first/model"));
    assert_eq!(request.body["model"], "first/model");
    let fixed = provider::build_request(
        &test_provider(),
        "ses_1",
        "pinned/model",
        &[],
        &[],
        None,
        false,
    );
    assert_eq!(
        RequestFor::Built(&fixed)
            .build(&test_session("second/model"))
            .body["model"],
        "pinned/model",
        "a compaction request is built once, from the model it was made with"
    );
}

fn test_provider() -> crate::config::ProviderConfig {
    crate::config::ProviderConfig {
        kind: crate::config::ProviderKind::OpenRouter,
        base_url: "https://example.invalid/v1".into(),
        key_env: None,
        key: Some("test".into()),
        key_cmd: None,
        key_origin: None,
        key_expires_at_ms: None,
        extra_headers: Default::default(),
        reasoning_field: None,
        reasoning_effort: None,
        reasoning_effort_explicit: false,
        session_header: None,
        max_concurrent_requests: None,
    }
}

/// A session with nothing but a model set: enough for a request to be built
/// from the empty fold.
fn test_session(model: &str) -> Session {
    let dir = std::env::temp_dir().join(format!("kelir-session-{}", ulid::Ulid::generate()));
    std::fs::create_dir_all(&dir).unwrap();
    let writer = EventLog::open(&dir.join("events.jsonl")).unwrap();
    Session {
        log: Log {
            writer,
            blobs: Blobs::new(&dir),
            events: Vec::new(),
            id: "ses_1".into(),
            dir: dir.clone(),
            config_dir: crate::paths::config_dir(),
            titled: false,
        },
        provider: Provider {
            client: Client::new(test_provider(), "ses_1", None).unwrap(),
            active_provider: test_provider(),
            provider_name: "openrouter".into(),
            model: model.into(),
            refused_fields: BTreeSet::new(),
            governor: None,
        },
        out: Out {
            json: true,
            echo_assistant: false,
            keep: false,
            tui: false,
            ui: None,
            herdr: None,
        },
        compact: CompactState {
            request: None,
            last_prompt_tokens: None,
            floor: None,
        },
        tools: ToolState {
            names: tools::names(),
            disabled: BTreeSet::new(),
            live_calls: Vec::new(),
        },
        run: Run {
            step: 0,
            turn: 0,
            turn_steps: 0,
            has_task: false,
            final_text: String::new(),
            run_sources: Vec::new(),
            task_from: "cli",
            step_cap: None,
            resumed: false,
            in_flight: false,
            retryable_failure: false,
            pending_task: String::new(),
        },
        cfg: Config {
            provider: String::new(),
            model: String::new(),
            shell: vec![],
            spill_limit: 0,
            max_steps: None,
            cache_ttl: None,
            system_prompt: crate::config::DEFAULT_PROMPT.to_string(),
            prompt_path: None,
            providers: Default::default(),
            window: None,
            model_windows: Default::default(),
            auto_compact_at: 0.8,
            compact_at_tokens: 0,
            compact_prompt: String::new(),
            skills_dirs: vec![],
            compact_keep: 0,
            memory_dir: None,
            promote_after: std::time::Duration::from_secs(0),
            retry: crate::retry::Policy::default(),
            warn_prompt_tokens: Some(crate::usage::DEFAULT_WARN_PROMPT_TOKENS),
            children: Default::default(),
            gates: Vec::new(),
            project_gates: Vec::new(),
            trusted_projects: Vec::new(),
            channel: crate::channel::Config {
                max_active: 4,
                per_conversation_rpm: 20,
                global_rpm: 120,
                fallback: String::new(),
                spool_limit: 1024,
            },
            notify: crate::notify::Config::default(),
        },
        ctx: Ctx::new(dir, std::env::temp_dir(), vec![], 0, String::new()),
        inbox: None,
        scheduler: Scheduler::default(),
        features: crate::features::Features {
            models: models::Models::new(1_000, "default", None, None, None, model),
            mcp: Some(crate::features::mcp::Mcp::new(
                crate::mcp::broker::Broker::with_servers(
                    Default::default(),
                    None,
                    std::env::temp_dir(),
                    std::env::temp_dir(),
                    "ses_1".into(),
                ),
            )),
            gates: Some(crate::features::gates::Gates::new()),
            sources: crate::features::sources::Sources::new(crate::sources::Watch::default()),
        },
        // A throwaway trust state in the same temp dir: no socket here, and
        // nothing in these tests sends a `bg` or `cancel` verb.
        trust: std::sync::Arc::new(crate::trust::Trust::new(dir_for_trust(), String::new())),
        tally: Default::default(),
        retry_wait: Default::default(),
        kids: children::Kids::new("ses_1", Default::default(), None),
    }
}

fn dir_for_trust() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kelir-trust-{}", ulid::Ulid::generate()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn system_prompt_has_a_date_and_no_time() {
    let system = crate::sources::render_system(&crate::sources::Inputs {
        herdr: None,
        base: "base",
        base_path: None,
        places: &crate::sources::Places::default(),
        cwd: Path::new("work"),
        date: "2026-11-19",
        session_id: Some("ses_test"),
    });
    let text = system.text;
    assert!(text.starts_with("base\n"), "{text}");
    assert!(text.contains("cwd: work"), "{text}");
    let date_line = text.lines().find(|line| line.starts_with("date:")).unwrap();
    // `date: YYYY-MM-DD`: the only colon is the label's.
    assert_eq!(date_line.chars().filter(|c| *c == ':').count(), 1);
    assert_eq!(date_line.len(), "date: 2026-11-18".len());
}

#[test]
fn system_prompt_is_stable_within_a_run() {
    let cfg = Config {
        provider: String::new(),
        model: String::new(),
        shell: vec![],
        spill_limit: 0,
        max_steps: None,
        cache_ttl: None,
        system_prompt: crate::config::DEFAULT_PROMPT.to_string(),
        prompt_path: None,
        providers: Default::default(),
        window: None,
        model_windows: Default::default(),
        auto_compact_at: 0.8,
        compact_at_tokens: crate::compact::DEFAULT_SOFT_TARGET,
        compact_prompt: String::new(),
        skills_dirs: vec![],
        compact_keep: 0,
        memory_dir: None,
        promote_after: std::time::Duration::from_secs(0),
        retry: crate::retry::Policy::default(),
        warn_prompt_tokens: Some(crate::usage::DEFAULT_WARN_PROMPT_TOKENS),
        children: Default::default(),
        gates: Vec::new(),
        project_gates: Vec::new(),
        trusted_projects: Vec::new(),
        channel: crate::channel::Config {
            max_active: 4,
            per_conversation_rpm: 20,
            global_rpm: 120,
            fallback: String::new(),
            spool_limit: 1024,
        },
        notify: crate::notify::Config::default(),
    };
    let a = render_system(&cfg, Path::new("tmp"), None, "ses_a");
    let b = render_system(&cfg, Path::new("tmp"), None, "ses_a");
    assert_eq!(a, b);
}

#[test]
fn compact_input_prefers_the_command() {
    assert_eq!(compact_input(&json!({"command": "ls -la"})), "ls -la");
    assert_eq!(compact_input(&json!({"path": "src/a.rs"})), "src/a.rs");
    assert_eq!(compact_input(&json!({"x": 1})), "{\"x\":1}");
    // A supplied description is what the human reads; the full command
    // still goes to the log and to `/expand`.
    assert_eq!(
        compact_input(&json!({
            "command": "ls -la",
            "description": "list the working directory"
        })),
        "list the working directory"
    );
    assert_eq!(
        compact_input(&json!({"command": "ls -la", "description": ""})),
        "ls -la"
    );
}

#[test]
fn a_model_target_splits_only_on_a_known_provider() {
    let mut providers = std::collections::BTreeMap::new();
    for name in ["openrouter", "opencode-go"] {
        providers.insert(
            name.to_string(),
            crate::config::ProviderConfig {
                kind: crate::config::ProviderKind::OpenRouter,
                base_url: "http://x/v1".into(),
                key_env: None,
                key: None,
                key_cmd: None,
                key_origin: None,
                key_expires_at_ms: None,
                extra_headers: Default::default(),
                reasoning_field: None,
                reasoning_effort: None,
                reasoning_effort_explicit: false,
                session_header: None,
                max_concurrent_requests: None,
            },
        );
    }
    assert_eq!(
        split_model_target("openrouter/deepseek/deepseek-chat", &providers),
        (Some("openrouter"), "deepseek/deepseek-chat")
    );
    assert_eq!(
        split_model_target("opencode-go/glm-5", &providers),
        (Some("opencode-go"), "glm-5")
    );
    // A model id whose first segment is not a provider stays whole.
    assert_eq!(
        split_model_target("deepseek/deepseek-chat", &providers),
        (None, "deepseek/deepseek-chat")
    );
    assert_eq!(
        split_model_target("no-slash-model", &providers),
        (None, "no-slash-model")
    );
    // `provider/` with nothing after it is a model name, not a swap.
    assert_eq!(
        split_model_target("openrouter/", &providers),
        (None, "openrouter/")
    );
}

/// F-window: the banner is silent about the window the provider answered, and says one line when the
/// start fell back — with the reason a person needs (a 401, an offline endpoint) — because that
#[test]
fn the_banner_names_a_window_only_when_it_is_not_the_providers() {
    let mut session = test_session("m");
    // The provider's own answer: no line at all.
    session.features.models.window = 262_144;
    session.features.models.window_source = "api";
    session.features.models.window_note = None;
    assert_eq!(session.features.models.banner_line(), None);
    // A configured number is the person's own: no apology either.
    session.features.models.window_source = "config";
    assert_eq!(session.features.models.banner_line(), None);
    // A stale cache has numbers that are real, and `state` carries the why.
    session.features.models.window_source = "cache";
    session.features.models.window = 100_000;
    assert_eq!(session.features.models.banner_line(), None);
    // The fallback: the number and the reason, on one line.
    session.features.models.window_source = "default";
    session.features.models.window = crate::compact::DEFAULT_WINDOW;
    session.features.models.window_note = Some("the model list: offline".into());
    let line = session
        .features
        .models
        .banner_line()
        .expect("a fallback is announced");
    assert!(line.contains("window 128000 tokens (default)"), "{line}");
    assert!(line.contains("offline"), "{line}");
    // A fallback with nothing to say about why still says it fell back.
    session.features.models.window_note = None;
    let line = session
        .features
        .models
        .banner_line()
        .expect("a fallback is announced");
    assert!(line.contains("window 128000 tokens (default)"), "{line}");
    assert!(line.contains("did not answer"), "{line}");
}

/// F-window (addendum): a `/model` swap after a fresh-cache startup still announces a changed window.
#[test]
fn a_window_that_changes_after_a_swap_is_announced_once() {
    let mut session = test_session("first/model");
    assert_eq!(
        session.features.models.window_model.as_deref(),
        Some("first/model"),
        "the startup answer names its model"
    );
    // The swap itself is the first announcement, from `swap_model`: this test only has the constructor's
    // seeding to check, so it drives `apply_list` the way a landed fetch does after a swap.
    session.provider.model = "second/model".into();
    session.features.models.window = 1_000;
    session.features.models.window_source = "default";
    let (tx, mut rx) = mpsc::unbounded_channel();
    session.out.ui = Some(tx);
    let mut cache = crate::models::Cache::new(
        "openrouter",
        "https://example.invalid/v1",
        vec![crate::models::ModelInfo {
            id: "second/model".into(),
            context: Some(262_144),
            tools: None,
            vision: None,
            parallel_calls: None,
            reasoning_effort: None,
        }],
        0,
    );
    cache.fetched_at_ms = crate::models::now_ms();
    let Session {
        log:
            Log {
                writer,
                blobs,
                events,
                dir,
                id,
                ..
            },
        provider:
            Provider {
                model,
                provider_name,
                client,
                active_provider,
                governor,
                ..
            },
        out: Out { ui, json, tui, .. },
        cfg,
        ctx,
        tally,
        features,
        ..
    } = &mut session;
    let mut cx = Cx {
        writer,
        blobs,
        events,
        ui,
        cfg,
        dir,
        id,
        model,
        provider_name,
        client,
        active_provider,
        governor,
        ctx,
        tally,
        json: *json,
        tui: *tui,
    };
    features.models.apply_list(
        ModelList {
            cache: Some(cache),
            fetched: true,
            why: None,
        },
        &mut cx,
    );
    let announced: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|event| match event {
            UiEvent::Info(text) => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(announced.len(), 1, "one line, once: {announced:?}");
    assert!(
        announced[0].contains("262144 tokens (api)"),
        "{announced:?}"
    );
    assert_eq!(session.features.models.window, 262_144);
    assert_eq!(session.features.models.window_source, "api");

    // A second answer for the same model is silent: the number did not move
    // for a new model, so there is nothing new to say.
    let Session {
        log:
            Log {
                writer,
                blobs,
                events,
                dir,
                id,
                ..
            },
        provider:
            Provider {
                model,
                provider_name,
                client,
                active_provider,
                governor,
                ..
            },
        out: Out { ui, json, tui, .. },
        cfg,
        ctx,
        tally,
        features,
        ..
    } = &mut session;
    let mut cx = Cx {
        writer,
        blobs,
        events,
        ui,
        cfg,
        dir,
        id,
        model,
        provider_name,
        client,
        active_provider,
        governor,
        ctx,
        tally,
        json: *json,
        tui: *tui,
    };
    features.models.apply_list(
        ModelList {
            cache: None,
            fetched: true,
            why: None,
        },
        &mut cx,
    );
    assert!(
        rx.try_recv().is_err(),
        "the same model's answer is not announced twice"
    );
}
