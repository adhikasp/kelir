# M8 review: gates (six events), builtin skills `delegation` and `herdr`

Reviewer: fresh QA agent. Diff `3a6ae83..01aab61` (implementer's HEAD), worktree `job/m8`.
Review commits: `tests/review_m8.rs`, `tests/support/rev_gate.py`, `scripts/live/m8-gates.sh`.

## Verdict: **PASS WITH FIXES** (0 blockers, 7 should-fix, 5 nits)

The gate runner holds up under the attack list. It cannot loop and it leaks nothing it controls,
except two Unix gaps (F2, F3). Three things stand between this and "done":
- `.cmd` gates are broken under the default Windows shell (F1).
- The two new builtin skills are unreachable by the model without the hint (F5), so the herdr
  acceptance row is not met by default.
- The delegation skill documents commands that do not exist yet (F7).

## Test counts

| run | result |
|---|---|
| Windows `bash scripts/gate.sh` at 01aab61 | fmt ok, clippy ok, nextest **1211 passed, 3 skipped**, doctest ok, `gate: PASS` |
| Linux `scripts/remote-check.sh the maintainer kelir-verify-m8-rev` at 01aab61 | fmt, clippy ok, **1213 passed, 3 skipped**, doctest ok, rc=0 |
| Reviewer file `tests/review_m8.rs`, Windows | 15 passed, 2 ignored (F1, F2) |
| Reviewer file, Linux (HEAD ec45b3e) | 13 passed, 2 skipped (F2, F3 `ignore` on unix); with `--run-ignored all`, F2 and F3 **fail** on Linux (proven) |
| Binary size | release `kelir.exe` 9.4 MB (limit 20 MB) |

## Findings

### F1 (should-fix, Windows): `.cmd`/`.bat` gates do not run under the default shell
- Where: `src/gates.rs:515` (`"cmd" | "bat" => format!("cmd /C {quoted}")`).
- Weakness: the command runs through the configured shell, which is Git Bash on Windows. MSYS rewrites the `/C` argument to `C:/`. `cmd` then starts its interactive banner instead of the script.
- Impact: a skill-bound `scripts/turn_end.cmd` fails open on every run with `error: not JSON … Microsoft Windows [Version …`. The brief requires `.cmd` and `.ps1` gates through the configured shell. `.ps1` works.
- Test: `a_cmd_gate_runs_from_a_console_less_parent` (`#[ignore = "F1"]`, fails). `a_ps1_gate_runs_from_a_console_less_parent` passes.
- Fix direction: pick the form by the configured shell. Use `cmd //C` or `MSYS2_ARG_CONV_EXCL=*` for bash, or run the `.cmd` directly. Add a unit test that runs the produced command line.

### F2 (should-fix, Unix): a gate can escape the kill by detaching (`setsid`)
- Where: `src/gates.rs:873` (`ProcessGroup::new`, `killpg`). The kill is group-based, so a `setsid` child leaves the group.
- Impact: a gate (or anything it runs, such as `nohup setsid …`) outlives its run, the 30 s timeout and the session. The doc claim "no gate leaves a process behind" holds on Windows (Job Object) but not on Unix.
- Test: `a_gate_that_daemonizes_does_not_outlive_the_run`, run with `--ignored` on the maintainer. It fails: the grandchild's flag appears after 5 s.
- Fix direction: on Linux use a subreaper (`PR_SET_CHILD_SUBREAPER`) and kill descendants by walking `/proc`, or a cgroup/pidfd scan. At minimum state the limit in `gates.md` and `DEVIATIONS`.

### F3 (should-fix, Unix): SIGKILL of kelir mid-gate orphans the gate tree
- Where: `src/gates.rs:873-875`. There is no `PR_SET_PDEATHSIG` or watchdog.
- Impact: on Windows the kill-on-close Job Object takes the tree down (test passes). On Linux a killed or crashed session leaves a running gate for up to its natural lifetime.
- Test: `killing_the_session_mid_gate_kills_the_gate_tree` (`#[cfg_attr(unix, ignore = "F3")]`). It passes on Windows and fails on the maintainer with `--run-ignored`.
- Fix direction: set `PR_SET_PDEATHSIG(SIGKILL)` in a `pre_exec` of the gate shell. A group-leader shim like the bash tool's would also work.

### F4 (should-fix): `git_sync.py` stages everything, including secrets, and has no dry run
- Where: `examples/gates/git_sync.py:37` (`git add -A`).
- Reproduced in a scratch repo with a bare remote: a `.env` file in the cwd was committed and **pushed** at `session_end`.
- What holds: it never forces, and a diverged `pull --ff-only` only prints to stderr.
- Still wrong: the brief and the review ask for "opt-in, never force, dry run". No dry-run mode exists. It stages untracked files, and `cwd` need not be the repo root.
- Live: against a local bare remote it pulled at start and committed and pushed `hello.txt` at end (`kelir: <session>`). It worked, but with the staging behaviour above.
- Fix direction: `git add -u` by default, with an opt-in `GIT_SYNC_ADD_NEW=1`. Add `GIT_SYNC_DRY_RUN=1` that logs what it would do. Refuse to push when `.env`-like names are staged.

### F5 (should-fix): the builtin `delegation`/`herdr` skills cannot be reached by the model
- Where: `src/prompt.md:9`, `src/sources.rs:288-309`, `docs/reference/sources-and-skills.md:54-79`. The prompt says "Skills are pull … Directories: …". A builtin has no path, and `kelir docs` has no topic for it (`kelir docs herdr` gives "no topic").
- Live evidence (`scripts/live/m8-gates.sh herdr`, `herdr-out`):
  - Default config, inside a pane: the model never found the skill. It ran 16 tool calls, `herdr`/`herdr-app` CLIs and registry spelunking, and answered "5 agents".
  - Outside Herdr: the model said "zero, no Herdr server running" instead of "I am not in Herdr".
  - With `[hint] model=…` (`herdr-hint`): the one-line hint (`might be relevant: herdr - … hc ls --json …`) steered it to `hc ls --json` and `hc machine ls` in 12 calls. It used the description, not the body, because the hint never carries the body.
- Impact: the SPEC M8 acceptance (inside a pane the model answers from `hc ls --json`, outside it says it is not in Herdr) is not met by default. The skill body, including the "never kill another agent's pane" rule, is dead text in every configuration.
- Fix direction:
  - Advertise builtin names and descriptions in the prompt.
  - Give the model a way to read the body, e.g. `kelir docs --skill herdr`.
  - Or push the body as a `source` when the hint selects a builtin.
  - The pre-existing `kelir` skill has the same gap.

### F6 (should-fix, security; the author declared it open): project-layer gates run without a trust prompt
- Where: `docs/DEVIATIONS.md` ("Project-layer gates start without a trust prompt"), `src/gates.rs:407` (`from_skills`), `src/config.rs`.
- Impact: cloning a repo with `.kelir/config.toml [gates]` or `.kelir/skills/*` using `kelir-gate` executes its commands at `session_start`, before any prompt. The gate env is scrubbed, but the command is arbitrary code as the user.
- Fix direction: route project gates through `src/trust.rs`, as STEER-7 intends. Until then, say it in `gates.md`'s first screen.

### F7 (should-fix): `delegation` documents commands that do not exist in this tree (M7 not merged)
- Where: `src/docs/skill-delegation.md:3` (description) and the body; `docs/DEVIATIONS.md` ("`delegation` describes M7's CLI from the spec").
- Checked on the built binary: `kelir run --child` gives "unexpected argument '--child'". `kelir brief` and `kelir ls` give "unrecognized subcommand". The `child_started`/`child_settled` events and the depth/10/16 caps are not implemented.
- Impact:
  - Description wording: the skill's description is the part the hint pushes, and it names `kelir run --child` and `kelir brief`.
  - When it breaks: any session that follows it hits an argument error. If M8 merges before M7, advertised behaviour is false.
  - What holds: the CLI spellings and caps match SPEC lines 397-406. Nothing is wrong beyond the missing implementation.
- Fix direction: gate the skill on M7 landing (merge order), or mark the body "requires M7" and leave it out of `BUILTIN_SKILLS` until then.

### Nits
- N1 `src/docs/skill-herdr.md:54`: `hc wait <t> --children` omits the value. `hc wait --help` shows `--children <all|any>` (children of `<t>`).
- N2 The nested-kelir guard is env-only (`KELIR_GATE_EVENT`, `src/gates.rs:87`). A gate that clears the variable, or crosses `ssh`/`docker`, can start gated sessions again. The 8-cap is per process, so it does not bound that. It needs a deliberate gate author or an untrusted project config, which is F6.
- N3 A gate that runs `kelir ctl <session> say …` against its own session stalls the full gate timeout (30 s by default) per run. The session reads verbs only after the gate ends. The message is then delivered and gated as a human prompt. It is bounded, not looping, but it is a trap worth one sentence in `gates.md`. Test: `a_gate_that_sends_a_verb_to_its_own_session_cannot_deadlock_it`.
- N4 The `gate` event has no `tool` field (declared in DEVIATIONS). For `before_tool` fan-out you must join on neighbouring `tool_started`.
- N5 Gate cost is about 70 ms per run on Windows (Git Bash start). 1000 gated calls cost about 75 s of wall time, and the harness's own 60 s cap in `Sandbox::kelir` stops that run at about 800. See the 1000-call result below.

## Gate-runner hammering (all with a process-count watchdog; nothing left behind)

Baseline `sh`/`bash`/`kelir`/`python` counts were polled around every run. The machine is shared (others' panes), and the count never grew by 20 in seconds from my runs. After the suites, `Get-CimInstance` showed no process whose path was under `kelir-wt\m8\target`.

| case | test (in `tests/review_m8.rs`) | result |
|---|---|---|
| command is a bare data file, a directory, a nonexistent file, a `Cargo.toml` copy | `degenerate_gate_commands_fail_open_once_each` | each run is one `gate` event with `error`, call still runs, <30 s |
| empty / blank command (table, string, list forms) | `an_empty_or_blank_gate_command_is_rejected_at_load` | config error, exit != 0, model never reached |
| prints 100 MB | `hostile_output_fails_open_and_stays_bounded` (`flood`) | `error: output over …`, log < 200 KB, call runs |
| binary / invalid UTF-8; JSON nested 200k deep | same (`binary`, `deep`) | fail open, no crash |
| 900 KB reason | `a_huge_reason_is_cut_in_the_log_and_in_what_the_model_reads` | reason cut to ≤2000 chars in log and in `denied by gate rev: R…` |
| hang past timeout, grandchild ignoring SIGTERM | `a_timed_out_gate_that_ignores_sigterm_leaves_nothing` (timeout 2 s) | `timed out`, flag never written, passes on Windows and the maintainer |
| `setsid` daemon | `a_gate_that_daemonizes_…` (ignored) | **F2**, fails on Linux |
| kill kelir mid-gate | `killing_the_session_mid_gate_…` | passes Windows; **F3** on Linux |
| gate never reads stdin | `a_gate_that_never_reads_stdin_does_not_wedge_the_session` | passes |
| gate spawns itself (2 children × 3 levels) | `a_self_spawning_gate_is_bounded_and_gone_after_the_run` | ≤15 processes started, one `gate` event, none left |
| gate re-enters kelir (`kelir run`) | author's `a_gate_that_starts_kelir_does_not_start_gates_again` (reviewed) | holds; see N2 for the bypass |
| gate does `kelir ctl say` to its own session | `a_gate_that_sends_a_verb_…` | no deadlock beyond the timeout; N3 |
| N tool calls in one stream, each vetoed by a trivial gate | `a_thousand_gated_tool_calls_in_one_stream` (`REVIEW_N`) | 700 calls: 700 `gate` events, all `denied by gate veto: no`, no bash ran, 54.9 s (≈75 ms/gate). 1000 exceeds the harness's 60 s wait, so default N is 150. Gates run one at a time; the cap of 8 is never approached on this path |
| `session_end` on clean `end` / on hard kill | `session_end_fires_on_end_but_not_on_a_hard_kill` | fires on end; no gate after a kill (expected) |
| turn_end block once per turn, not once per session | `turn_end_blocks_once_per_turn_not_once_per_session` | two turns, each blocked once: 4 model requests |
| env scrub canary | `the_gate_env_holds_no_secret_even_with_custom_key_env` | `OPENROUTER_API_KEY`/`ANTHROPIC_API_KEY`/`MY_CUSTOM_TOKEN`/`AWS_SECRET_…`/`GITHUB_TOKEN` absent from gate env; no key/token/secret variable at all; canary absent from the log, stdout and stderr; no `KELIR_TOKEN` or `KELIR_SOCK` |
| config / inline / skill declaration | `the_three_declarations_are_equivalent` | same decision and same denial text `denied by gate <name>: nope` |
| `.ps1` gate from a `DETACHED_PROCESS` parent | `a_ps1_gate_runs_from_a_console_less_parent` | passes, no error |
| `.cmd` gate | `a_cmd_gate_runs_…` | **F1** |

Not independently exercised (author's tests reviewed, not re-run adversarially): the 8-live cap through a real multi-call MCP session (unit-tested at `Runner` level: 24 runs never exceed 8), MCP `mcp:<server>.<tool>` gating without recursion, rewrite for each of the four events (`e2e_m8` has them), and a visible-window check beyond the `CREATE_NO_WINDOW` code path and the DETACHED tests.

## Live drive (Herdr panes, `deepseek/deepseek-v4.1-flash-1017` via OpenRouter, release binary)

Script `scripts/live/m8-gates.sh <scenario> <scratch>`, run as `hc run --bg --name live-m8-<s> … bash -c 'set -a; . live.env; set +a; …'`. The key was never printed.

| scenario | what happened | verdict |
|---|---|---|
| `hedge` (verify_before_claim at turn_end) | Model answered "Probably … 5432". Gate `deny` (seq 6). Model got `[gate verify_before_claim] Your answer was not accepted: …`, ran 3 checks, answered. The second `turn_end` was `allow`. | PASS (once-only block, turn continued). Note: the model then wrote a fabricated "Verified: local config at C:/Program Files/PostgreSQL/16/…" line. Model behaviour, not a gate bug. |
| `deny-note` (before_tool no-rm + note_check on edit) | `edit` created the file; `note_check` rewrote the result with `[gate note_check] … checker: file still contains TODO`; `rm` gave `tool_settled outcome=denied "denied by gate no-rm: deleting files is not allowed in this session"`; a later benign bash was allowed. | PASS |
| `numbered` (numbered_answers; kept session + `ctl say "do 2,4"`) | The gate allowed the first prompt and rewrote the second into `do 2,4\n\nThe items I mean, from your last numbered list:\n2. say banana\n4. say date`. The model replied "banana / date". | PASS |
| `git` (git_sync start/end, local bare remote) | The pull ran at start (`git_sync-2`, allow). At end it committed and pushed `kelir: ses_…` containing `hello.txt`. | PASS, with F4's staging caveat |
| `herdr` / `herdr-out` / `herdr-hint` | See F5. | FAIL by default; works with hint via the description |

I did not run the headline 1000-call scenario through the release binary in a pane. The 700-call e2e covers it on the debug binary.

## Spec and brief coverage

| brief item | state |
|---|---|
| six events fire | yes (author's e2e plus my `each…`/probe use) |
| allow/deny/rewrite shapes | yes, per DEVIATIONS; live-checked deny, rewrite (after_tool, prompt_submit) |
| fail open, logged as `gate` events | yes |
| 30 s timeout, tree kill | yes on Windows; Unix F2/F3 |
| `turn_end` block ≤ once per turn | yes (my test: per turn, not per session) |
| deny text `denied by gate <name>: <reason>` | yes, live and e2e |
| declared three ways | yes, equivalent (my test) |
| scrubbed env, nothing secret logged | yes (canary) |
| `.cmd`/`.ps1` via configured shell, no window | `.ps1` yes; `.cmd` F1; windows only checked via the DETACHED path |
| `observe` lists gates | yes (author's e2e; `describe` never shows env values) |
| example gates generic | yes; `scrub-check.sh` flags nothing in `examples/gates`, the skills or `gates.md`. The tree as a whole is "not publishable" for pre-existing reasons. M8 adds hits in `docs/retros/m8-impl-notes.md` (a local path and `the maintainer`) and `docs/PROGRESS.md`; `src/docs/tests.rs:928` is a banned-words list. |
| builtin skills truthful and short | `herdr` truthful (checked against `hc ls --json`, `hc get`, `hc machine ls`, `--all`, `--label`; N1 aside); `delegation` aspirational (F7); both ~500 words; reachable only via description (F5) |
| docs / drift | `gates.md` plus the generated tables pass the drift test in the gate |

## Process hygiene

All reviewer tests run under the `Sandbox` Job Object guard. The self-spawning, SIGTERM-ignoring and daemon gates are depth- or time-capped in `rev_gate.py` so a missing guard shows up as a counter, not a fork bomb. The only processes started outside the harness were my own Herdr panes (all exited; I removed them with `hc rm`). Nothing was killed by image name.

## Verdict recap

Merge only after F1, F5 and F7 are resolved. F4 and F6 are declared, but F4 pushes secrets by default and F6 is code execution from a cloned repo. F2/F3 are Linux-only leak paths and should be fixed or documented. The runner itself is sound.
