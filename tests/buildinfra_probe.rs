//! Probes for the reviewer of BUILD-INFRA: deliberately flaky, failing and
//! leaking tests, all `#[ignore]`d so the normal suite never runs them. Run with
//! `cargo nextest run --run-ignored only -E 'binary(buildinfra_probe)'` or
//! `bash scripts/gate.sh -- --run-ignored only -E 'binary(buildinfra_probe)'`
//! to see how the gate reports a retried test, a failing test and a leaked child.

fn attempt() -> u32 {
    std::env::var("NEXTEST_ATTEMPT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}

#[test]
#[ignore = "probe: fails on the first attempt, passes on the retry"]
fn probe_flaky_fails_first_attempt_only() {
    assert!(attempt() >= 2, "probe: deliberate first-attempt failure");
}

#[test]
#[ignore = "probe: always fails"]
fn probe_always_fails() {
    panic!("probe: deliberate permanent failure");
}

#[test]
#[ignore = "probe: leaves a child process running past the test"]
fn probe_leaks_a_child() {
    // A short-lived (20 s) child that the test does not wait for or kill.
    let mut c = if cfg!(windows) {
        let mut c = std::process::Command::new("powershell");
        c.args(["-NoProfile", "-Command", "Start-Sleep 20"]);
        c
    } else {
        let mut c = std::process::Command::new("sleep");
        c.arg("20");
        c
    };
    let child = c.spawn().expect("spawn");
    println!("probe child pid {}", child.id());
    std::mem::forget(child);
}
