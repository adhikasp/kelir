use std::sync::atomic::{AtomicU64, Ordering};

/// Windows job names handed out by this process.
#[cfg(windows)]
pub(super) static JOBS: AtomicU64 = AtomicU64::new(0);

/// Gate runs started by this process (part of each run's `KELIR_GATE_RUN` token).
static RUNS: AtomicU64 = AtomicU64::new(0);

/// At most this many gate processes are alive in one process at any moment.
pub const MAX_LIVE: usize = 8;

/// A child session (a descendant of the root) may have at most this many gate processes alive.
pub const MAX_LIVE_CHILD: usize = 2;

/// The most gate processes one root and all its live descendants can have alive.
pub const fn root_bound(max_live_children: usize) -> usize {
    MAX_LIVE + max_live_children * MAX_LIVE_CHILD
}

/// The cap that applies to this process (set once by a child session).
static CAP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(MAX_LIVE);

/// A session that is a child lowers its cap to [`MAX_LIVE_CHILD`].
pub fn limit_for_child() {
    CAP.store(MAX_LIVE_CHILD, std::sync::atomic::Ordering::SeqCst);
}

static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn live() -> usize {
    LIVE.load(std::sync::atomic::Ordering::SeqCst)
}

/// One of the [`MAX_LIVE`] slots; released on drop, whatever happened.
pub(crate) struct Slot;

impl Slot {
    pub(crate) fn take() -> Result<Slot, String> {
        use std::sync::atomic::Ordering::SeqCst;
        let mut now = LIVE.load(SeqCst);
        loop {
            let cap = CAP.load(SeqCst);
            if now >= cap {
                return Err(format!(
                    "{cap} gates are already running; this run was skipped"
                ));
            }
            match LIVE.compare_exchange(now, now + 1, SeqCst, SeqCst) {
                Ok(_) => return Ok(Slot),
                Err(seen) => now = seen,
            }
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The env var that marks every process of one gate run.
pub const RUN_ENV: &str = "KELIR_GATE_RUN";

/// A token no other run shares: this process, a counter and the clock.
pub(crate) fn run_token() -> String {
    let n = RUNS.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{n}-{nanos:x}", std::process::id())
}

/// Unix: after a gate run, nothing of it is left, even what left its process
/// group, and a killed session cannot orphan it.
#[cfg(unix)]
pub mod reap {
    use super::RUN_ENV;

    /// Kill the gate's descendants (by parent links) and every process that carries the run's token in its
    /// environment, until none is left.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn kill_run(root: u32, token: &str) {
        let me = std::process::id();
        let needle = format!("{RUN_ENV}={token}");
        let mut quiet = 0;
        for _ in 0..20 {
            let table = process_table();
            let mut victims: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
            let mut frontier = vec![root];
            while let Some(parent) = frontier.pop() {
                for (pid, ppid) in &table {
                    if *ppid == parent && *pid != me && victims.insert(*pid) {
                        frontier.push(*pid);
                    }
                }
            }
            for pid in marked_pids(&needle) {
                if pid != me {
                    victims.insert(pid);
                }
            }
            // A descendant can be reparented to init and become invisible to the parent-link walk before `ps` sees
            // it with the run token. Tolerate a couple of empty passes for macOS `ps` lag, then stop.
            if victims.is_empty() {
                quiet += 1;
                if quiet >= 3 {
                    return;
                }
            } else {
                quiet = 0;
                for pid in victims {
                    // Safe: SIGKILL to a pid read from /proc or ps a moment ago.
                    unsafe {
                        libc::kill(pid as libc::pid_t, libc::SIGKILL);
                    }
                }
            }
            // macOS `ps` can lag a freshly forked child, and a daemon that forks just after the
            // gate exits may only appear after a few sweeps; `/proc` on Linux is immediate.
            // Give macOS 20 x 50 ms = 1 s of scanning before the reaper gives up.
            #[cfg(target_os = "macos")]
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn kill_run(_root: u32, _token: &str) {}

    /// `(pid, ppid)` of every process, read from `/proc` on Linux.
    #[cfg(target_os = "linux")]
    fn process_table() -> Vec<(u32, u32)> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                // `pid (comm) S ppid …`: comm may hold spaces and parentheses.
                let rest = &stat[stat.rfind(')')? + 1..];
                let ppid: u32 = rest.split_whitespace().nth(1)?.parse().ok()?;
                Some((pid, ppid))
            })
            .collect()
    }

    /// macOS has no `/proc`: `ps` is the portable way to read the parent
    /// links. Only pid/ppid are parsed (no command line, no environment).
    #[cfg(target_os = "macos")]
    fn process_table() -> Vec<(u32, u32)> {
        let output = crate::sys::runner::std_command_name("ps")
            .args(["-ax", "-o", "pid=,ppid="])
            .output();
        let Ok(output) = output else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pid: u32 = fields.next()?.parse().ok()?;
                let ppid: u32 = fields.next()?.parse().ok()?;
                Some((pid, ppid))
            })
            .collect()
    }

    /// macOS: one `ps` call lists both pids and their environments. The spelling is probed once
    /// (first successful call wins) and cached; each reaper sweep then costs exactly one `ps`.
    /// The alternatives are defensive: no CI log has shown a failure for any one spelling.
    #[cfg(target_os = "macos")]
    pub(crate) fn ps_args() -> &'static [&'static str] {
        use std::sync::OnceLock;
        static ARGS: OnceLock<&'static [&'static str]> = OnceLock::new();
        ARGS.get_or_init(|| {
            const CANDIDATES: [&[&str]; 3] = [
                &["axww", "-E", "-o", "pid=,command="],
                &["axww", "e", "-o", "pid=,command="],
                &["axwwe", "-o", "pid=,command="],
            ];
            for args in CANDIDATES {
                if crate::sys::runner::std_command_name("ps")
                    .args(args)
                    .output()
                    .is_ok_and(|out| out.status.success())
                {
                    return args;
                }
            }
            // No spelling worked: keep one so a sweep still returns an empty set cheaply.
            CANDIDATES[0]
        })
    }

    #[cfg(target_os = "macos")]
    fn marked_pids(needle: &str) -> std::collections::BTreeSet<u32> {
        let Ok(output) = crate::sys::runner::std_command_name("ps")
            .args(ps_args())
            .output()
        else {
            return std::collections::BTreeSet::new();
        };
        if !output.status.success() {
            return std::collections::BTreeSet::new();
        }
        parse_marked_pids(&String::from_utf8_lossy(&output.stdout), needle)
    }

    /// Pure parser for the `ps` output: a line starts with the pid, the rest
    /// is the command and its `ps e` environment. Only whole entries count.
    #[cfg(target_os = "macos")]
    pub(crate) fn parse_marked_pids(ps: &str, needle: &str) -> std::collections::BTreeSet<u32> {
        ps.lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let pid: u32 = fields.next()?.parse().ok()?;
                line.split_whitespace()
                    .any(|entry| entry == needle)
                    .then_some(pid)
            })
            .collect()
    }

    #[cfg(target_os = "linux")]
    fn marked_pids(needle: &str) -> std::collections::BTreeSet<u32> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return std::collections::BTreeSet::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
                std::fs::read(format!("/proc/{pid}/environ"))
                    .map(|bytes| bytes.split(|b| *b == 0).any(|e| e == needle.as_bytes()))
                    .unwrap_or(false)
                    .then_some(pid)
            })
            .collect()
    }

    /// Owns the cleanup of one run: a guard process that outlives a killed
    /// session, and the final sweep. Dropping it sweeps too (a cancelled run).
    pub struct Reaper {
        pid: u32,
        token: String,
        guard: Option<std::process::Child>,
        done: bool,
    }

    impl Reaper {
        /// `pid` is the gate shell (also its process group).
        pub fn start(pid: u32, token: &str, ttl: std::time::Duration) -> Self {
            let guard = crate::sys::runner::start_guard(&[
                "gate-guard".into(),
                "--parent".into(),
                std::process::id().to_string(),
                "--group".into(),
                pid.to_string(),
                "--token".into(),
                token.to_string(),
                "--ttl".into(),
                ttl.as_secs().to_string(),
            ]);
            Self {
                pid,
                token: token.to_string(),
                guard,
                done: false,
            }
        }

        /// The sweep. `group` also signals the gate's process group (a run that
        /// was cancelled rather than finished).
        pub fn reap(&mut self, group: bool) {
            if self.done {
                return;
            }
            self.done = true;
            if group {
                // Safe: the group this run created.
                unsafe {
                    libc::killpg(self.pid as libc::pid_t, libc::SIGKILL);
                }
            }
            kill_run(self.pid, &self.token);
            if let Some(mut guard) = self.guard.take() {
                let _ = guard.kill();
                let _ = guard.wait();
            }
        }
    }

    impl Drop for Reaper {
        fn drop(&mut self) {
            self.reap(true);
        }
    }

    /// `kelir gate-guard`: wait for the session (the parent) to disappear, then
    /// kill the gate's tree. Gives up after `ttl` seconds either way.
    pub fn guard_main(parent: u32, group: u32, token: &str, ttl: u64) -> u8 {
        let started = std::time::Instant::now();
        while started.elapsed().as_secs() < ttl {
            std::thread::sleep(std::time::Duration::from_millis(200));
            // Safe: signal 0 only probes; getppid has no preconditions.
            let alive = unsafe { libc::kill(parent as libc::pid_t, 0) } == 0
                && unsafe { libc::getppid() } as u32 == parent;
            if !alive {
                // Safe: the gate's own group.
                unsafe {
                    libc::killpg(group as libc::pid_t, libc::SIGKILL);
                }
                kill_run(group, token);
                return 0;
            }
        }
        0
    }
}

/// A kelir started by a gate (`KELIR_GATE_EVENT` is in its environment) runs
/// no gates of its own: a gate never triggers gates.
pub fn started_by_a_gate(marker: Option<&str>) -> bool {
    marker.is_some_and(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;
    use crate::features::gates::*;
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::time::Duration;

    fn def(event: Event, command: &str) -> GateDef {
        GateDef {
            name: name_from_command(command),
            event,
            command: command.to_string(),
            builtin: None,
            options: toml::Table::new(),
            tools: Vec::new(),
            env: BTreeMap::new(),
            origin: "test".into(),
        }
    }

    #[allow(dead_code)]
    fn runner(dir: &Path, defs: Vec<GateDef>) -> Runner {
        std::fs::create_dir_all(dir).unwrap();
        Runner::new(
            defs,
            crate::config::default_shell(),
            dir.to_path_buf(),
            "ses_test".into(),
            dir.to_path_buf(),
        )
        .with_timeout(Duration::from_secs(20))
    }
    // ------------------------------------------------ process sweep

    /// under a POSIX shell on Windows the `/C` of `cmd /C` must survive MSYS.
    #[test]
    fn a_cmd_line_is_adapted_to_a_posix_shell_on_windows_only() {
        let line = script_command(Path::new("C:/g/turn_end.cmd"));
        assert_eq!(line, "cmd /C \"C:/g/turn_end.cmd\"");
        let bash = adapt_for_shell(&line, "C:/Program Files/Git/bin/bash.exe");
        if cfg!(windows) {
            assert_eq!(bash, "cmd //C \"C:/g/turn_end.cmd\"");
            assert_eq!(
                adapt_for_shell("CMD.EXE /c x.bat", "sh"),
                "CMD.EXE //C x.bat"
            );
        } else {
            assert_eq!(bash, line);
        }
        // cmd and PowerShell take `/C` as is; other commands are never touched.
        assert_eq!(adapt_for_shell(&line, "C:/Windows/System32/cmd.exe"), line);
        assert_eq!(adapt_for_shell(&line, "pwsh"), line);
        assert_eq!(adapt_for_shell("python x.py /C", "bash"), "python x.py /C");
    }

    #[test]
    fn project_gates_are_recognised_and_dropped_when_untrusted() {
        let mut project = def(Event::TurnEnd, "echo a");
        project.origin = "project config".into();
        let mut skill = def(Event::TurnEnd, "echo b");
        skill.origin = "project skill watch".into();
        let machine = def(Event::TurnEnd, "echo c");
        assert!(is_project_gate(&project) && is_project_gate(&skill) && !is_project_gate(&machine));
        let all = vec![project, skill, machine];
        let mut notes = Vec::new();
        let kept = apply_trust(all.clone(), false, &mut notes);
        assert_eq!(kept.len(), 1);
        assert_eq!(notes.len(), 2, "{notes:?}");
        let mut none = Vec::new();
        assert_eq!(apply_trust(all, true, &mut none).len(), 3);
        assert!(none.is_empty());
    }

    #[test]
    fn a_trusted_directory_covers_what_is_under_it_and_nothing_beside_it() {
        let root = tempfile::tempdir().unwrap();
        let inside = root.path().join("a").join("b");
        std::fs::create_dir_all(&inside).unwrap();
        let beside = root.path().join("a-other");
        std::fs::create_dir_all(&beside).unwrap();
        let trusted = vec![root.path().join("a")];
        assert!(project_trusted(&inside, &trusted));
        assert!(project_trusted(&root.path().join("a"), &trusted));
        assert!(!project_trusted(&beside, &trusted));
        assert!(!project_trusted(&inside, &[]));
    }

    /// The macOS `ps e` parser matches whole fields only: a process that merely
    /// mentions a prefix of the run token must never be swept.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_token_sweep_matches_only_exact_entries() {
        let ps = concat!(
            "  1 /sbin/launchd\n",
            " 42 bash gate.sh KELIR_GATE_RUN=token123 HOME=/x\n",
            " 43 sleep 3 KELIR_GATE_RUN=token1234\n",
            " 44 echo KELIR_GATE_RUN=token123 done\n",
            " 45 sh -c KELIR_GATE_RUN=token123 sleep\n",
        );
        let token = "KELIR_GATE_RUN=token123";
        assert_eq!(
            reap::parse_marked_pids(ps, token),
            [42, 44, 45].into_iter().collect()
        );
        assert!(reap::parse_marked_pids(ps, "KELIR_GATE_RUN=zzz").is_empty());
        // All three cached `ps` spellings feed the same pure parser; the sample
        // output is identical whatever spelling produced it.
        let spellings: [&[&str]; 3] = [
            &["axww", "-E", "-o", "pid=,command="],
            &["axww", "e", "-o", "pid=,command="],
            &["axwwe", "-o", "pid=,command="],
        ];
        assert!(
            spellings.contains(&reap::ps_args()),
            "{:?}",
            reap::ps_args()
        );
        for args in spellings {
            let out = crate::sys::runner::std_command_name("ps")
                .args(args)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(
                reap::parse_marked_pids(&text, "KELIR_GATE_RUN=not-a-real-token").is_empty(),
                "args={args:?}"
            );
            assert_eq!(
                reap::parse_marked_pids(ps, token),
                [42, 44, 45].into_iter().collect(),
                "args={args:?}"
            );
        }
    }

    /// Linux: the sweep finds a process that left the group by its token.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_sweep_kills_a_setsid_process_that_carries_the_token() {
        use std::os::unix::process::CommandExt as _;
        let token = run_token();
        let mut child = crate::sys::runner::test_command_name("sleep")
            .arg("30")
            .env(RUN_ENV, &token)
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        // Not a descendant of the (unrelated) root, not in any group we signal.
        reap::kill_run(1_000_000, &token);
        let status = child.wait().unwrap();
        assert!(
            !status.success(),
            "the marked process survived the sweep ({pid})"
        );
    }
}
