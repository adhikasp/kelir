//! `kelir doctor`: which provider is wired to what, and where its key comes
//! from — the source, never the secret (STEER-2).
//!
//! It is a report, not a test: it prints the two providers, their base URL, the
//! key source that answered (or why none did) and, when the source publishes
//! one, the key's own expiry plus the fix for a stale one. It never sends a
//! request, so it works offline and costs nothing, and it never prints a key —
//! the code path that reads one hands it to [`crate::credentials`] and nothing
//! else.

use serde_json::{Value, json};

use crate::cli::DoctorArgs;
use crate::config::{self, Config, PROVIDERS};
use crate::credentials;

/// `kelir doctor [--json] [--provider <name>]`.
pub fn doctor(args: DoctorArgs) -> u8 {
    let mut cfg = match config::load(args.common.config.as_deref()) {
        Ok(cfg) => cfg,
        Err(error) => {
            eprintln!("kelir: {error}");
            return 2;
        }
    };
    let wanted: Vec<String> = match &args.common.provider {
        Some(name) if PROVIDERS.contains(&name.as_str()) => vec![name.clone()],
        Some(other) => {
            eprintln!("kelir: {}", config::unknown_provider(other).0);
            return 2;
        }
        None => PROVIDERS.iter().map(|name| name.to_string()).collect(),
    };
    // A flag is a session's choice; the report still says whose key each
    // provider would use, so `--provider` only narrows which are printed.
    if let Some(provider) = &args.common.provider
        && wanted.len() == 1
    {
        cfg.provider = provider.clone();
    }

    let reports: Vec<Value> = wanted.iter().map(|name| report(&cfg, name)).collect();
    let usable = reports
        .iter()
        .filter(|report| report["ok"].as_bool().unwrap_or(false))
        .count();

    if args.json {
        let value = json!({
            "type": "doctor",
            "kelir_version": crate::VERSION,
            "providers": reports,
            "ok": usable == reports.len(),
        });
        println!("{value}");
    } else {
        println!("kelir {} · {} provider(s)", crate::VERSION, reports.len());
        for report in &reports {
            print_report(report);
        }
    }
    if usable == reports.len() { 0 } else { 1 }
}

/// One provider: its configuration and where its key comes from.
fn report(cfg: &Config, name: &str) -> Value {
    let session_provider = cfg.provider == name;
    let provider_cfg = match cfg.provider(name) {
        Ok(provider) => provider,
        Err(error) => {
            return json!({
                "provider": name,
                "ok": false,
                "key_origin": Value::Null,
                "error": error.to_string(),
            });
        }
    };
    let mut provider_cfg = provider_cfg;
    let mut value = json!({
        "provider": name,
        "default": session_provider,
        "base_url": provider_cfg.base_url,
        "key_env": provider_cfg.key_env,
        "key_cmd": provider_cfg.key_cmd,
        "ok": false,
        "key_origin": Value::Null,
        "expires_at": Value::Null,
        "read_ms": Value::Null,
    });
    match credentials::resolve(&mut provider_cfg) {
        Ok(credential) => {
            value["ok"] = json!(true);
            value["key_origin"] = json!(credential.describe());
            value["expires_at"] = json!(credential.expires_at_ms.map(credentials::format_ms));
            value["expiry"] = json!(credential.expiry_text());
            value["read_ms"] = json!(credential.read_ms);
            let expired = credential.is_expired(crate::models::now_ms());
            value["expired"] = json!(expired);
            if expired {
                value["ok"] = json!(false);
                value["fix"] = json!(credentials::REFRESH_FIX);
            }
        }
        Err(error) => {
            // The message says what is missing and how to fix it; it never
            // quotes a key (nothing here holds one).
            value["error"] = json!(error);
            if name == config::PROVIDER_GO {
                value["fix"] = json!(credentials::REFRESH_FIX);
            }
        }
    }
    value
}

fn print_report(report: &Value) {
    let name = report["provider"].as_str().unwrap_or("?");
    let mark = if report["ok"].as_bool().unwrap_or(false) {
        "ok  "
    } else {
        "FAIL"
    };
    let default = if report["default"].as_bool().unwrap_or(false) {
        " (default)"
    } else {
        ""
    };
    println!(
        "{mark} {name}{default}  {}",
        report["base_url"].as_str().unwrap_or("")
    );
    match report["key_origin"].as_str() {
        Some(origin) => {
            let expiry = report["expiry"].as_str().unwrap_or("no expiry");
            let read = report["read_ms"].as_u64().unwrap_or(0);
            println!("     key: {origin} · {expiry} · read in {read} ms");
            if report["expired"].as_bool().unwrap_or(false) {
                println!("     the credential has expired");
            }
        }
        None => println!("     key: none"),
    }
    if let Some(fix) = report["fix"].as_str() {
        println!("     fix: {fix}");
    }
    if let Some(error) = report["error"].as_str() {
        println!("     {error}");
    }
}
