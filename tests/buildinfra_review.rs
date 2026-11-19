//! Reviewer's tests for BUILD-INFRA (docs/reviews/buildinfra-review.md).
//!
//! The `#[ignore]`d tests each prove one finding of that review and FAIL on the
//! reviewed code; whoever fixes the finding removes the `ignore` in the same
//! commit. Run them with
//! `cargo nextest run --run-ignored only -E 'binary(buildinfra_review)'`.
//! The one test without `ignore` hardens a pinning test that cannot fail.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

mod support;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn bash() -> String {
    let shell = kelir::config::default_shell();
    if shell.get(1).map(String::as_str) == Some("-c") && shell[0].contains("bash") {
        return shell[0].clone();
    }
    "bash".to_string()
}

fn fwd(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

/// `bash -c <script>` with no console window and the session's KELIR_* scrubbed.
fn run_bash(script: &str, env: &[(&str, String)]) -> Output {
    let mut cmd = Command::new(bash());
    cmd.args(["-c", script]);
    support::scrub_std(&mut cmd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(kelir::proc::win::CREATE_NO_WINDOW | 0x0000_0200);
    }
    cmd.output().expect("bash runs")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

// -------------------------------------------------------------- profile pin

/// `the_shipped_profile_stays_fat_lto_and_review_is_thin` in buildinfra.rs
/// matches substrings of the raw text, so `codegen-units = 16` satisfies its
/// `codegen-units = 1` needle and a comment can satisfy `lto = true`. This one
/// reads the parsed values.
#[test]
fn release_is_pinned_by_value_not_by_substring() {
    let manifest: toml::Value = std::fs::read_to_string(repo().join("Cargo.toml"))
        .unwrap()
        .parse()
        .unwrap();
    let release = &manifest["profile"]["release"];
    assert_eq!(
        release["lto"].as_bool(),
        Some(true),
        "fat LTO is `lto = true`"
    );
    assert_eq!(release["codegen-units"].as_integer(), Some(1));
    assert_eq!(release["strip"].as_bool(), Some(true));
    let review = &manifest["profile"]["review"];
    assert_eq!(review["inherits"].as_str(), Some("release"));
    assert_eq!(review["lto"].as_str(), Some("thin"));
    assert_eq!(review["codegen-units"].as_integer(), Some(16));
}

// ------------------------------------------------------------------ prune

/// The far-side prune script, verbatim from `scripts/remote-check.sh`.
fn far_prune() -> String {
    let script = std::fs::read_to_string(repo().join("scripts/remote-check.sh")).unwrap();
    let start = script.find("far_prune='").expect("far_prune") + "far_prune='".len();
    let end = start + script[start..].find("\n}'").expect("end of far_prune") + 2;
    script[start..end].to_string()
}

/// Run the prune script against a throwaway HOME. `setup` is bash run first
/// with `$H` the home; it can create dirs and age them with `touch -d`.
fn prune(setup: &str, keep: &str, force: &str, job: &str) -> (Output, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    let home = fwd(dir.path());
    let prog = format!(
        "H='{home}'; {setup}\nHOME=\"$H\" KEEP='{keep}' FORCE='{force}' JOB='{job}' bash -c \"$FAR\""
    );
    let out = run_bash(&prog, &[("FAR", far_prune())]);
    (out, dir)
}

const DECOYS: &str = r#"
mkdir -p "$H/kelir-verify-notes/.git" "$H/kelir-verifyXYZ" "$H/precious" "$H/target-of-link"
echo mine > "$H/kelir-verify-notes/README"; echo u > "$H/kelir-verifyXYZ/u"; echo p > "$H/target-of-link/p"
touch -d '500 hours ago' "$H/kelir-verify-notes" "$H/kelir-verifyXYZ"
"#;

/// A user's own directory whose name merely starts `kelir-verify` has no marker
/// from this script and must never be removed, even with --force --keep 0.
#[test]
#[ignore = "buildinfra-review B1: prune removes any $HOME/kelir-verify* dir"]
fn prune_only_removes_directories_it_created() {
    let (out, dir) = prune(DECOYS, "0", "1", "");
    let h = dir.path();
    for survivor in ["kelir-verify-notes/README", "kelir-verifyXYZ/u"] {
        assert!(
            h.join(survivor).exists(),
            "prune --force removed a directory it did not create: {survivor}\n{}",
            text(&out)
        );
    }
}

/// `--job` is spliced into a path: `alpha/../Documents` reaches a sibling.
#[test]
#[ignore = "buildinfra-review B1: --job is spliced into a path unvalidated"]
fn prune_job_cannot_name_a_path() {
    let setup = r#"
mkdir -p "$H/kelir-verify-alpha" "$H/Documents"; echo d > "$H/Documents/d.txt"
touch -d '500 hours ago' "$H/kelir-verify-alpha" "$H/Documents"
"#;
    let (out, dir) = prune(setup, "0", "1", "alpha/../Documents");
    assert!(
        dir.path().join("Documents/d.txt").exists(),
        "prune --job 'alpha/../Documents' --force deleted ~/Documents\n{}",
        text(&out)
    );
}

/// A non-numeric --keep makes `[ n -le keep ]` error, which reads as "not kept":
/// everything is removed. It must be rejected instead.
#[test]
#[ignore = "buildinfra-review B1: --keep abc --force keeps nothing"]
fn prune_rejects_a_keep_that_is_not_a_number() {
    let (out, _dir) = prune(DECOYS, "abc", "1", "");
    assert!(
        !out.status.success(),
        "--keep abc should be an error, got success:\n{}",
        text(&out)
    );
}

// ------------------------------------------------------------------- lock

/// A waiter that has already waited 10 s treats *any* ownerless lock as stale,
/// including one a new holder created a moment ago and has not yet written its
/// owner file into (`mkdir` and the `owner` write are two steps, and the write
/// forks `date`). The new holder's lock is taken from under it: two gates run.
#[test]
#[ignore = "buildinfra-review B2: gate-lock steals a fresh holder's lock before its owner file exists"]
fn a_waiter_does_not_steal_a_lock_that_was_just_created() {
    let dir = tempfile::TempDir::new().unwrap();
    let base = fwd(dir.path());
    let gate_lock = fwd(&repo().join("scripts/gate-lock.sh"));
    let script = format!(
        r#"
L='{base}/t.lock'; OUT='{base}/out.txt'; : > "$OUT"
sleep 60 & LIVE=$!
mkdir "$L"; printf 'pid %s\nstarted x\ncmd y\n' "$LIVE" > "$L/owner"
KELIR_GATE_LOCK="$L" bash '{gate_lock}' --timeout 40 -- echo WAITER-RAN > "$OUT" 2>&1 &
W=$!
sleep 12                               # the waiter has now waited >= 10 s
rm -rf "$L"; mkdir "$L"                # a new holder takes the lock ...
sleep 3                                # ... and has not written its owner file yet
if grep -q WAITER-RAN "$OUT"; then echo STOLEN; else echo KEPT; fi
kill "$LIVE" "$W" 2>/dev/null; wait 2>/dev/null; true
"#
    );
    let out = run_bash(&script, &[]);
    assert!(
        text(&out).contains("KEPT"),
        "the waiter ran the gate inside a lock that was just taken:\n{}",
        text(&out)
    );
}
