//! M8 fix round: the review's findings that are proved against the real binary
//! (F5 the builtin skills are reachable, F6 project gates need trust).
//!
//! Every run goes through the `Sandbox` Drop-guard.

mod support;

use std::path::Path;

use support::*;

fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

fn fwd(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().replace('\\', "/")
}

// ------------------------------------------------------------------------ F5

fn kelir_docs(args: &[&str]) -> (i32, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_kelir"))
        .arg("docs")
        .args(args)
        .env("KELIR_HOME", std::env::temp_dir())
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// F5: `kelir docs --skill <name>` prints a builtin skill whole, front matter and
/// body (the model has no path to read it from).
#[test]
fn docs_skill_prints_each_builtin_skill_in_full() {
    for (name, needle) in [
        ("herdr", "Never kill, remove, interrupt"),
        ("delegation", "kelir ask"),
        ("kelir", "kelir docs"),
    ] {
        let (code, out, err) = kelir_docs(&["--skill", name]);
        assert_eq!(code, 0, "{name}: {err}");
        assert!(
            out.starts_with("---\nname: ") || out.starts_with("---\r\nname: "),
            "{out}"
        );
        assert!(out.contains(&format!("name: {name}")), "{out}");
        assert!(out.contains(needle), "{name}: {needle:?} missing");
    }
}

#[test]
fn docs_skill_with_an_unknown_name_lists_the_builtin_ones() {
    let (code, _out, err) = kelir_docs(&["--skill", "nope"]);
    assert_eq!(code, 1);
    for name in ["kelir", "delegation", "herdr"] {
        assert!(err.contains(name), "{err}");
    }
}

/// F5: the system prompt the model actually receives names each builtin skill with
/// its description and the command that reads it.
#[tokio::test]
async fn the_system_prompt_sent_to_the_model_advertises_the_builtin_skills() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let sandbox = Sandbox::new("");
    let out = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "hi"], &[])
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    let requests = mock.requests();
    let system = requests[0].body["messages"][0]["content"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| requests[0].body["messages"][0]["content"].to_string());
    for name in ["kelir", "delegation", "herdr"] {
        assert!(
            system.contains(&format!("- {name} - ")),
            "{name} missing in:\n{system}"
        );
    }
    assert!(system.contains("kelir docs --skill <name>"), "{system}");
}

// ------------------------------------------------------------------------ F6

/// A gate that writes `<dir>/ran` when it runs.
fn recorder(dir: &Path) -> String {
    let script = dir.join("rec.py");
    std::fs::write(
        &script,
        "import sys,json\nsys.stdin.read()\nopen(sys.argv[1],'w').write('ran')\nprint(json.dumps({'decision':'allow'}))\n",
    )
    .unwrap();
    format!(
        "{} \"{}\" \"{}\"",
        python(),
        fwd(&script),
        fwd(dir.join("ran"))
    )
}

/// A run whose project layer is `.kelir/config.toml` in the cwd (the explicit
/// `KELIR_CONFIG` that every other test uses would switch the layers off) and
/// whose machine layer is `<state>/no-config/config.toml`.
async fn project_run(
    machine_extra: &str,
    project_toml: &str,
    env: &[(&str, &str)],
) -> (Sandbox, Output, tempfile::TempDir) {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let sandbox = Sandbox::new("");
    let rec = tempfile::tempdir().unwrap();
    let machine_dir = sandbox.state.join("no-config");
    std::fs::create_dir_all(&machine_dir).unwrap();
    std::fs::write(
        machine_dir.join("config.toml"),
        test_config(&machine_extra.replace("{CWD}", &fwd(&sandbox.cwd))),
    )
    .unwrap();
    std::fs::create_dir_all(sandbox.cwd.join(".kelir")).unwrap();
    std::fs::write(
        sandbox.cwd.join(".kelir").join("config.toml"),
        project_toml.replace("{REC}", &recorder(rec.path())),
    )
    .unwrap();
    let mut all = vec![("KELIR_CONFIG", "")];
    all.extend_from_slice(env);
    let out = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "go"], &all)
        .await;
    (sandbox, out, rec)
}

const PROJECT_GATE: &str = "[gates]\nsession_start = '{REC}'\n";

fn notes(sandbox: &Sandbox) -> Vec<String> {
    sandbox
        .log(&sandbox.only_session())
        .iter()
        .filter(|e| e["type"] == "error" && e["where"] == "gate")
        .map(|e| e["message"].as_str().unwrap_or("").to_string())
        .collect()
}

/// F6: a cloned repo's `.kelir/config.toml` `[gates]` does not run by default; the
/// log says why, and the session itself is unaffected.
#[tokio::test]
async fn a_project_config_gate_does_not_run_in_an_untrusted_project() {
    let (sandbox, out, rec) = project_run("", PROJECT_GATE, &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!rec.path().join("ran").exists(), "an untrusted gate ran");
    let log = sandbox.log(&sandbox.only_session());
    assert!(log.iter().all(|e| e["type"] != "gate"), "{log:?}");
    let notes = notes(&sandbox);
    assert!(
        notes
            .iter()
            .any(|n| n.contains("not trusted") && n.contains("project config")),
        "{notes:?} {log:?}"
    );
}

#[tokio::test]
async fn kelir_trust_project_lets_the_project_gate_run() {
    let (sandbox, out, rec) = project_run("", PROJECT_GATE, &[("KELIR_TRUST_PROJECT", "1")]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        rec.path().join("ran").exists(),
        "a trusted gate did not run"
    );
    assert!(
        sandbox
            .log(&sandbox.only_session())
            .iter()
            .any(|e| e["type"] == "gate")
    );
}

#[tokio::test]
async fn trusted_projects_in_the_machine_config_lets_the_project_gate_run() {
    let (_sandbox, out, rec) =
        project_run("trusted_projects = ['{CWD}']\n", PROJECT_GATE, &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(rec.path().join("ran").exists());
}

/// A trusted directory above the cwd covers it; an unrelated one does not.
#[tokio::test]
async fn trusted_projects_names_a_directory_not_a_prefix_of_a_name() {
    let (_sandbox, out, rec) =
        project_run("trusted_projects = ['{CWD}-other']\n", PROJECT_GATE, &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!rec.path().join("ran").exists());
}

/// The project cannot vouch for itself.
#[tokio::test]
async fn a_project_config_cannot_trust_itself() {
    let toml = format!("trusted_projects = ['{}']\n{PROJECT_GATE}", "/");
    let (_sandbox, out, rec) = project_run("", &toml, &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!rec.path().join("ran").exists());
    assert!(out.stderr.contains("trusted_projects"), "{}", out.stderr);
}

/// The machine layer's own gates are never subject to the project's trust.
#[tokio::test]
async fn a_machine_config_gate_runs_in_an_untrusted_project() {
    let rec = tempfile::tempdir().unwrap();
    let machine = format!("[gates]\nsession_start = '{}'\n", recorder(rec.path()));
    let (_sandbox, out, _own) = project_run(&machine, "", &[]).await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(rec.path().join("ran").exists());
}

/// A skill under the project's `.kelir/skills` that binds a gate is project code
/// too.
#[tokio::test]
async fn a_project_skill_gate_does_not_run_in_an_untrusted_project() {
    let mock = Mock::start(vec![Reply::Sse(sse(&text_turn("ok", 0)))]).await;
    let sandbox = Sandbox::new("");
    let rec = tempfile::tempdir().unwrap();
    let skill = sandbox.cwd.join(".kelir").join("skills").join("watch");
    std::fs::create_dir_all(skill.join("scripts")).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: watch\ndescription: watches\nmetadata: {kelir-gate: turn_end}\n---\nBody.\n",
    )
    .unwrap();
    std::fs::write(
        skill.join("scripts").join("turn_end.py"),
        format!(
            "import sys\nopen(r'{}','w').write('ran')\n",
            fwd(rec.path().join("ran"))
        ),
    )
    .unwrap();
    let out = sandbox
        .kelir(&mock.base_url(), &["run", "--json", "go"], &[])
        .await;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(!rec.path().join("ran").exists());
    assert!(
        notes(&sandbox)
            .iter()
            .any(|n| n.contains("not trusted") && n.contains("project skill watch"))
    );
}
