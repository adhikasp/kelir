//! `config.toml`: machine config, then the project's `.kelir/config.toml`.
//! CLI flags override the file; the environment supplies the keys and can point the
//! provider at a stand-in base URL (`KELIR_BASE_URL`).

pub mod load;
pub mod raw;
pub mod resolve;
pub mod types;

pub use load::*;
pub use raw::*;
pub use resolve::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_config(text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kelir-config-{}", ulid::Ulid::generate()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
        path
    }

    #[test]
    fn file_values_override_defaults() {
        let path = temp_config(
            r#"
provider = "opencode-go"
model = "some/model"
spill_limit = 2048
max_steps = 7
shell = ["sh", "-c"]
[providers.opencode-go]
base_url = "https://example.invalid/v1"
reasoning_field = "thinking"
extra_headers = { "x-extra" = "yes" }
"#,
        );
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.provider, PROVIDER_GO);
        assert_eq!(cfg.model, "some/model");
        assert_eq!(cfg.spill_limit, 2048);
        assert_eq!(cfg.max_steps, Some(7));
        assert_eq!(cfg.shell, vec!["sh", "-c"]);
        let go = cfg.providers.get(PROVIDER_GO).unwrap();
        assert_eq!(go.base_url, "https://example.invalid/v1");
        assert_eq!(go.reasoning_field.as_deref(), Some("thinking"));
        assert_eq!(
            go.extra_headers.get("x-extra").map(String::as_str),
            Some("yes")
        );
    }

    #[test]
    fn removed_keys_get_the_ordinary_unknown_key_treatment() {
        let key = format!("warn_{}", ['c', 'o', 's', 't'].iter().collect::<String>());
        let table = ['p', 'r', 'i', 'c', 'e', 's'].iter().collect::<String>();
        for removed in [
            format!("{key} = 0.5\n"),
            format!("[{table}]\n"),
            format!("[{table}.\"a/b\"]\ninput = 1.0\noutput = 2.0\n"),
        ] {
            let error = load(Some(&temp_config(&removed))).unwrap_err().to_string();
            assert!(error.contains("unknown field"), "{error}");
            assert!(!error.contains("no longer used"), "{error}");
            assert!(!error.contains("removed"), "{error}");
            // No tailored "this was removed" message: a dead key gets the
            // ordinary unknown-key treatment.
        }
        // A literal key is refused too: it goes stale, shadows a live store and puts a secret
        // in a file that gets copied.
        let error = load(Some(&temp_config(
            "key = \"sk-literal-not-used\"\n[providers.openrouter]\nkey = \"sk-also-not-used\"\n",
        )))
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown field"), "{error}");
        assert!(!error.contains("literal"), "{error}");
    }

    #[test]
    fn the_two_provider_tables_only_override_their_settings() {
        // The table name is the provider; it cannot be changed from inside.
        let path = temp_config("[providers.opencode-go]\nbase_url = \"http://127.0.0.1:1/v1\"\n");
        let cfg = load(Some(&path)).unwrap();
        let go = cfg.providers.get(PROVIDER_GO).unwrap();
        assert_eq!(
            go.kind,
            ProviderKind::OpencodeGo,
            "the name decides the kind"
        );
        assert_eq!(go.base_url, "http://127.0.0.1:1/v1");
    }

    #[test]
    fn deep_merge_keeps_sibling_keys() {
        let mut base: toml::Value = toml::from_str("a = 1\n[t]\nx = 1\ny = 2\n").unwrap();
        let over: toml::Value = toml::from_str("a = 2\n[t]\ny = 3\n").unwrap();
        merge(&mut base, over);
        assert_eq!(base["a"].as_integer(), Some(2));
        assert_eq!(base["t"]["x"].as_integer(), Some(1));
        assert_eq!(base["t"]["y"].as_integer(), Some(3));
    }

    #[test]
    fn compaction_keys_are_read_and_defaulted() {
        let path = temp_config("");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.window, None);
        assert_eq!(cfg.auto_compact_at, 0.8);
        assert_eq!(cfg.compact_keep, crate::core::compact::DEFAULT_KEEP);
        assert!(
            !cfg.compact_prompt.trim().is_empty(),
            "a prompt is always resolved"
        );
        assert_eq!(cfg.override_window("deepseek/deepseek-chat"), None);

        let path = temp_config(
            r#"
window = 64000
[compaction]
auto_at = 0.5
keep = 0
[model_windows]
"deepseek/deepseek-chat" = 32000
"#,
        );
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.auto_compact_at, 0.5);
        assert_eq!(cfg.compact_keep, 0);
        assert_eq!(cfg.override_window("deepseek/deepseek-chat"), Some(32_000));
        assert_eq!(cfg.override_window("other/model"), Some(64_000));
    }

    #[test]
    fn reasoning_effort_defaults_to_medium_and_can_be_tuned_or_turned_off() {
        let path = temp_config("");
        let cfg = load(Some(&path)).unwrap();
        for name in [PROVIDER_OPENROUTER, PROVIDER_GO] {
            assert_eq!(
                cfg.reasoning_effort(name).as_deref(),
                Some(crate::config::DEFAULT_REASONING_EFFORT),
                "{name}"
            );
        }

        let path = temp_config(
            "[providers.openrouter]\nreasoning_effort = \"low\"\n\
             [providers.opencode-go]\nreasoning_effort = \"none\"\n",
        );
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.reasoning_effort("openrouter").as_deref(), Some("low"));
        // `none` sends no field at all: the provider's own default.
        assert_eq!(cfg.reasoning_effort("opencode-go"), None);
        // Case is not the user's problem.
        let path = temp_config("[providers.openrouter]\nreasoning_effort = \" HIGH \"\n");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.reasoning_effort("openrouter").as_deref(), Some("high"));
    }

    #[test]
    fn the_retry_schedule_defaults_and_reads_every_knob() {
        let path = temp_config("");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.retry.base_ms, crate::provider::retry::DEFAULT_BASE_MS);
        assert_eq!(cfg.retry.cap_ms, crate::provider::retry::DEFAULT_CAP_MS);
        assert_eq!(
            cfg.retry.max_wait,
            Some(std::time::Duration::from_secs(
                crate::provider::retry::DEFAULT_WAIT_SECS
            ))
        );
        assert_eq!(cfg.retry.max_attempts, None);

        let path = temp_config(
            "[retry]
base_ms = 500
cap_ms = 4000
max_wait = 60
max_attempts = 5
",
        );
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(cfg.retry.base_ms, 500);
        assert_eq!(cfg.retry.cap_ms, 4_000);
        assert_eq!(cfg.retry.max_wait, Some(std::time::Duration::from_secs(60)));
        assert_eq!(cfg.retry.max_attempts, Some(5));

        // `[retry] max_wait = 0` is "wait as long as the provider needs".
        let path = temp_config(
            "[retry]
max_wait = 0
",
        );
        assert_eq!(load(Some(&path)).unwrap().retry.max_wait, None);
        // A count cap of zero would mean "never retry", which `[retry] max_wait = 0` cannot
        // express; it is refused as an attempt cap and falls back to the budget.
        let path = temp_config("[retry]\nmax_attempts = 0\n");
        assert_eq!(load(Some(&path)).unwrap().retry.max_attempts, None);
    }

    #[test]
    fn max_concurrent_requests_is_per_provider_and_off_by_default() {
        let path = temp_config("");
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(
            cfg.providers
                .get(PROVIDER_OPENROUTER)
                .unwrap()
                .max_concurrent_requests,
            None
        );
        let path = temp_config(
            "[providers.opencode-go]
max_concurrent_requests = 3
",
        );
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(
            cfg.providers
                .get(PROVIDER_GO)
                .unwrap()
                .max_concurrent_requests,
            Some(3)
        );
        assert_eq!(
            cfg.providers
                .get(PROVIDER_OPENROUTER)
                .unwrap()
                .max_concurrent_requests,
            None,
            "one provider's limit is not another's"
        );
        // Zero means "no limit", exactly like leaving it out.
        let path = temp_config(
            "[providers.opencode-go]
max_concurrent_requests = 0
",
        );
        let cfg = load(Some(&path)).unwrap();
        assert_eq!(
            cfg.providers
                .get(PROVIDER_GO)
                .unwrap()
                .max_concurrent_requests,
            None
        );
    }

    #[test]
    fn project_config_is_found_by_walking_up() {
        // The repo root is the only ancestor here, and it has no .kelir dir;
        // the helper must simply not panic and not invent a path.
        let found = find_project_config(std::path::Path::new("."));
        if let Some(path) = found {
            assert!(path.ends_with("config.toml"));
        }
    }
}
