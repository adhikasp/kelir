//! buildinfra guard tests: every_gate_stage_logs_to_its_o, the_gate_reports_the_summary_l, the_gate_script_keeps_the_meas, the_load_helper_kills_only_the, the_remote_script_reuses_its_w, the_scripts_name_bash_and_are_.
#![allow(clippy::pedantic, clippy::nursery)]

use super::buildinfra_gate::*;

/// The load helper starts its busy loops itself and kills exactly those pids,
/// on every path; it never signals a group or an image name.
#[test]
fn the_load_helper_kills_only_the_pids_it_started() {
    let load = read("scripts/gate-load.sh");
    assert!(load.contains("load_pids+=(\"$!\")"), "it records the pids");
    assert!(load.contains("kill -9 \"$pid\""), "it kills by pid");
    let lowered = load.to_lowercase();
    assert!(
        !lowered.contains("killall") && !lowered.contains("pkill"),
        "never kill by image name"
    );
    assert!(
        !load.contains("kill -9 0") && !load.contains("kill -INT 0"),
        "never broadcast a signal to a group"
    );
    assert!(
        load.contains("trap 'stop_load' EXIT INT TERM"),
        "the loops die with the script, whatever happens"
    );
    assert!(
        load.contains("--retries 0"),
        "a load run measures the flake, so it must not retry it"
    );
}

#[test]
fn the_remote_script_reuses_its_work_dir_and_can_prune() {
    let remote = read("scripts/remote-check.sh");
    assert!(
        !remote.contains("rm -rf src-tree && git clone"),
        "the work dir is reused, not recloned every run"
    );
    for (needle, why) in [
        ("fetch -q --force", "the reused clone is fast-forwarded"),
        (
            "checkout -q -f --detach",
            "the work tree is forced to the commit",
        ),
        ("git clean -qfdx -e target", "stale files go, target/ stays"),
        ("prune", "a prune subcommand"),
        ("cargo-nextest", "the Linux gate uses nextest"),
        (
            ".kelir-workdir",
            "a created work dir carries the marker prune requires",
        ),
        (
            "refuse_reason",
            "prune decides per candidate, not by name alone",
        ),
        ("symlink", "prune refuses a symlink"),
        (
            "pwd -P",
            "prune resolves the real path before accepting one",
        ),
    ] {
        assert!(remote.contains(needle), "{why}: {needle} is missing");
    }
    assert!(
        remote.contains("kill -0"),
        "prune skips a work dir a live run is in"
    );
    // No fixed /tmp log names: two jobs' Linux gates share the box (S6).
    assert!(
        !remote.contains("/tmp/fmt.log") && !remote.contains("/tmp/nx.log"),
        "far-side logs live in a per-run dir, not at fixed /tmp paths"
    );
    assert!(
        remote.contains("logdir="),
        "the far side makes a log dir per run"
    );
}
