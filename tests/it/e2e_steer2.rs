//! STEER-2 end-to-end: exactly two providers, and OpenCode Go's credential read
//! from opencode's own store through the real binary.
//!
//! Every test here puts a fake `opencode` on the child's `PATH` (a `.cmd` shim on
//! Windows, a shell script elsewhere — both are what an install really has) and
//! never talks to a provider: the store's JSON is crafted, the endpoint is the
//! in-process mock. The harness's default `KELIR_GO_API_KEY=test-go-key` is
//! cleared per test with an empty value, which is the resolver's "unset".

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]

use std::path::{Path, PathBuf};

use crate::support::*;
use serde_json::Value;

/// A key that would be easy to spot if it leaked anywhere.
const SECRET: &str = "oc_go_store_6f2a9c";

fn of_type<'a>(log: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|event| event["type"] == event_type)
        .collect()
}

/// `opencode auth export opencode`'s answer for a live credential.
fn export(access: &str, expires_at_ms: u64) -> String {
    format!(
        r#"[{{"id":"id-1","integrationID":"github","label":"gh","active":true,
            "value":{{"type":"oauth","access":"gho_not_ours","expires":9999999999999}}}},
           {{"id":"id-2","integrationID":"opencode","label":"opencode","active":true,
            "value":{{"type":"oauth","methodID":"m","refresh":"r","access":"{access}","expires":{expires_at_ms}}}}}]"#
    )
}

/// The clock opencode's store uses for `expires` (epoch ms).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

/// Far enough away that no freshness re-read fires.
fn far_future() -> u64 {
    now_ms() + 3_600_000
}

/// A directory holding a fake `opencode`, plus the `PATH` a child needs to find
/// it and nothing else of opencode's.
struct FakeOpencode {
    dir: PathBuf,
    path: String,
    temp: tempfile::TempDir,
}

impl FakeOpencode {
    /// `first` on the first run, `second` on every run after it: that is what
    /// the 401 path re-reads, and what a `refresh` looks like.
    fn new(first: &str, second: &str) -> FakeOpencode {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("first.json"), first).unwrap();
        std::fs::write(dir.join("second.json"), second).unwrap();
        write_shim(&dir, "opencode", &dir);
        let path = prepend_path(&dir);
        FakeOpencode { dir, path, temp }
    }

    /// A `PATH` with no `opencode` on it at all: an empty directory and the
    /// system directory a `.cmd` shim needs (`cmd.exe`), never this machine's
    /// own `opencode` (the dev box has one, and a test must not read it).
    fn missing() -> FakeOpencode {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        let system = std::env::var_os("SystemRoot")
            .map(|root| PathBuf::from(root).join("System32"))
            .unwrap_or_default();
        let path = std::env::join_paths([&dir, &system])
            .unwrap()
            .to_string_lossy()
            .into_owned();
        FakeOpencode { dir, path, temp }
    }

    /// The argv the helper was run with, as it recorded it.
    fn argv(&self) -> String {
        std::fs::read_to_string(self.dir.join("args.txt"))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    /// Did the helper ever run? (It writes `args.txt` before answering.)
    fn ran(&self) -> bool {
        self.dir.join("args.txt").exists()
    }

    /// The temp dir itself, so a test can keep it alive for its whole body.
    fn root(&self) -> &Path {
        self.temp.path()
    }
}

fn prepend_path(dir: &Path) -> String {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&current));
    std::env::join_paths(paths)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// A runnable `opencode` in `dir` that records its argv and then prints
/// `first.json` once and `second.json` afterwards.
fn write_shim(dir: &Path, name: &str, store: &Path) {
    // `cmd`'s own builtins read a forward-slashed path as a switch (`type
    // "C:/x"` is "cannot find the file" from any cwd but the drive root), so a
    // `.cmd` gets the native separators; a shell script gets plain ones.
    #[cfg(windows)]
    let store = store.to_string_lossy().into_owned();
    #[cfg(not(windows))]
    let store = normalize(store);
    #[cfg(windows)]
    let (file, body) = (
        dir.join(format!("{name}.cmd")),
        format!(
            "@echo off\r\n\
             >\"{store}\\args.txt\" echo %*\r\n\
             if exist \"{store}\\ran\" goto second\r\n\
             type nul > \"{store}\\ran\"\r\n\
             type \"{store}\\first.json\"\r\n\
             exit /b 0\r\n\
             :second\r\n\
             type \"{store}\\second.json\"\r\n",
        ),
    );
    #[cfg(not(windows))]
    let (file, body) = (
        dir.join(name),
        format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" > '{store}/args.txt'\n\
             if [ -e '{store}/ran' ]; then cat '{store}/second.json'; \
             else : > '{store}/ran'; cat '{store}/first.json'; fi\n",
        ),
    );
    std::fs::write(&file, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Every byte a run could have written, for the "the secret is nowhere"
/// assertions: the event log, the status files, stdout and stderr.
fn everything_written(run: &Run) -> String {
    let mut text = String::new();
    text.push_str(&run.stdout);
    text.push_str(&run.stderr);
    let mut files = vec![run.session_dir.join("events.jsonl")];
    for entry in std::fs::read_dir(&run.session_dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") && path.is_file() {
            files.push(path);
        }
    }
    for path in files {
        if let Ok(body) = std::fs::read_to_string(&path) {
            text.push_str(&body);
        }
    }
    text
}

/// OpenCode Go's key comes from opencode's own store: the request carries it as
/// a bearer token, the log records where it came from, the helper was asked
/// with the documented subcommand, and the secret is in no output at all.
#[tokio::test]
async fn the_go_provider_reads_opencodes_own_credential() {
    let fake = FakeOpencode::new(&export(SECRET, far_future()), &export(SECRET, far_future()));
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("hello", 0)))]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "--provider", "opencode-go", "hi"],
        &[("KELIR_GO_API_KEY", ""), ("PATH", &fake.path)],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);

    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].header("authorization"),
        Some(format!("Bearer {SECRET}").as_str()),
        "the store's access token must be what the request carries"
    );
    assert!(
        requests[0].header("x-opencode-session").is_some(),
        "the Go provider's session header"
    );

    let started = of_type(&run.events, "session_started");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["provider"], "opencode-go");
    assert_eq!(
        started[0]["key_origin"], "opencode auth export opencode",
        "the log says which source answered: {:#?}",
        started[0]
    );

    // The supported CLI, once, with exactly the arguments the brief names.
    assert_eq!(fake.argv(), "auth export opencode");
    assert!(
        !fake.argv().to_ascii_lowercase().contains("sqlite")
            && !fake.argv().contains("opencode.db"),
        "the store is read through the CLI, never the database"
    );

    // Redaction: a secret string in any event, status file or stream fails this.
    let written = everything_written(&run);
    assert!(
        !written.contains(SECRET),
        "the key leaked into the run's output or log"
    );
    assert!(
        !written.contains("gho_not_ours"),
        "another integration's token"
    );
    let _ = fake.root();
}

/// A 401 means the credential went stale: the store is read again (opencode
/// rotates its own token) and the step is retried once with the fresh one.
#[tokio::test]
async fn a_401_rereads_the_store_and_retries_once() {
    let fresh = "oc_go_refreshed_7b31";
    let fake = FakeOpencode::new(&export(SECRET, far_future()), &export(fresh, far_future()));
    let mock = Mock::start(vec![
        Reply::Http(401, "token expired".into()),
        Reply::Sse(sse(&text_turn("second time lucky", 0))),
    ])
    .await;
    let run = run_task(
        &mock,
        &["run", "--json", "--provider", "opencode-go", "hi"],
        &[("KELIR_GO_API_KEY", ""), ("PATH", &fake.path)],
        None,
        "",
    )
    .await;
    assert_eq!(
        run.code, 0,
        "stderr: {} events {:#?}",
        run.stderr, run.events
    );

    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "one refusal, one retry with the new key");
    assert_eq!(
        requests[0].header("authorization"),
        Some(format!("Bearer {SECRET}").as_str())
    );
    assert_eq!(
        requests[1].header("authorization"),
        Some(format!("Bearer {fresh}").as_str()),
        "the retry must carry the re-read token"
    );
    assert_eq!(run.result()["final_text"], "second time lucky");

    let written = everything_written(&run);
    assert!(!written.contains(SECRET), "the stale key leaked");
    assert!(!written.contains(fresh), "the fresh key leaked");
    let _ = fake.root();
}

/// An entry that is expired on the first read is read again once: opencode
/// refreshes its own token when it runs, so the second read may already carry
/// the fresh one (STEER-2 fix 1).
#[tokio::test]
async fn a_store_entry_expired_on_the_first_read_recovers_on_the_re_read() {
    let past = now_ms().saturating_sub(60_000);
    let fresh = "oc_go_refreshed_e2e";
    let fake = FakeOpencode::new(&export(SECRET, past), &export(fresh, far_future()));
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "--provider", "opencode-go", "hi"],
        &[("KELIR_GO_API_KEY", ""), ("PATH", &fake.path)],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        mock.requests()[0].header("authorization"),
        Some(format!("Bearer {fresh}").as_str())
    );
    let written = everything_written(&run);
    assert!(!written.contains(SECRET), "the stale key leaked");
    assert!(!written.contains(fresh), "the fresh key leaked");
    let _ = fake.root();
}

/// An expired credential is refused before a request is made, with the fix in
/// the message: a request with a token that cannot work is not tried.
#[tokio::test]
async fn an_expired_store_credential_fails_with_the_fix() {
    let past = now_ms().saturating_sub(60_000);
    let fake = FakeOpencode::new(&export(SECRET, past), &export(SECRET, past));
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("never asked", 0)))]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "--provider", "opencode-go", "hi"],
        &[("KELIR_GO_API_KEY", ""), ("PATH", &fake.path)],
        None,
        "",
    )
    .await;
    assert_eq!(
        run.code, 2,
        "a stale credential is a usage error: {}",
        run.stderr
    );
    assert_eq!(mock.request_count(), 0);
    assert!(
        run.stderr.contains("opencode") && run.stderr.contains("expired"),
        "the message must name the expiry: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("run any `opencode` command"),
        "the message must name the fix: {}",
        run.stderr
    );
    assert!(
        !run.stderr.contains(SECRET),
        "the key leaked: {}",
        run.stderr
    );
    assert!(run.events.is_empty());
    let _ = fake.root();
}

/// An explicit `KELIR_GO_API_KEY` is an override: it wins over the store, and
/// opencode is not run at all.
#[tokio::test]
async fn the_override_env_wins_and_opencode_is_not_run() {
    let fake = FakeOpencode::new(&export(SECRET, far_future()), &export(SECRET, far_future()));
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "--provider", "opencode-go", "hi"],
        &[("KELIR_GO_API_KEY", "explicit-ov"), ("PATH", &fake.path)],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(
        mock.requests()[0].header("authorization"),
        Some("Bearer explicit-ov")
    );
    assert!(!fake.ran(), "the store must not be read when the env wins");
    assert_eq!(
        of_type(&run.events, "session_started")[0]["key_origin"],
        "env KELIR_GO_API_KEY"
    );
    let _ = fake.root();
}

/// OpenRouter's `key_cmd` runs the named command and takes its trimmed stdout:
/// the second source in that provider's list.
#[tokio::test]
async fn an_openrouter_key_cmd_supplies_the_key() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let keyfile = temp.path().join("key.txt");
    std::fs::write(&keyfile, "or_from_cmd_4455\n").unwrap();
    let keyfile = normalize(&keyfile);
    #[cfg(windows)]
    std::fs::write(
        bin.join("getkey.cmd"),
        format!("@echo off\r\ntype \"{}\"\r\n", keyfile.replace('/', "\\")),
    )
    .unwrap();
    #[cfg(not(windows))]
    {
        let script = bin.join("getkey");
        std::fs::write(&script, format!("#!/bin/sh\ncat '{keyfile}'\n")).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let program = normalize(&bin.join(if cfg!(windows) {
        "getkey.cmd"
    } else {
        "getkey"
    }));
    let config = format!("[providers.openrouter]\nkey_cmd = ['{program}']\n");

    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "hi"],
        &[("OPENROUTER_API_KEY", "")],
        None,
        &config,
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(
        mock.requests()[0].header("authorization"),
        Some("Bearer or_from_cmd_4455")
    );
    assert_eq!(
        of_type(&run.events, "session_started")[0]["key_origin"],
        "key_cmd getkey.cmd".to_string().replace(
            "getkey.cmd",
            if cfg!(windows) {
                "getkey.cmd"
            } else {
                "getkey"
            }
        )
    );
    assert!(!everything_written(&run).contains("or_from_cmd_4455"));
}

/// A third provider name is refused, and the error names the two that exist —
/// for the flag and for the config file alike.
#[tokio::test]
async fn a_third_provider_name_is_refused_naming_the_two() {
    let mock = Mock::start(vec![]).await;
    for args in [
        vec!["run", "--json", "--provider", "kelir", "hi"],
        vec!["run", "--json", "--provider", "alt", "hi"],
    ] {
        let run = run_task(&mock, &args, &[], None, "").await;
        assert_eq!(run.code, 2, "{:?}: {}", args, run.stderr);
        assert!(
            run.stderr.contains("exactly two")
                && run.stderr.contains("openrouter")
                && run.stderr.contains("opencode-go"),
            "{:?}: {}",
            args,
            run.stderr
        );
        assert!(run.events.is_empty(), "{:?} wrote a log", args);
    }
    let config = "[providers.kelir]\nbase_url = \"http://example.invalid/v1\"\n";
    let run = run_task(&mock, &["run", "--json", "hi"], &[], None, config).await;
    assert_eq!(run.code, 2, "{}", run.stderr);
    assert!(run.stderr.contains("exactly two"), "{}", run.stderr);
    assert_eq!(mock.request_count(), 0);
}

/// A `[providers.<name>]` table with an unknown name is a migration message:
/// which table, which file, and what to rename it to (STEER-2 fix 3). The
/// diagnostic tool reports it too, instead of a bare exit 2.
#[tokio::test]
async fn an_unknown_provider_table_says_where_it_is_and_what_to_rename() {
    let mock = Mock::start(vec![]).await;
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"kelir\"\nmodel = \"test/model\"\n\
         [providers.kelir]\nkind = \"openrouter\"\nkey_env = \"KELIR_OPENROUTER_API_KEY\"\n",
    )
    .unwrap();
    let path = config.to_string_lossy().into_owned();
    for args in [
        vec!["run", "--json", "hi"],
        vec!["doctor"],
        vec!["run", "--ask", "hi"],
    ] {
        let run = run_task(&mock, &args, &[("KELIR_CONFIG", &path)], None, "").await;
        assert_eq!(run.code, 2, "{:?}: {}{}", args, run.stdout, run.stderr);
        for needle in ["`kelir`", "exactly two", "openrouter", "opencode-go"] {
            assert!(
                run.stderr.contains(needle),
                "{:?}: missing {needle:?} in {}",
                args,
                run.stderr
            );
        }
        assert_eq!(mock.request_count(), 0, "{args:?}");
    }
}

/// OpenRouter's key sources are `OPENROUTER_API_KEY` first, then `key_env`, then
/// `key_cmd` (SPEC: first match wins). A `key_env` that names an unset variable
/// must not hide the machine's usual one (STEER-2 fix 2) — the migrated
/// maintainer config names `KELIR_OPENROUTER_API_KEY`.
#[tokio::test]
async fn openrouter_reads_the_default_env_before_key_env_and_key_cmd() {
    let mock = Mock::start(vec![
        Reply::Sse(sse(&text_turn("ok", 0))),
        Reply::Sse(sse(&text_turn("ok", 0))),
    ])
    .await;
    // `OPENROUTER_API_KEY` (set by the harness) answers, whatever `key_env` says.
    let named_unset = "[providers.openrouter]\nkey_env = \"KELIR_UNSET_FOR_THIS_TEST\"\n";
    let run = run_task(&mock, &["run", "--json", "hi"], &[], None, named_unset).await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        mock.requests()[0].header("authorization"),
        Some("Bearer test-openrouter-key")
    );

    // No `OPENROUTER_API_KEY`: the named variable answers next.
    let run = run_task(
        &mock,
        &["run", "--json", "hi"],
        &[
            ("OPENROUTER_API_KEY", ""),
            ("KELIR_NAMED_KEY", "sk-from-key-env"),
        ],
        None,
        "[providers.openrouter]\nkey_env = \"KELIR_NAMED_KEY\"\n",
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        mock.requests().last().unwrap().header("authorization"),
        Some("Bearer sk-from-key-env")
    );
}

/// `kelir doctor` prints each provider's source, expiry and read time — and
/// never the secret.
#[tokio::test]
async fn doctor_reports_the_source_and_never_the_key() {
    let fake = FakeOpencode::new(&export(SECRET, far_future()), &export(SECRET, far_future()));
    let mock = Mock::start(vec![]).await;
    let run = run_task(
        &mock,
        &["doctor", "--json"],
        &[("KELIR_GO_API_KEY", ""), ("PATH", &fake.path)],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let report: Value = serde_json::from_str(run.stdout.trim())
        .unwrap_or_else(|error| panic!("doctor is not JSON ({error}): {}", run.stdout));
    assert_eq!(report["type"], "doctor");
    assert_eq!(report["ok"], true);
    let providers = report["providers"].as_array().expect("providers");
    let names: Vec<&str> = providers
        .iter()
        .map(|provider| provider["provider"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(names, vec!["openrouter", "opencode-go"]);
    let go = &providers[1];
    assert_eq!(go["key_origin"], "opencode auth export opencode");
    assert_eq!(go["ok"], true);
    assert!(
        go["expires_at"].as_str().unwrap_or("").contains('T'),
        "an RFC 3339 expiry: {go}"
    );
    assert!(go["read_ms"].as_u64().is_some(), "{go}");
    assert!(
        providers[0]["key_origin"] == "env OPENROUTER_API_KEY",
        "{:#?}",
        providers[0]
    );
    assert!(!run.stdout.contains(SECRET), "doctor printed the key");
    assert!(!run.stderr.contains(SECRET));

    // The text report says the same thing, in one line per provider.
    let text = run_task(
        &mock,
        &["doctor"],
        &[("KELIR_GO_API_KEY", ""), ("PATH", &fake.path)],
        None,
        "",
    )
    .await;
    assert_eq!(text.code, 0, "stderr: {}", text.stderr);
    assert!(
        text.stdout.contains("opencode auth export opencode"),
        "{}",
        text.stdout
    );
    assert!(
        text.stdout.contains("env OPENROUTER_API_KEY"),
        "{}",
        text.stdout
    );
    assert!(!text.stdout.contains(SECRET));
    let _ = fake.root();
}

/// `kelir doctor` fails (exit 1) only for a provider that matters: a session on
/// Go has no `OPENROUTER_API_KEY`, and that is reported as information, not as
/// a failure (STEER-2 review, nit 5).
#[tokio::test]
async fn doctor_does_not_fail_for_the_provider_this_session_does_not_use() {
    let fake = FakeOpencode::new(&export(SECRET, far_future()), &export(SECRET, far_future()));
    let mock = Mock::start(vec![]).await;
    let run = run_task(
        &mock,
        &["doctor", "--json", "--provider", "opencode-go"],
        &[("KELIR_GO_API_KEY", ""), ("PATH", &fake.path)],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    let report: Value = serde_json::from_str(run.stdout.trim()).expect("doctor is JSON");
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["providers"][0]["ok"], true, "{report}");

    // Both providers reported (no `--provider`), Go selected by the config and
    // healthy, OpenRouter has no key: still exit 0, and the OpenRouter line is
    // reported as information, not as the reason the command failed.
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        "provider = \"opencode-go\"\nmodel = \"go/model\"\n",
    )
    .unwrap();
    let both = run_task(
        &mock,
        &["doctor"],
        &[
            ("KELIR_CONFIG", &config.to_string_lossy()),
            ("KELIR_GO_API_KEY", ""),
            ("OPENROUTER_API_KEY", ""),
            ("PATH", &fake.path),
        ],
        None,
        "",
    )
    .await;
    assert_eq!(both.code, 0, "{}{}", both.stdout, both.stderr);
    assert!(both.stdout.contains("FAIL openrouter"), "{}", both.stdout);
    assert!(both.stdout.contains("ok   opencode-go"), "{}", both.stdout);
    let _ = fake.root();
}

/// With no opencode on `PATH` the Go provider has no source, and the message
/// says what to install or override — never a key, never a request.
#[tokio::test]
async fn a_missing_opencode_is_a_usage_error_naming_the_override() {
    let fake = FakeOpencode::missing();
    let mock = Mock::start(vec![]).await;
    let run = run_task(
        &mock,
        &["run", "--json", "--provider", "opencode-go", "hi"],
        &[("KELIR_GO_API_KEY", ""), ("PATH", &fake.path)],
        None,
        "",
    )
    .await;
    assert_eq!(run.code, 2, "{}", run.stderr);
    assert_eq!(mock.request_count(), 0);
    assert!(
        run.stderr.contains("opencode") && run.stderr.contains("not found on PATH"),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("KELIR_GO_API_KEY"), "{}", run.stderr);
    assert!(run.events.is_empty());
    let _ = fake.root();
}
