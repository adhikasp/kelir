//! STEER-2 reviewer's adversarial tests (the implementer's own suite is
//! `e2e_steer2.rs`). A fake `opencode` with scripted behaviour on the child's
//! PATH; the endpoint is the in-process mock. No real credentials.

mod support;

use std::path::{Path, PathBuf};
use std::time::Instant;

use support::*;

const SECRET: &str = "oc_go_review_9d41ab";

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn entry(integration: &str, active: bool, access: &str, expires: u64) -> String {
    format!(
        r#"{{"id":"i","integrationID":"{integration}","label":"l","active":{active},
        "value":{{"type":"oauth","methodID":"m","refresh":"r","access":"{access}","expires":{expires}}}}}"#
    )
}

fn array(entries: &[String]) -> String {
    format!("[{}]", entries.join(","))
}

/// A fake `opencode`: prints `first` the first time, `second` afterwards;
/// `hang` makes it sleep first.
struct Fake {
    dir: PathBuf,
    path: String,
    _temp: tempfile::TempDir,
}

impl Fake {
    fn new(first: &str, second: &str, hang: bool) -> Fake {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("first.json"), first).unwrap();
        std::fs::write(dir.join("second.json"), second).unwrap();
        write_shim(&dir, hang);
        let current = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![dir.clone()];
        paths.extend(std::env::split_paths(&current));
        let path = std::env::join_paths(paths)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        Fake {
            dir,
            path,
            _temp: temp,
        }
    }
    fn runs(&self) -> usize {
        std::fs::read_to_string(self.dir.join("count.txt"))
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }
}

fn write_shim(dir: &Path, hang: bool) {
    #[cfg(windows)]
    {
        let store = dir.to_string_lossy().into_owned();
        let sleep = if hang {
            "ping -n 31 127.0.0.1 >nul\r\n"
        } else {
            ""
        };
        let body = format!(
            "@echo off\r\n>>\"{store}\\count.txt\" echo x\r\n{sleep}\
             if exist \"{store}\\ran\" goto second\r\n\
             type nul > \"{store}\\ran\"\r\n\
             type \"{store}\\first.json\"\r\nexit /b 0\r\n:second\r\n\
             type \"{store}\\second.json\"\r\n"
        );
        std::fs::write(dir.join("opencode.cmd"), body).unwrap();
    }
    #[cfg(not(windows))]
    {
        let store = dir.to_string_lossy().replace('\\', "/");
        let sleep = if hang { "sleep 30\n" } else { "" };
        let body = format!(
            "#!/bin/sh\necho x >> '{store}/count.txt'\n{sleep}\
             if [ -e '{store}/ran' ]; then cat '{store}/second.json'; \
             else : > '{store}/ran'; cat '{store}/first.json'; fi\n"
        );
        let file = dir.join("opencode");
        std::fs::write(&file, body).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn written(run: &Run) -> String {
    let mut text = format!("{}{}", run.stdout, run.stderr);
    fn walk(dir: &Path, text: &mut String) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, text);
            } else if let Ok(b) = std::fs::read(&p) {
                text.push_str(&String::from_utf8_lossy(&b));
            }
        }
    }
    walk(&run.session_dir, &mut text);
    text
}

async fn go(mock: &Mock, fake: &Fake, extra_env: &[(&str, &str)]) -> Run {
    let mut env: Vec<(&str, &str)> = vec![("KELIR_GO_API_KEY", ""), ("PATH", &fake.path)];
    env.extend_from_slice(extra_env);
    run_task(
        mock,
        &["run", "--json", "--provider", "opencode-go", "hi"],
        &env,
        None,
        "",
    )
    .await
}

/// Brief 3: a credential that is already expired on the first read gets one
/// re-read (opencode refreshes itself when it runs) before failing.
#[tokio::test]
async fn expired_on_first_read_is_reread_once_and_recovers() {
    let past = now_ms().saturating_sub(60_000);
    let good = now_ms() + 3_600_000;
    let fresh = "oc_go_refreshed_aa11";
    let fake = Fake::new(
        &array(&[entry("opencode", true, SECRET, past)]),
        &array(&[entry("opencode", true, fresh, good)]),
        false,
    );
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = go(&mock, &fake, &[]).await;
    assert_eq!(
        run.code, 0,
        "expired-then-fresh must recover: {}",
        run.stderr
    );
    assert_eq!(
        mock.requests()[0].header("authorization"),
        Some(format!("Bearer {fresh}").as_str())
    );
}

/// Within two minutes of expiry the store is re-read and the fresher token used.
#[tokio::test]
async fn near_expiry_prefers_the_fresher_second_read() {
    let soon = now_ms() + 30_000;
    let good = now_ms() + 3_600_000;
    let fresh = "oc_go_refreshed_bb22";
    let fake = Fake::new(
        &array(&[entry("opencode", true, SECRET, soon)]),
        &array(&[entry("opencode", true, fresh, good)]),
        false,
    );
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = go(&mock, &fake, &[]).await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        mock.requests()[0].header("authorization"),
        Some(format!("Bearer {fresh}").as_str())
    );
    assert_eq!(fake.runs(), 2);
    let w = written(&run);
    assert!(!w.contains(SECRET) && !w.contains(fresh));
}

/// A provider that echoes the bearer token in its 401 body must not put it in
/// the log / stdout / stderr.
#[tokio::test]
async fn a_provider_error_body_that_echoes_the_key_is_redacted() {
    let good = now_ms() + 3_600_000;
    let fake = Fake::new(
        &array(&[entry("opencode", true, SECRET, good)]),
        &array(&[entry("opencode", true, SECRET, good)]),
        false,
    );
    let body = format!("{{\"error\":{{\"message\":\"Invalid API key: {SECRET}\"}}}}");
    let mock = Mock::start(vec![Reply::Http(401, body.clone()), Reply::Http(401, body)]).await;
    let run = go(&mock, &fake, &[]).await;
    assert_ne!(run.code, 0);
    let w = written(&run);
    assert!(
        !w.contains(SECRET),
        "the key leaked through a provider error body"
    );
}

/// Same for an override key from the environment.
#[tokio::test]
async fn an_env_key_echoed_by_the_provider_is_redacted() {
    let junk = "junk_override_key_77cc";
    let fake = Fake::new("[]", "[]", false);
    let body = format!("bad key {junk}");
    let mock = Mock::start(vec![Reply::Http(401, body)]).await;
    let run = go(&mock, &fake, &[("KELIR_GO_API_KEY", junk)]).await;
    assert_ne!(run.code, 0);
    assert_eq!(
        fake.runs(),
        0,
        "an explicit override must not consult the store"
    );
    let w = written(&run);
    assert!(
        !w.contains(junk),
        "the override key leaked through a provider error body"
    );
}

#[tokio::test]
async fn hanging_helper_is_stopped_within_the_timeout() {
    let fake = Fake::new("[]", "[]", true);
    let mock = Mock::start(vec![]).await;
    let t = Instant::now();
    let run = go(&mock, &fake, &[]).await;
    let took = t.elapsed();
    assert_eq!(run.code, 2, "{}", run.stderr);
    assert!(took.as_secs() < 12, "took {took:?}");
    assert!(run.stderr.contains("did not answer"), "{}", run.stderr);
    assert_eq!(mock.request_count(), 0);
}

#[tokio::test]
async fn bad_shapes_are_clear_usage_errors() {
    let good = now_ms() + 3_600_000;
    let cases: Vec<(&str, String)> = vec![
        ("garbage", "this is not json {{{".to_string()),
        ("empty array", "[]".to_string()),
        ("object", "{\"a\":1}".to_string()),
        ("inactive", array(&[entry("opencode", false, SECRET, good)])),
        (
            "other integration",
            array(&[entry("github", true, SECRET, good)]),
        ),
        (
            "no access",
            r#"[{"integrationID":"opencode","active":true,"value":{}}]"#.to_string(),
        ),
        ("empty", String::new()),
    ];
    for (name, json) in cases {
        let fake = Fake::new(&json, &json, false);
        let mock = Mock::start(vec![]).await;
        let run = go(&mock, &fake, &[]).await;
        assert_eq!(run.code, 2, "{name}: {}", run.stderr);
        assert_eq!(mock.request_count(), 0, "{name}");
        assert!(run.stderr.contains("opencode"), "{name}: {}", run.stderr);
        assert!(!run.stderr.contains(SECRET), "{name}");
        assert!(
            !run.stderr.to_lowercase().contains("panick"),
            "{name}: {}",
            run.stderr
        );
    }
}

/// Two active opencode entries: deterministic (the first active), only that
/// token goes out.
#[tokio::test]
async fn two_active_entries_pick_one_deterministically() {
    let good = now_ms() + 3_600_000;
    let json = array(&[
        entry("opencode", false, "inactive_tok", good),
        entry("opencode", true, SECRET, good),
        entry("opencode", true, "second_active_tok", good),
    ]);
    let fake = Fake::new(&json, &json, false);
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = go(&mock, &fake, &[]).await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        mock.requests()[0].header("authorization"),
        Some(format!("Bearer {SECRET}").as_str())
    );
}

/// A very large export (about 30 MB) neither hangs nor crashes.
#[tokio::test]
async fn a_huge_export_is_handled() {
    let good = now_ms() + 3_600_000;
    let pad = "x".repeat(30 * 1024 * 1024);
    let json = format!(
        "[{{\"integrationID\":\"junk\",\"active\":false,\"label\":\"{pad}\",\"value\":{{}}}},{}]",
        entry("opencode", true, SECRET, good)
    );
    let fake = Fake::new(&json, &json, false);
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let t = Instant::now();
    let run = go(&mock, &fake, &[]).await;
    assert_eq!(run.code, 0, "{} (after {:?})", run.stderr, t.elapsed());
}

/// 401 with a junk override: exactly one request, a clear error, no store read.
#[tokio::test]
async fn junk_override_gives_a_clear_401_and_no_retry() {
    let good = now_ms() + 3_600_000;
    let json = array(&[entry("opencode", true, SECRET, good)]);
    let fake = Fake::new(&json, &json, false);
    let mock = Mock::start(vec![
        Reply::Http(401, "unauthorized".into()),
        Reply::Http(401, "unauthorized".into()),
    ])
    .await;
    let run = go(&mock, &fake, &[("KELIR_GO_API_KEY", "junk")]).await;
    assert_ne!(run.code, 0);
    assert_eq!(mock.request_count(), 1, "stderr {}", run.stderr);
    assert_eq!(fake.runs(), 0);
    assert!(run.events.iter().any(|e| e["type"] == "error"));
}

/// A literal key in the config: warned about, never sent.
#[tokio::test]
async fn literal_key_in_config_is_warned_and_unused() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let cfg = "key = \"sk-literal-top\"\n[providers.openrouter]\nkey = \"sk-literal-prov\"\n";
    let run = run_task(&mock, &["run", "--json", "hi"], &[], None, cfg).await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stderr.contains("warning"), "{}", run.stderr);
    assert_eq!(
        mock.requests()[0].header("authorization"),
        Some("Bearer test-openrouter-key")
    );
    let w = written(&run);
    assert!(!w.contains("sk-literal"), "literal leaked into outputs/log");
    let mock = Mock::start(vec![]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "hi"],
        &[("OPENROUTER_API_KEY", "")],
        None,
        cfg,
    )
    .await;
    assert_eq!(run.code, 2, "{}", run.stderr);
    assert_eq!(mock.request_count(), 0);
    assert!(!run.stderr.contains("sk-literal"));
}

/// Typos get the pair named.
#[tokio::test]
async fn typo_in_provider_name_names_the_pair() {
    let mock = Mock::start(vec![]).await;
    for typo in ["opencode_go", "OpenRouter", "opencodego", "", " openrouter"] {
        let run = run_task(
            &mock,
            &["run", "--json", "--provider", typo, "hi"],
            &[],
            None,
            "",
        )
        .await;
        assert_eq!(run.code, 2, "{typo:?}: {}", run.stderr);
        assert!(
            run.stderr.contains("openrouter") && run.stderr.contains("opencode-go"),
            "{typo:?}: {}",
            run.stderr
        );
    }
}

/// `doctor` with an expired store exits 1 and prints no secret.
#[tokio::test]
async fn doctor_with_expired_store_fails_clearly_without_secret() {
    let past = now_ms().saturating_sub(60_000);
    let json = array(&[entry("opencode", true, SECRET, past)]);
    let fake = Fake::new(&json, &json, false);
    let mock = Mock::start(vec![]).await;
    let run = run_task(
        &mock,
        &["doctor"],
        &[("KELIR_GO_API_KEY", ""), ("PATH", &fake.path)],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
    let all = format!("{}{}", run.stdout, run.stderr);
    assert!(all.contains("expired"), "{all}");
    assert!(!all.contains(SECRET));
}

/// SPEC/brief: OpenRouter reads `OPENROUTER_API_KEY` first, then `key_env`.
/// A `key_env` naming an unset variable must not hide `OPENROUTER_API_KEY`.
#[tokio::test]
async fn openrouter_key_env_is_a_fallback_not_a_replacement() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let cfg = "[providers.openrouter]\nkey_env = \"STEER2_REVIEW_UNSET_VAR\"\n";
    let run = run_task(&mock, &["run", "--json", "hi"], &[], None, cfg).await;
    assert_eq!(
        run.code, 0,
        "OPENROUTER_API_KEY is set and must be tried first: {}",
        run.stderr
    );
}
