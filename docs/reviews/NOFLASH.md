# NOFLASH — does kelir still flash console windows on Windows?

Tester run 2026-11-19 (10:00–10:35 local), dev box = Windows 11 with Windows Terminal as the default
terminal application. Question: after the CREATE_NO_WINDOW work, does kelir, or the test suites that
exercise it, still create visible console/terminal windows?

## Verdict

| binary | verdict | spawn site |
|---|---|---|
| **HEAD** (`ee5ceda`, built in a clean worktree) | **FLASH** | every `kelir bg-shim` runs its command (`bash.exe -c ...`) with no console to inherit |
| **WIP snapshot** (`kelir-wip-snapshot.exe`, 09:57:30, sha256 `e029a408…`) | **FLASH** | same site; also every bash call when kelir itself has no console |
| **WIP now** (copy of the working tree `src/` + `tests/` taken ~10:15 and again ~10:30, built in scratch) | **NO FLASH** | none found in any scenario below |

"WIP now" is *not* a commit; it is the implementer's uncommitted tree as it stood then
(`bg.rs`/`proc.rs` last modified 10:10). Re-run the matrix on the final commit
(`scripts/live/flash_matrix2.sh`) before closing the issue.

### The mechanism (why 5-10 windows flash)

Windows Terminal is the default terminal. Any console-subsystem process that starts **without a console to
inherit** makes conhost allocate a new console, which is delegated to Windows Terminal
(`OpenConsole.exe -Embedding`, started by `svchost`) and shows as a window for as long as the process lives.
Evidence line from the HEAD `cancel` run (spy log `flash-cancel-head-canned.spy.log`, 10:08:09):

```
PROC kelir  bg-shim --dir ...\ses_...            ppid=<kelir run>      (no conhost is created for the shim)
PROC bash   "C:\Program Files\Git\bin\bash.exe" -c "sleep 120"   ppid=<bg-shim>
PROC conhost \??\C:\WINDOWS\system32\conhost.exe 0x4              ppid=<that bash.exe>
PROC OpenConsole ... -Embedding                                   ppid=svchost   (Windows Terminal delegation)
** WIN SHOW class=CASCADIA_HOSTING_WINDOW_CLASS "Terminal"   pid=WindowsTerminal
** WIN CREATE/SHOW class=PseudoConsoleWindow vis=1            pid=<that bash.exe>
```

Sites in the HEAD source:
- `src/bg.rs` `spawn_shim` (HEAD line ~521) and `detach()` (~404): `DETACHED_PROCESS` -> the shim has no console.
- `src/bg.rs` `shim_main` (HEAD ~726): spawns the user's command with `CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP`
  and nothing else, so `Git\bin\bash.exe` gets a fresh console when the shim has none: one window per
  `kelir bg run` job, per promoted call.
- `src/proc.rs` `configure` (HEAD ~88): same flags for every bash call; harmless while the session has a
  console (a Herdr pane, a terminal) and a window per call when the session itself has none (see `direct`).

The WIP snapshot still shows the shim without a console (no `conhost` is parented by a shim; the conhost is
parented by its `bash.exe` child), so it predates the fix that works; the current tree gives every
shim/call a windowless console (conhosts are still created, but with no window).

## Results per scenario (attributed visible console windows; each window is counted twice, CREATE + SHOW)

`attributed` = the window's owner chain contains a process of the scenario. `conhost` = conhost processes
started by the scenario. Leftover processes after every run: **none** (see below).

| scenario | launch | HEAD | WIP snapshot | WIP now |
|---|---|---|---|---|
| calls (10 bash calls, 2x5 parallel), canned | pane | 0 win / 0 conhost | 0 / 0 | 0 / 0 |
| calls, canned | detached bash root | 0 / 1 | 0 / 1 | 0 / 1 |
| calls, live (OpenRouter `deepseek/deepseek-v4.1-flash-1017`) | pane | 0 / 0 | 0 / 0 | 0 / 0 |
| calls, live | detached bash root | 0 / 1 | 0 / 1 | not run |
| bg run x10 + promotion (promote_after=2, sleep 6), canned | pane | **16** / 10 | **20** / 10 | 0 / 10 |
| bg, canned | detached bash root | **20** / 11 | **19** / 11 | 0 / 11 |
| bg, live | pane | **20** / 10 | **7** / 10 | 0 / 10 |
| bg, live | detached bash root | **18** / 11 | **19** / 11 | 0 / 11 |
| bg cancel x3 (+ a `bash -c 'sleep & sleep & wait'` tree), canned | pane | **6** / 3 | **4** / 3 | 0 / 3 |
| kill the session mid-call (taskkill /F, no /T), canned | pane | 0 / 0 | 0 / 0 | 0 / 0 |
| mix (5 calls + 3 bg + promoted call), canned | detached bash root | **6** / 4 | **6** / 4 | 0 / 4 |
| mix, canned | hidden new console | **4** / 4 | **6** / 4 | not run |
| **direct**: `kelir.exe` itself started console-less (DETACHED_PROCESS, then hidden console), canned | pane + launcher | **4** + 5 unattributed WT/OpenConsole events (incl. a window for the shim itself) | **0 attributed, but 6 Windows Terminal window shows inside 0.5 s of the scenario's 5 parallel bash `conhost` starts** (timing, not process chain, ties them; chain stops at `svchost`) | 0 windows / 11 conhost |
| TUI (`kelir`), prompt makes a bash call, Ctrl-C twice, canned | pane | 0 / 0 | 0 / 0 | 0 / 0 |
| TUI, live | pane | 0 / 0 | 0 / 0 | 0 / 0 |

Reading the table: the flash is specific to console-less starts: `bg-shim` jobs and promoted calls (always
console-less in HEAD/snapshot), and any bash call when the kelir session has no console. Plain `kelir run`
and the TUI from a console never flash. Note: with a `bash -c` root (the `detached` rows) bash itself gets a
console that kelir inherits, which is why plain calls are clean there; `direct` is the real console-less case.

### Test suites (step 4)

- `cargo test --test e2e_m4` at HEAD (worktree, run from a Herdr pane, spy on): 8 passed, 4.5 s.
  **FLASH: 4 visible-window events = 2 distinct windows**, both `Git\bin\bash.exe -c ...` children of a
  `bg-shim` (`sleep 40 # kelir-m4-survive-...` and `sleep 2; echo job-output`), plus one Windows Terminal
  window show; 4 conhosts. The test suite is a flash source, through the same shim path.
- Current tree `e2e_m4` + `e2e_verbs` (10 + 23 tests, all pass, spy on): **0 visible windows**, 9 conhosts
  (windowless), 273 descendant processes.

Spy logs: `~/kelir-scratch/flash/e2e_m4.spy.log`, `e2e_m4_wipnow.spy.log`.

### Baseline (nothing of ours running, 60 s, twice)

Not kelir: one visible Windows Terminal "Ubuntu" window (`ubuntu.exe` under WindowsTerminal, started by
`svchost`; 10:05:08), WSL `wslhost.exe` consoles (hidden), and opencode's constant `git.exe` helper
processes with hidden conhosts (`opencode` -> `git.EXE` ... 20 per burst). Hidden consoles are not flashes.
The first baseline (09:59-10:00) had zero visible windows.

### Leftovers

After every scenario the runner lists processes still alive that carry the scenario's tag or bin dir, kills
them by exact PID, and reports them: **none** for every run listed above. `kill` (taskkill /F on the session
without /T mid-call) left no `sleep 61/62/63` behind on any binary. `cancel` leaves nothing after
`kelir bg cancel` on all three binaries. At the end `tasklist` showed no `kelir.exe`, `winspy.exe`,
`canned_server.py`, `sleep.exe` or `flash-*` Herdr panes from this run.

## How to run the spy

```
powershell -File scripts/live/winspy-build.ps1          # builds ~/kelir-scratch/flash/winspy.exe (needs only the .NET Framework csc)
winspy.exe <log> [max-seconds=120]                      # touch <log>.stop to end it early; SUMMARY + VERDICT at the end
python -I scripts/live/winspy_report.py <log> <tag>     # <tag> = a string in the scenario root's command line
```

`winspy.exe` combines a WinEvent hook (EVENT_OBJECT_CREATE/SHOW/DESTROY, out of context) with a ~5 ms
`EnumWindows` poll, and a ~5 ms Toolhelp32 process snapshot (command line read from the PEB, parent chain
resolved). Flash = a visible window of class `ConsoleWindowClass`, `PseudoConsoleWindow` or
`CASCADIA_HOSTING_WINDOW_CLASS`, or whose title mentions bash.exe/kelir/cmd.exe/pwsh. Positive control (earlier tester, `control.spy.log`): a `cmd` started via PowerShell from a tool shell shows a Windows Terminal window, and the spy logs it as FLASH.

One scenario: `bash scripts/live/flash_run.sh <head|wip|wipnow> <calls|bg|cancel|kill|mix|direct|tui> <canned|live> [pane|detached|hidden]`
(binaries to test are copied to `~/kelir-scratch/flash/bin-<name>/kelir.exe`; live mode sources
`~/kelir-scratch/live.env` and forces `--provider openrouter`). Whole matrix: `flash_matrix.sh`,
`flash_matrix2.sh`. Each scenario runs in a Herdr pane via `hc run --bg --name flash-<x>` with the spy up;
`launch_detached.py` provides the console-less / hidden-console parents.

## Limits and notes

- Window/process attribution is by parent chain from a tagged root. Windows Terminal delegation windows
  come from `OpenConsole -Embedding` under `svchost`, so they appear "unattributed"; they are tied to the
  scenario by timing (see `direct`/WIP). The total counts in the raw reports include other agents' activity
  on this busy box (opencode git helpers, another agent's cargo tests); only "attributed" rows are
  used above, plus the timing check for `direct`.
- Run on Windows Terminal as default terminal app. With classic conhost the same console allocations would
  show as `ConsoleWindowClass` windows instead.
- Fixed in the tooling during this run (the previous tester's scripts): the leftover cleanup matched any
  process whose command line merely *quoted* `canned_server.py` and killed the tester's own `claude.exe`
  (the likely cause of the earlier session's death) - it now uses a process-name allow-list and skips
  ancestors; the `bg` scenario's canned JSON had unescaped quotes (server never started); `PATH` had an
  unconverted `C:/...` entry so the model's `kelir bg run` ran the installed `~/.cargo/bin/kelir.exe`
  instead of the binary under test; live mode did not force `--provider openrouter` (default provider
  `opencode-go` rejects the model with HTTP 400); the `cancel` scenario's `bg ls` ran outside the session
  and found no jobs (it now sets `KELIR_SESSION_DIR`).
