//! `scripts/target-sweep.sh` deletes `target/` directories, so its tests are the
//! evidence that it can only delete what it should. Each test builds a throwaway
//! git repo with real worktrees (the script's candidate set comes from
//! `git worktree list`, not from looking at directory names) and says, for one
//! class of target, that it is offered or skipped.

// Test crates target behaviour, not pedantic style (#23).
#![allow(clippy::pedantic, clippy::nursery)]
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use super::buildinfra_gate::{bash, repo};

/// What cargo writes at the root of every target/ (bford.info/cachedir/).
const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n";

fn fwd(path: &Path) -> String {
    path.to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=kelir-tests",
            "-c",
            "user.email=tests@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("git runs (every checkout the gate runs in has it)");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        text(&out)
    );
    out
}

/// A throwaway repository with one commit and the worktrees a test asks for.
struct Farm {
    tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Farm {
    fn new() -> Farm {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).expect("repo dir");
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("README"), "one\n").expect("README");
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "one"]);
        Farm { tmp, root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    fn worktree(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        git(
            &self.root,
            &["worktree", "add", "-q", "-b", name, &fwd(&path)],
        );
        path
    }

    /// A cargo target/ with a blob of `mb` megabytes, aged `hours` hours.
    fn target(&self, wt: &Path, mb: usize, hours: u32) -> PathBuf {
        let target = wt.join("target");
        std::fs::create_dir_all(target.join("debug")).expect("target/debug");
        std::fs::write(target.join("CACHEDIR.TAG"), TAG).expect("CACHEDIR.TAG");
        std::fs::write(target.join("debug").join("blob"), vec![0x5a; mb << 20]).expect("blob");
        age(&target, hours);
        target
    }
}

/// Age everything under `path` without a fixed sleep, the way the prune tests
/// do: GNU `date -d` takes a relative string, BSD/macOS takes an absolute one
/// made by `date -v`, and `touch -d` is handed the same absolute string on both.
fn age(path: &Path, hours: u32) {
    let script = format!(
        r#"
p='{}'
if date -d "$H hours ago" +%Y-%m-%dT%H:%M:%S >/dev/null 2>&1; then
  stamp=$(date -d "$H hours ago" +%Y-%m-%dT%H:%M:%S)
else
  stamp=$(date -v-"${{H}}"H +%Y-%m-%dT%H:%M:%S)
fi
find "$p" -exec touch -d "$stamp" {{}} +
"#,
        fwd(path).replace('\'', r"'\''")
    );
    let mut cmd = Command::new(bash());
    cmd.args(["-c", &script]).env("H", hours.to_string());
    crate::support::scrub_std(&mut cmd);
    let out = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("bash runs");
    assert!(
        out.status.success(),
        "cannot age {}: {}",
        path.display(),
        text(&out)
    );
}

fn sweep(farm: &Farm, args: &[&str], cwd: Option<&Path>) -> Output {
    let mut cmd = Command::new(bash());
    cmd.arg(repo().join("scripts").join("target-sweep.sh"))
        .arg("--repo")
        .arg(&farm.root)
        .args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    crate::support::scrub_std(&mut cmd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.output().expect("the sweep script runs")
}

/// The line the script printed about `path`, if any.
fn line_for<'a>(out: &'a str, path: &Path) -> Option<&'a str> {
    let needle = fwd(path);
    out.lines().find(|line| line.contains(&needle))
}

// --------------------------------------------------------------------- tests

/// The whole point: a stale target of another worktree is offered, and `--yes`
/// removes it while the worktree and its source stay exactly where they were.
#[test]
fn a_stale_target_is_removed_and_the_worktree_survives() {
    let farm = Farm::new();
    let stale = farm.worktree("stale");
    let target = farm.target(&stale, 2, 500);

    let out = sweep(
        &farm,
        &["--older-than", "24", "--min-mb", "1", "--yes"],
        None,
    );
    let shown = text(&out);
    assert!(out.status.success(), "{}", text(&out));
    let line =
        line_for(&shown, &stale).unwrap_or_else(|| panic!("no line for {stale:?}:\n{shown}"));
    assert!(
        line.starts_with("remove "),
        "it is removed, not skipped: {line}"
    );
    assert!(!target.exists(), "target/ is gone");
    assert!(
        stale.join("README").is_file(),
        "the worktree's files are not"
    );
}

/// A dry run is the default and changes nothing, however stale the target is.
#[test]
fn a_dry_run_removes_nothing_and_says_so() {
    let farm = Farm::new();
    let stale = farm.worktree("stale");
    let target = farm.target(&stale, 2, 500);

    let out = sweep(&farm, &["--older-than", "24", "--min-mb", "1"], None);
    let shown = text(&out);
    assert!(out.status.success(), "{}", text(&out));
    let line =
        line_for(&shown, &stale).unwrap_or_else(|| panic!("no line for {stale:?}:\n{shown}"));
    assert!(
        line.contains("dry run"),
        "the line says what it did not do: {line}"
    );
    assert!(
        shown.contains("reclaimable"),
        "the summary says how much:\n{shown}"
    );
    assert!(target.exists(), "a dry run deleted target/");
}

/// A target somebody used a minute ago is not a candidate: this is the only
/// protection a build that is running right now has.
#[test]
fn a_target_written_recently_is_skipped() {
    let farm = Farm::new();
    let fresh = farm.worktree("fresh");
    let target = farm.target(&fresh, 2, 0);

    let out = sweep(
        &farm,
        &["--older-than", "24", "--min-mb", "1", "--yes"],
        None,
    );
    let shown = text(&out);
    assert!(out.status.success(), "{}", text(&out));
    let line =
        line_for(&shown, &fresh).unwrap_or_else(|| panic!("no line for {fresh:?}:\n{shown}"));
    assert!(line.starts_with("skip "), "{line}");
    assert!(line.contains("written in the last 24h"), "{line}");
    assert!(target.exists(), "a fresh target was deleted");
}

/// The caller's worktree and the script's own are protected: deleting the
/// target of the checkout you are standing in is a footgun, not a cleanup.
#[test]
fn the_caller_s_worktree_is_never_swept() {
    let farm = Farm::new();
    let here = farm.worktree("here");
    let target = farm.target(&here, 2, 500);

    let out = sweep(
        &farm,
        &["--older-than", "24", "--min-mb", "1", "--yes"],
        Some(&here),
    );
    let shown = text(&out);
    assert!(out.status.success(), "{}", text(&out));
    let line = line_for(&shown, &here).unwrap_or_else(|| panic!("no line for {here:?}:\n{shown}"));
    assert!(line.contains("the caller's worktree"), "{line}");
    assert!(target.exists(), "the caller's target was deleted");
}

/// Cargo's `CACHEDIR.TAG` is the proof that a directory named `target` is a
/// cache and not somebody's data; without it the directory is left alone. So is
/// a directory that is not a worktree at all, even if it looks exactly like one.
#[test]
fn a_directory_cargo_did_not_write_is_never_touched() {
    let farm = Farm::new();
    let wt = farm.worktree("decoy");
    let decoy = wt.join("target");
    std::fs::create_dir_all(decoy.join("debug")).expect("decoy");
    std::fs::write(decoy.join("debug").join("precious"), "keep me").expect("precious");
    age(&decoy, 500);

    let stray = farm.path("stray");
    std::fs::create_dir_all(stray.join("target").join("debug")).expect("stray");
    std::fs::write(stray.join("target").join("CACHEDIR.TAG"), TAG).expect("stray tag");
    std::fs::write(stray.join("target").join("debug").join("blob"), "x").expect("stray blob");
    age(&stray.join("target"), 500);

    let out = sweep(
        &farm,
        &["--older-than", "24", "--min-mb", "0", "--yes"],
        None,
    );
    let shown = text(&out);
    assert!(out.status.success(), "{}", text(&out));
    let line = line_for(&shown, &wt).unwrap_or_else(|| panic!("no line for {wt:?}:\n{shown}"));
    assert!(line.contains("no cargo CACHEDIR.TAG"), "{line}");
    assert!(
        decoy.join("debug").join("precious").is_file(),
        "the decoy was deleted"
    );
    assert!(
        stray.join("target").join("CACHEDIR.TAG").is_file(),
        "a directory git does not list as a worktree was touched"
    );
}

/// A worktree git reports `locked` is being kept for a reason; the sweep
/// respects it instead of guessing.
#[test]
fn a_locked_worktree_is_skipped() {
    let farm = Farm::new();
    let wt = farm.worktree("locked");
    let target = farm.target(&wt, 2, 500);
    git(
        &farm.root,
        &["worktree", "lock", "--reason", "in use", &fwd(&wt)],
    );

    let out = sweep(
        &farm,
        &["--older-than", "24", "--min-mb", "1", "--yes"],
        None,
    );
    let shown = text(&out);
    assert!(out.status.success(), "{}", text(&out));
    let line = line_for(&shown, &wt).unwrap_or_else(|| panic!("no line for {wt:?}:\n{shown}"));
    assert!(line.contains("locked worktree"), "{line}");
    assert!(target.exists(), "a locked worktree's target was deleted");
}

/// `--min-mb` keeps a sweep from churning through targets too small to matter,
/// and a symlinked `target` is not something cargo wrote: both survive.
#[test]
fn small_targets_and_symlinks_are_skipped() {
    let farm = Farm::new();
    let small = farm.worktree("small");
    let small_target = farm.target(&small, 1, 500);

    let out = sweep(
        &farm,
        &["--older-than", "24", "--min-mb", "5", "--yes"],
        None,
    );
    let shown = text(&out);
    assert!(out.status.success(), "{}", text(&out));
    let line =
        line_for(&shown, &small).unwrap_or_else(|| panic!("no line for {small:?}:\n{shown}"));
    assert!(line.contains("smaller than --min-mb 5"), "{line}");
    assert!(small_target.exists(), "a small target was deleted");

    #[cfg(unix)]
    {
        let link = farm.worktree("linked");
        let elsewhere = farm.path("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("elsewhere");
        std::fs::write(elsewhere.join("CACHEDIR.TAG"), TAG).expect("tag");
        std::fs::write(elsewhere.join("blob"), vec![0x5a; 2 << 20]).expect("blob");
        age(&elsewhere, 500);
        std::os::unix::fs::symlink(&elsewhere, link.join("target")).expect("symlink");

        let out = sweep(
            &farm,
            &["--older-than", "24", "--min-mb", "1", "--yes"],
            None,
        );
        let shown = text(&out);
        assert!(out.status.success(), "{}", text(&out));
        let line =
            line_for(&shown, &link).unwrap_or_else(|| panic!("no line for {link:?}:\n{shown}"));
        assert!(line.contains("symlink"), "{line}");
        assert!(
            elsewhere.join("blob").is_file(),
            "a symlinked target was deleted"
        );
    }
}
