//! Reviewer's tests for BUILD-INFRA (docs/reviews/buildinfra-review.md).
//!
//! Each test here proves one finding of that review and failed on the reviewed
//! code (the `ignore` came off in the commit that fixed the finding). They are
//! the permanent proof of B1 (prune) and B2/S2/S3/S4 (the gate lock), plus the
//! S1 pin and the deploy-config gates that make a leak or a known-bad value
//! impossible to merge quietly.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

mod support;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let path = repo().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("cannot read {rel}: {error}"))
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
    // BSD `touch -d` takes an absolute ISO timestamp; GNU's takes a relative
    // string like '500 hours ago'. Give every script a portable `age_hours`
    // helper and route the test scripts through it (buildinfra-review tests
    // age throwaway work dirs to make the prune script treat them as stale).
    let script = format!(
        r#"
age_hours() {{
  n="$1"; shift
  if date -d "$n hours ago" +%Y-%m-%dT%H:%M:%S >/dev/null 2>&1; then
    touch -d "$n hours ago" "$@"
  else
    stamp=$(date -v-"${{n}}H" +%Y-%m-%dT%H:%M:%S)
    touch -d "$stamp" "$@"
  fi
}}
age_link_hours() {{
  n="$1"; shift
  if date -d "$n hours ago" +%Y-%m-%dT%H:%M:%S >/dev/null 2>&1; then
    touch -h -d "$n hours ago" "$@"
  else
    stamp=$(date -v-"${{n}}H" +%Y-%m-%dT%H:%M:%S)
    touch -h -d "$stamp" "$@"
  fi
}}
{script}"#
    );
    let mut cmd = Command::new(bash());
    cmd.args(["-c", &script]);
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
    let manifest: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo().join("Cargo.toml")).unwrap()).unwrap();
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

/// The body is shipped as `far_prune='<body>'`, so a single quote *inside* it
/// closes that quote early and every later line loses its escaping: the version
/// that used a `$'\n'` record separator ended the assignment at the `'`, the
/// separator became a literal `n`, and all 35 work dirs came back as one merged
/// record that then read as "not a directory". Nothing failed loudly. The body
/// must therefore be free of single quotes, and the listing below must stay one
/// line per work dir.
#[test]
fn the_prune_body_survives_its_own_quoting() {
    let body = far_prune();
    assert!(
        !body.contains('\''),
        "the prune body is pasted inside single quotes; it must not contain one:\n{body}"
    );
    let dir = tempfile::TempDir::new().unwrap();
    let home = fwd(dir.path());
    let script = format!(
        r#"
H='{home}'
for j in alpha alpha-2 alpha-3 beta gamma delta; do
  mkdir -p "$H/kelir-verify-$j/src-tree"
  printf 'token t\ncreated x\njob %s\n' "$j" > "$H/kelir-verify-$j/.kelir-workdir"
  age_hours 500 "$H/kelir-verify-$j"
done
HOME="$H" KEEP=9 YES=0 JOB='' bash -c "$FAR"
"#
    );
    let out = run_bash(&script, &[("FAR", far_prune())]);
    let text = text(&out);
    let listed = text.lines().filter(|l| l.starts_with("keep ")).count();
    assert_eq!(
        listed, 6,
        "one line per work dir, not one merged line:\n{text}"
    );
    assert!(
        !text.contains("not a directory"),
        "no record may be split by a bad separator:\n{text}"
    );
    for j in ["alpha", "alpha-2", "alpha-3", "beta", "gamma", "delta"] {
        assert!(text.contains(j), "{j} is listed:\n{text}");
    }
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
age_hours 500 "$H/kelir-verify-notes" "$H/kelir-verifyXYZ"
"#;

/// A user's own directory whose name merely starts `kelir-verify` has no marker
/// from this script and must never be removed, even with --force --keep 0.
#[test]
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
fn prune_job_cannot_name_a_path() {
    let setup = r#"
mkdir -p "$H/kelir-verify-alpha" "$H/Documents"; echo d > "$H/Documents/d.txt"
age_hours 500 "$H/kelir-verify-alpha" "$H/Documents"
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
fn prune_rejects_a_keep_that_is_not_a_number() {
    let (out, _dir) = prune(DECOYS, "abc", "1", "");
    assert!(
        !out.status.success(),
        "--keep abc should be an error, got success:\n{}",
        text(&out)
    );
}

// ------------------------------------------------------------------- lock

/// A waiter that finds an ownerless lock treats *any* such lock as stale,
/// including one a new holder created a moment ago and has not yet written its
/// owner file into (`mkdir` and the `owner` write are two steps, and the write
/// forks `date`). The new holder's lock is taken from under it: two gates run.
#[test]
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
# Wait for the gate to announce that it is waiting, not for a clock: under load
# the old fixed 30 s both wasted time and could still be too short. The message
# is the gate's own first "waiting ... for the gate" line (it waits every 2 s).
for _ in $(seq 1 100); do grep -q 'waiting .* for the gate' "$OUT" 2>/dev/null && break; sleep 0.1; done
grep -q 'waiting .* for the gate' "$OUT" || {{ echo NO-WAIT; cat "$OUT"; exit 1; }}
# A new holder takes the lock ... (the old owner record goes and the directory
# is made fresh in place: removing and re-creating it left a window in which the
# waiter, polling, legitimately took the free lock and the test mistook that for
# a steal on a loaded box, STEER-9)
rm -f "$L/owner"; touch "$L"
# Give the waiter one full poll (the script naps 2 s between looks) plus slack.
sleep 5                               # ... and has not written its owner file yet
if grep -q WAITER-RAN "$OUT"; then echo STOLEN; else echo KEPT; fi
kill "$LIVE" 2>/dev/null; kill "$W" 2>/dev/null
wait "$LIVE" 2>/dev/null; wait "$W" 2>/dev/null; true
"#
    );
    let out = run_bash(&script, &[]);
    assert!(
        text(&out).contains("KEPT"),
        "the waiter ran the gate inside a lock that was just taken:\n{}",
        text(&out)
    );
}

// ------------------------------------------------------- lock: process tree

/// A gate whose wrapper is `kill -9`d must not hand the box to a second gate:
/// the command it was running is still alive, and the next gate has to wait for
/// it (B2/S3). The failure on the reviewed code was `E HOLD ... E NEXT ...`
/// interleaved: two gates at once.
#[test]
fn a_killed_wrapper_does_not_hand_its_lock_to_the_next_gate() {
    let dir = tempfile::TempDir::new().unwrap();
    let base = fwd(dir.path());
    let gate_lock = fwd(&repo().join("scripts/gate-lock.sh"));
    let log = format!("{base}/log");
    let lock = format!("{base}/h.lock");
    let script = format!(
        r#"
L='{lock}'; LOG='{log}'; : > "$LOG"
KELIR_GATE_LOCK="$L" bash '{gate_lock}' --timeout 120 -- \
  bash -c 'echo E-HOLD >> "$0"; sleep 6; echo X-HOLD >> "$0"' "$LOG" &
H=$!
for _ in $(seq 1 100); do [ -f "$L" ] && break; sleep 0.1; done
gra=$(grep -l . "$L".grp.* 2>/dev/null | head -1)   # the command's grp record
pg=$(sed -n 's/^pgid //p' "$gra" 2>/dev/null | head -1)
kill -9 "$H"                                        # wrapper dies, command lives
KELIR_GATE_LOCK="$L" bash '{gate_lock}' --timeout 120 -- \
  bash -c 'echo E-NEXT >> "$0"; echo X-NEXT >> "$0"' "$LOG" &
W=$!
wait "$W"
cat "$LOG"
kill -9 "-$pg" 2>/dev/null; true
"#
    );
    let out = run_bash(&script, &[]);
    let text = text(&out);
    let order: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("E-") || line.starts_with("X-"))
        .collect();
    // Exactly one gate at a time: HOLD opens and closes before NEXT opens.
    assert_eq!(
        order,
        ["E-HOLD", "X-HOLD", "E-NEXT", "X-NEXT"],
        "the next gate waited for the killed wrapper's command:\n{text}"
    );
}

/// Five gates contending for one lock run strictly one at a time, whatever the
/// order they get it in. Fourteen other properties would still pass if the lock
/// were advisory.
#[test]
fn five_gates_at_once_run_strictly_one_at_a_time() {
    let dir = tempfile::TempDir::new().unwrap();
    let base = fwd(dir.path());
    let gate_lock = fwd(&repo().join("scripts/gate-lock.sh"));
    let script = format!(
        r#"
L='{base}/c.lock'; LOG='{base}/order'; : > "$LOG"
for i in 1 2 3 4 5; do
  KELIR_GATE_LOCK="$L" bash '{gate_lock}' --timeout 300 -- \
    bash -c 'echo "start-"$0 >> "$1"; sleep 0.6; echo "end-"$0 >> "$1"' "$i" "$LOG" &
done
wait
cat "$LOG"
"#
    );
    let out = run_bash(&script, &[]);
    let text = text(&out);
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("start-") || line.starts_with("end-"))
        .collect();
    assert_eq!(lines.len(), 10, "five gates, two lines each:\n{text}");
    for pair in lines.chunks(2) {
        let n = pair[0].trim_start_matches("start-");
        assert_eq!(pair[1], format!("end-{n}"), "one gate at a time:\n{text}");
    }
    let mut seen: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.strip_prefix("start-"))
        .collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), 5, "every gate ran exactly once:\n{text}");
}

/// A recycled pid must not look like the holder: the start time is part of the
/// record. The lock here names a pid that IS alive (this test's shell) with a
/// start time that is not its own, which is exactly what pid reuse looks like.
#[test]
fn a_reused_pid_does_not_keep_the_lock_from_the_next_gate() {
    let dir = tempfile::TempDir::new().unwrap();
    let base = fwd(dir.path());
    let gate_lock = fwd(&repo().join("scripts/gate-lock.sh"));
    let script = format!(
        r#"
L='{base}/r.lock'
read -r -a F <<< "$(cat /proc/$$/stat)"
printf 'token faketoken\npid %s\nstart %s\ncmd other\nstarted x\n' "$$" "$(( ${{F[21]}} + 1 ))" > "$L"
KELIR_GATE_LOCK="$L" bash '{gate_lock}' --timeout 30 -- echo REUSED-RAN
"#
    );
    let out = run_bash(&script, &[]);
    assert!(
        text(&out).contains("REUSED-RAN"),
        "the pid was reused, so the lock is stale:\n{}",
        text(&out)
    );
}

/// A live holder with the *right* start time is never taken over: it waits, and
/// the waiter gives up with 75 rather than running beside it.
#[test]
fn a_live_holder_is_never_taken_over() {
    let dir = tempfile::TempDir::new().unwrap();
    let base = fwd(dir.path());
    let gate_lock = fwd(&repo().join("scripts/gate-lock.sh"));
    let script = format!(
        r#"
L='{base}/r.lock'
read -r -a F <<< "$(cat /proc/$$/stat)"
printf 'token live\npid %s\nstart %s\ncmd shell\nstarted x\n' "$$" "${{F[21]}}" > "$L"
KELIR_GATE_LOCK="$L" bash '{gate_lock}' --timeout 4 -- echo NOT-RUN
echo "exit=$?"
"#
    );
    let out = run_bash(&script, &[]);
    let text = text(&out);
    assert!(
        !text.contains("NOT-RUN"),
        "the command must not run:\n{text}"
    );
    assert!(text.contains("exit=75"), "a timeout exits 75:\n{text}");
}

/// An outside driver's lock (the documented interop) is a plain `mkdir` with no
/// record of ours. It is waited on and never taken over (S2).
#[test]
fn an_outside_drivers_mkdir_lock_is_respected() {
    let dir = tempfile::TempDir::new().unwrap();
    let base = fwd(dir.path());
    let gate_lock = fwd(&repo().join("scripts/gate-lock.sh"));
    let script = format!(
        r#"
L='{base}/d.lock'; mkdir "$L"
KELIR_GATE_LOCK="$L" bash '{gate_lock}' --timeout 4 -- echo NOT-RUN
echo "exit=$?"
[ -d "$L" ] && echo STILL-THERE
"#
    );
    let out = run_bash(&script, &[]);
    let text = text(&out);
    assert!(!text.contains("NOT-RUN"), "never stolen:\n{text}");
    assert!(
        text.contains("exit=75"),
        "it waits and gives up with 75:\n{text}"
    );
    assert!(
        text.contains("STILL-THERE"),
        "the driver's lock survives:\n{text}"
    );
}

// -------------------------------------------------------------- prune decoys

/// The prune safety property, on a throwaway HOME with decoys: a directory this
/// script created is a candidate, and a user project with a matching name, a
/// pattern-only dir, a symlink with a matching name, a dir with a marker but a
/// foreign file inside, and a nested dir are never touched, even at `--keep 0`
/// and with `--yes`.
#[test]
fn prune_with_decoys_removes_only_dirs_it_created() {
    let dir = tempfile::TempDir::new().unwrap();
    let home = fwd(dir.path());
    let script = format!(
        r#"
H='{home}'
mkwork() {{
  mkdir -p "$H/$1/src-tree" "$H/$1/logs"
  printf 'token t.%s\ncreated x\njob %s\ncreator remote-check.sh\n' "$$" "$1" > "$H/$1/.kelir-workdir"
  echo x > "$H/$1/k.b64"; age_hours "$2" "$H/$1"
}}
mkwork kelir-verify-alpha 500
mkwork kelir-verify-alpha 400
mkwork kelir-verify-beta 500
# decoys, all of them old enough to be a prune candidate by age
mkdir -p "$H/kelir-verify-notes/.git"; echo mine > "$H/kelir-verify-notes/README"
mkdir -p "$H/kelir-verify-noMk"
mkdir -p "$H/kelir-verify-away"; printf 'token t\n' > "$H/kelir-verify-away/.kelir-workdir"
echo foreign > "$H/kelir-verify-away/README"
mkdir -p "$H/target-of-link" "$H/sub/kelir-verify-nested"
echo p > "$H/target-of-link/p"
# A real symlink, not a copied dir: MSYS would otherwise make a "helpful"
# shortcut. If the platform still cannot, the decoy is skipped rather than
# silently weaker.
MSYS=winsymlinks:nativestrict ln -s "$H/target-of-link" "$H/kelir-verify-linky" 2>/dev/null || true
age_hours 500 "$H/kelir-verify-notes" "$H/kelir-verify-noMk" \
  "$H/kelir-verify-away" "$H/sub/kelir-verify-nested"
age_link_hours 500 "$H/kelir-verify-linky"
HOME="$H" KEEP=0 YES=1 JOB='' bash -c "$FAR"
echo "--- survivors:"
for d in kelir-verify-notes kelir-verify-noMk kelir-verify-away sub/kelir-verify-nested target-of-link; do
  [ -e "$H/$d" ] && echo "kept $d" || echo "GONE $d"
done
if [ -L "$H/kelir-verify-linky" ]; then echo "kept the symlink"; else echo "no symlink support"; fi
[ -f "$H/target-of-link/p" ] || echo "GONE the symlink target"
[ -e "$H/kelir-verify-alpha" ] && echo "GONE a created work dir" || echo "removed kelir-verify-alpha"
[ -e "$H/kelir-verify-beta" ] && echo "GONE a created work dir" || echo "removed kelir-verify-beta"
"#
    );
    let out = run_bash(&script, &[("FAR", far_prune())]);
    let text = text(&out);
    for survivor in [
        "kept kelir-verify-notes",
        "kept kelir-verify-noMk",
        "kept kelir-verify-away",
        "kept sub/kelir-verify-nested",
        "kept target-of-link",
        "kept the symlink",
        "removed kelir-verify-alpha",
        "removed kelir-verify-beta",
    ] {
        assert!(text.contains(survivor), "{survivor}:\n{text}");
    }
    assert!(!text.contains("GONE"), "nothing it did not create:\n{text}");
}

/// Keeping is per job (S8): the newest `--keep` of *each* job, so an idle job
/// keeps its warm target while a busier job's older dirs are retired.
#[test]
fn prune_keeps_the_newest_per_job_not_the_newest_overall() {
    let dir = tempfile::TempDir::new().unwrap();
    let home = fwd(dir.path());
    let script = format!(
        r#"
H='{home}'
mkwork() {{
  mkdir -p "$H/$1/src-tree"
  printf 'token t\ncreated x\njob %s\n' "$1" > "$H/$1/.kelir-workdir"
  age_hours "$2" "$H/$1"
}}
mkwork kelir-verify-alpha 100
mkwork kelir-verify-alpha 90
mkwork kelir-verify-alpha 80
mkwork kelir-verify-beta 900
HOME="$H" KEEP=1 YES=1 JOB='' bash -c "$FAR"
echo "--- survivors:"
for d in kelir-verify-alpha kelir-verify-beta; do
  [ -d "$H/$d" ] && echo "has $d" || echo "lost $d"
done
"#
    );
    let out = run_bash(&script, &[("FAR", far_prune())]);
    let text = text(&out);
    assert!(
        text.contains("has kelir-verify-alpha"),
        "alpha keeps its newest:\n{text}"
    );
    assert!(
        text.contains("has kelir-verify-beta"),
        "the idle job's only dir is kept, though it is the oldest overall:\n{text}"
    );
}

/// A dry run is the default: nothing is removed without `--yes`, which is what
/// makes the first call on the real box safe.
#[test]
fn prune_removes_nothing_without_yes() {
    let dir = tempfile::TempDir::new().unwrap();
    let home = fwd(dir.path());
    let script = format!(
        r#"
H='{home}'
mkdir -p "$H/kelir-verify-old/src-tree"
printf 'token t\ncreated x\njob old\n' > "$H/kelir-verify-old/.kelir-workdir"
age_hours 500 "$H/kelir-verify-old"
HOME="$H" KEEP=0 YES=0 JOB='' bash -c "$FAR"
[ -d "$H/kelir-verify-old" ] && echo "survived the dry run"
"#
    );
    let out = run_bash(&script, &[("FAR", far_prune())]);
    let text = text(&out);
    assert!(
        text.contains("survived the dry run"),
        "a dry run removes nothing:\n{text}"
    );
    assert!(
        text.contains("--yes removes it"),
        "and says how to remove it:\n{text}"
    );
}

// ------------------------------------------------------- gate reporting

/// The shell helpers of `scripts/gate.sh` that turn a nextest log into the
/// gate's summary, called exactly as the gate calls them.
fn gate_report(script: &str) -> String {
    let gate = fwd(&repo().join("scripts/gate.sh"));
    let prog = format!(
        r#"
set -uo pipefail
log=/dev/null
say() {{ echo "$*"; }}
eval "$(sed -n '/^nextest_names()/,/^}}/p' '{gate}')"
eval "$(sed -n '/^nextest_report()/,/^}}/p' '{gate}')"
eval "$(sed -n '/^nextest_names_report()/,/^}}/p' '{gate}')"
{script}
"#
    );
    text(&run_bash(&prog, &[]))
}

/// A leaked child process must fail the gate (S5). AGENTS.md makes it a hard
/// rule; before this a leaking test passed with exit 0 and the gate went green.
#[test]
fn a_leaking_test_fails_the_gate_and_is_named() {
    let profile = read(".config/nextest.toml");
    assert!(
        profile.contains("leak-timeout") && profile.contains("result = \"fail\""),
        "nextest must treat a leaked process as a failure:\n{profile}"
    );
    let dir = tempfile::TempDir::new().expect("temp dir");
    let log = dir.path().join("nextest.log");
    std::fs::write(
        &log,
        "   LKFAIL [   0.125s] (1/1) kelir::buildinfra_probe probe_leaks_a_child
     Summary [   0.407s] 1 test run: 0 passed, 1 failed (1 due to being leaky), 0 skipped
",
    )
    .expect("write the sample log");
    let out = gate_report(&format!(
        "nextest_report '{log}'; echo \"report-rc=$?\"",
        log = fwd(&log)
    ));
    assert!(out.contains("report-rc=1"), "a leak fails the gate:\n{out}");
    assert!(out.contains("LEAKED PROCESSES"), "it is named:\n{out}");
    assert!(out.contains("probe_leaks_a_child"), "by test name:\n{out}");
}

/// A failing nextest run names the failed tests in the gate's own summary (S11),
/// and a flake is counted per test, not per printed line.
#[test]
fn a_failing_run_names_its_tests_and_counts_a_flake_once() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let log = dir.path().join("nextest.log");
    std::fs::write(
        &log,
        "  TRY 1 FAIL [   0.01s] (---) kelir::suite a_permanent_failure
  TRY 2 PASS [   0.01s] (1/3) kelir::suite a_flake
  TRY 3 FAIL [   0.01s] (2/3) kelir::suite a_permanent_failure
   FLAKY 2/3 [   0.01s] (1/3) kelir::suite a_flake
     Summary [   0.24s] 3 tests run: 1 passed (1 flaky), 1 failed, 1 skipped
",
    )
    .expect("write the sample log");
    let out = gate_report(&format!(
        "nextest_names_report '{log}'; nextest_report '{log}'; echo \"report-rc=$?\"",
        log = fwd(&log)
    ));
    let failed_block = out
        .split("FAILED TESTS:")
        .nth(1)
        .and_then(|rest| rest.split("result:").next())
        .unwrap_or("");
    assert!(
        failed_block.contains("a_permanent_failure"),
        "the failing test is named:\n{out}"
    );
    assert!(
        !failed_block.contains("a_flake"),
        "a flake is not a failure:\n{out}"
    );
    assert!(
        out.contains("RETRIED (a flake to fix, STEER-9): 1 test(s)"),
        "one flake, counted once:\n{out}"
    );
    assert!(
        out.contains("report-rc=0"),
        "a flake is not fatal by default:\n{out}"
    );
}

/// The gate refuses to start the Linux half without a machine name: which
/// machine that is, is not repo content (S9).
#[test]
fn the_linux_half_has_no_built_in_machine_name() {
    // A hostname is never written down here — not even as a negative assertion:
    // the repo is public. The check is structural instead: whenever the gate
    // takes the machine from the flag or the environment it must have no
    // fallback, i.e. the expansion is never of the form `${X:-<literal>}` or
    // `else <literal>`.
    let gate = read("scripts/gate.sh");
    for line in gate.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.split("KELIR_LINUX:-").nth(1) {
            assert!(
                rest.starts_with('}'),
                "KELIR_LINUX must have no default machine, found: {line}"
            );
        }
    }
    let dir = tempfile::TempDir::new().expect("temp dir");
    let out = run_bash(
        &format!(
            "cd '{}' && env -u KELIR_LINUX bash '{}' --linux 2>&1; echo \"rc=$?\"",
            fwd(&repo()),
            fwd(&repo().join("scripts/gate.sh"))
        ),
        &[("KELIR_GATE_LOCK", fwd(&dir.path().join("l.lock")))],
    );
    let text = text(&out);
    assert!(
        text.contains("--linux needs a machine"),
        "it says what to do:\n{text}"
    );
    assert!(text.contains("rc=2"), "and exits 2:\n{text}");
}
