# steer7b review (STEER-7 re-review after steer7fix + merge of main) — verdict: FAIL (1 blocker, 3 should-fix)

Reviewer: fresh QA. Worktree `kelir-wt/steer7fix` (branch `job/steer7fix`), HEAD `06e97ac`, diff `07fd85e..HEAD`.
Windows 11 + Linux (the maintainer, work dir `kelir-verify-steer7b-rev`). The release binary was built from HEAD.

## Verdict

**FAIL.** The socket half of the fix works: every round-1 attack is refused, and the capability, id-registry and
refusal-budget paths do what the notes say. The blocker is the other way in. A tool child needs no socket, token or
capability to put forged `bg_started`/`bg_settled` events and a wake `user` message from `job` into the log. It only
writes a file that the session's watcher trusts. That is B1's impact again, for every id the registry holds. The SPEC and
DEVIATIONS text ("a file is never ownership evidence") is wrong about this.

## Results

| Check | Result |
|---|---|
| Windows gate (`scripts/gate.sh`) | clippy ok; fmt failed only on my new test file (rustfmt applied before commit); nextest 900 run, 897 passed (1 flaky), 3 failed = exactly my 3 new blocker tests; doctest ok. The flake is `review_m3 r8_resume_continues_a_stopped_turn` (STEER-9's). |
| Linux gate on HEAD (`remote-check.sh the maintainer kelir-verify-steer7b-rev`) | FMT/CLIPPY/NEXTEST/DOCTEST all OK: 899 passed, 3 skipped. A first attempt died on rustc being killed (no diagnostic, box at 8/12 GB): infra, and the rerun is green. |
| Round-1 attacks (`steer7_attack.py child`, live, real model, two sessions) | a1-a4 refused (capability missing / id not the session's); all 25 bg-spawn escapes refused; no marker file written; nul/con/trailing-dot ids refused. `b2` shows `ok:true`, but the id `13-2-1` is the attacker session's *own* call (call dir names repeat across sessions), so it is a script false positive. `b4` (same-user reads a sibling session's token file) is the documented S1 limit. |
| M4 bg, STEER-5 interrupts, TUI verbs | The full suites are green on both OSes (`e2e_m4`, `review_m4`, `e2e_steer5`, `e2e_tui*` among the 899/900). No regression. |
| Shim argv | Neither token nor capability is in the shim command line (Get-CimInstance on Windows; asserted in the test). Good. |
| Verbs added on main (view, status, interrupt, doctor) | They all go through `Trust::accepts` (token) and `check_job`. `view` is validated at the ack. No new state-changing verb skips the token. |

## Findings

### B1' (BLOCKER) — a status file written by a tool child is still evidence, for every registered id
`src/session.rs:849` (`watch_bg`) and `src/session.rs:1636-1642` (resume pass) feed every `bg/*.status` with `kind == job` or
`promoted == true` to `note_bg` (`src/session.rs:2583`). `note_bg` only asks `trust.owns(id)`. Every bash call's id
(`<seq>-<step>-<n>`, `src/session.rs:4264`) and every minted job id is in the registry, and the child can list the ids
(`ls $KELIR_SESSION_DIR/commands`, `bg ls`, the log). Nothing authenticates the file's contents.

The child writes `bg/<registered id>.status` and the watcher records `bg_started`, `bg_settled` and a
`user` message from `job` carrying the child's text. This works with no socket, token or capability, so the
round-1 fix is bypassed:
- for any earlier or concurrent call id (the call's shim has finished and never overwrites it);
- for a *running* job the session really minted: the child overwrites its status with `exited` and the model is told it finished.

It also reads files: `out` in the status is not path-checked on the watcher path (`check_job_path` runs only on the socket
route), so `wake_text` (`src/bg.rs:768`) embeds the tail of **any file** named in `out` into the log and the model's next
request. Live with the real model, `out` pointing at a file outside the session put its contents in the log.

Reproductions (all fail today):
- `tests/review_steer7b.rs::a_child_cannot_forge_a_status_file_for_a_registered_call_id` (run from inside a real bash call).
- `tests/review_steer7b.rs::a_child_cannot_settle_a_running_job_by_rewriting_its_status_file`.
- `scripts/live/steer7b_live.sh` (+ `steer7b_forge.sh`): real binary, real model (OpenRouter, deepseek flash), in a Herdr pane. Output:
  `bg_started`/`bg_settled` for `6-1-1` with cmd `FORGED-BY-CHILD (no token, no capability used)`, a
  `user` from `job` ending in `TOP-SECRET-OUTSIDE-THE-SESSION`, then `EXPLOITED`.

Fix direction: authenticate what the watcher folds in. Options: (a) the watcher records only a transition the session can verify
itself: a call's settlement from the call it supervises (it knows when its own shim exits), and a job's from a verified shim
(pid + `shim_started` against the live OS process, not only the file's say-so); (b) a MAC in the status file keyed by a
per-job secret the shim holds (but see S-A: that secret is in the shim's environment); (c) at least refuse any `out` outside
`bg/`/`commands/` and treat the file's `cmd` as untrusted text. (a)+(c) is the cheap, honest route. Correct the docs either way.

### S-A (should-fix) — the capability lives in the shim's environment, readable by a same-user child
`KELIR_JOB_CAP` is set in the shim's env (`src/bg.rs:516`). `tests/review_steer7b.rs::a_same_user_child_cannot_read_a_jobs_capability_and_forge_its_report`
reads it with an ordinary same-user process: `/proc/<pid>/environ` on Linux, and on Windows `OpenProcess(VM_READ)` + PEB
(`scripts/live/steer7b_envread.ps1`, about 20 lines, no privilege). With token + cap the forged `bg` report is **accepted**
(`ok:true`). The brief allows env, but SPEC calls this "in principle" while it is trivial on both OSes; say so plainly. The
effect of an accepted report is limited, because `note_bg` re-reads the real file. So the capability is no stronger than the
file write in B1'. Fixing B1' makes it meaningful; an inherited handle or pipe would be the real fix.

### S-B (should-fix) — SPEC/DEVIATIONS are inaccurate in three places
1. "A file is never ownership evidence" and "the model's own tools cannot impersonate a shim" are false (B1'). `docs/SPEC.md` (*Who may drive a
   session*) and `docs/DEVIATIONS.md` ~1340-1360 need the real boundary.
2. SPEC says a bash call's `bg/` id is `call_<32 hex>`. It is the deterministic `<seq>-<step>-<n>` (`run_tools`), so two sessions
   share those names: S2 holds only for explicit jobs. Either mint the ids or fix the text.
3. The log is trusted on resume: `events.jsonl` is writable by any same-user child, and a forged `bg_started`/`tool_started.dir`
   is adopted by the resume (`src/session.rs:1630`). Same trust class as S1; it needs one sentence of documentation.

### S-C (should-fix) — attack script vs. brief
The brief says the attack scripts "must all be refused": `steer7_attack.py child` still prints `b2 ... ok:true` (a false positive
from the shared call-dir names). Use a victim id the attacker does not also own, or check who owns it. Once B1' is fixed, the
`review_steer7b` tests belong in the expected-refused set.

### Nits
- N-a: `CLAIM_BUDGET` is shared by `bg-claim` and `bg-spawn`; a child can spend all 256 ids and block the model's own `bg run` (authenticated, bounded).
- N-b: the refusal budget (32) is global per session: a child can use it up so a later legitimate refusal is not logged. Key it per verb, or log one "N further refusals" summary.

## Attacks I tried that held

- Replay of a captured capability after the job ended: the registry keeps the job, `note_bg` dedupes on the log, and it re-reads the file, so no second event.
- Brute force / timing of id or capability: ids are 128-bit random; `same()` is constant-time over equal-length strings; `proves` needs the id first.
- Capability or token in argv / process listing: not present.
- Unauthenticated flood: the 1 MB line is cut at the 64 KB cap, the listener keeps answering, the log is unchanged; token-less requests never reach the log. An authenticated refusal flood stops at 32 events.
- Cancel of a promoted job by another session: refused at the listener (wrong token); naming it to one's own session is refused (not in the registry).
- A pipelined line after a refused one is not processed. Paths, junctions, UNC, other drives and device names are refused.
- Children sessions and the verbs on main: no new authentication gap found.

## Live-drive log

| Scenario | Command | Verdict |
|---|---|---|
| Forged status via real model | `hc run --bg --name live-steer7b ... steer7b_live.sh` (flash model) | EXPLOITED (B1'): the events and the arbitrary-file tail |
| Round-1 attacks, 2 real sessions | `steer7_setup.sh` in pane `live-s7setup` | all refused except the documented S1 and the `b2` false positive |
| Leftovers | sessions ended with `kelir ctl <id> end`; no `kelir.exe` of mine remains | ok |

## Files added
`tests/review_steer7b.rs` (3 failing tests that prove B1' and S-A), `scripts/live/steer7b_live.sh`, `steer7b_forge.sh`,
`steer7b_envread.ps1`. The Linux gate ran on HEAD before these existed; the `cfg(unix)` branch that reads `/proc/<pid>/environ`
was not run on Linux.
