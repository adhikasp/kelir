# M7 review: children as CLI, brief, caps, ls, kill

Reviewer: fresh QA agent. Range `52be955..891d1eb` (job/m7). Date 2026-11-21.

## Verdict: PASS WITH FIXES (0 blockers, 3 should-fix, 4 nits)

Every acceptance line of `docs/tasks/M7.md` holds on the real release binary on Windows and Linux. The
issues are in `kelir ls` and the brief's behaviour, not in the caps, the registry, the process tree or the
trust model. The delegation skill (M8) is not merged: nothing in M7 depends on it. Today a model learns
`kelir run --child` only from `kelir docs` or its prompt.

## Pyramid

| layer | result |
|---|---|
| `cargo fmt --check`, `clippy --all-targets -D warnings` | clean |
| Windows `bash scripts/gate.sh` | PASS: nextest 1203 passed, 3 skipped, 0 retried; doctest ok |
| Linux `scripts/remote-check.sh the maintainer kelir-verify-m7-rev` | PASS: fmt, clippy, nextest 1206 passed, 3 skipped, doctest |
| release binary | 9.7 MB on Windows (limit 20 MB) |
| my drive (`scripts/live/m7_drive.py`, scripted mock, real binary) | Windows: 12 scenarios, Linux: 12 scenarios; all checks pass except the findings below |
| real model (`scripts/live/m7_live.sh`, OpenRouter `deepseek/deepseek-v4.1-flash-1017`) | two-bg, depth3, brief: see log |

The e2e tests in `tests/e2e_m7.rs` are meaningful: they assert on the logs, the mock's request bodies and
process liveness, and I found nothing that merely mirrors the implementation. I added no Rust test: the
findings below are demonstrated by the drive scripts (scenario names given), and none is a blocker.

## Findings

### S1 (should-fix): `kelir ls` shows a brief as `session` with the parent's task
`src/childcli.rs:271-285` (`node_from`) takes the **first** `user` event. A brief's log is a copy of the
parent's, so the first user event is the parent's task (`from: cli`). Result: `d1 session ... ROOT go`
instead of `d1 brief ... <question>`, and `--json` has `"kind":"session"`. `docs/reference/children.md`
shows `d1 brief`. Seen in the mock (`m7_drive.py brief`, "ls brief nodes: [('session','done')]") and in the real
run (`live-brief`: all five briefs listed as `session`, task = the parent's long prompt).
Fix: if any `user` event has `from == "brief"`, kind is `brief` and the task is that (last) event's text.

### S2 (should-fix): the tree cost counts the brief's inherited usage twice
`node_from` tallies `usage` events from the brief's own log, which starts with the parent's copied events.
Real run: parent usage per step `(1326,0),(3267,1280),...`; the brief's log holds the same six entries
first, then its own two. `ls` printed the brief at `$0.0018 cache 80%` (parent `$0.0021`), so `tree cost`
and `tree_cost_usd` overstate the spend by the whole shared prefix, once per brief. The same copied usage
feeds `status` cost and the `warn_cost` check in the brief. Fix: count usage only from events after the
snapshot cut (the brief's first own `user event from brief`), or tag the copied events.

### S3 (should-fix): a brief can delegate, and a brief on a task that says "delegate" recurses
The brief keeps `bash`, and `kelir run --child` / `kelir brief` are allowed in it. In `live-brief` the
parent's task (copied into the snapshot) said "run kelir brief ...", so the brief obeyed that old
instruction: briefs of briefs reached depth 3, then the depth cap stopped it (5 briefs for one question,
about 7x the cost). The caps held, but a "read-only side question" should not be able to start children.
Fix: refuse `kelir run --child` / `kelir brief` when the session is a brief (`child_start` in
`src/session/children.rs`), with the same `refused:` text and exit 3; also consider a one-line note in the
appended question that it is a side question.

### Nits
- N1 `registry_up` (`src/session/children.rs:437`) can be called again for a live child and re-point the pid
  the later `kill` signals (the start-time check is also supplied by the caller). Needs the child's link, so
  not reachable from a tool call; make `up` once-only anyway.
- N2 `kelir ctl --help` verb list omits `brief` (`src/cli.rs`), though `ctl <s> brief` works.
- N3 `children.md` says a dead child is reaped "within a second". A child killed from outside is settled at
  once by its starter (`error`, or `killed` when signalled, ~1 s in `crash`); only a child nobody reports on
  waits the 5 s grace plus the 1 s tick. Say so.
- N4 My own tooling: the first draft of `m7_watchdog.ps1` killed by "new process of that name" and tripped on
  42 new `sh/bash/kelir` processes that were **not mine** (my run had about 6; another job was running tests).
  It killed them by exact pid, so it may have cut a sibling's run. I stopped using it; the driver's watchdog
  (in `m7_drive.py`) only kills processes whose ancestry reaches my own roots. The file is kept for the record
  and should not be reused.

## What I tried and could not break (Windows and Linux unless noted)

| area | scenario / evidence | result |
|---|---|---|
| depth | `chain`: root, L1, L2, L3 finish; L3's `kelir run --child` exits 3, message names `max_child_depth` | PASS |
| foreground child | the bash call's stdout is the child's final text, rc 0 | PASS; also real model `depth3` (BOTTOM through 3 levels) |
| 10 per agent | `percap`: starts 1-10 rc 0, the 11th rc 3 "already has 10 live children (limit 10"; `ctl status` shows children=10, `child_list` has no link/token; human `kelir kill` frees the slot | PASS |
| 16 per root | `caps16`: 3 mids x 6 sleepers: 13 admitted, 5 refused with the 16-descendant message, exactly 16 live | PASS (about 80 processes at peak on Windows) |
| crashed child frees its slot | `crash`: child host hard-killed, next start admitted about 1 s later, registry/log say `error` (Windows) or `killed` (Linux, signalled) | PASS |
| kill root, soft and hard | `rootkill`, `hardkill`: a child, a child with a grandchild, a signal-ignoring child (`trap '' TERM INT HUP`), plus a detached `kelir bg run`. After `ctl end` or `kill -9` / `taskkill /F` of the root only the bg job's tree remains; gone within 0.5-0.7 s on both OSes. Orphaned child sessions show `crashed` in `ls` | PASS |
| detached bg survives on purpose | same scenarios | PASS |
| `kill --tree`, scoping | `killtree`: root's `kill --tree` stops 5 descendants, all `killed`, their tool processes gone. A child cannot kill itself, its root or a sibling (exit 1, `refused`) | PASS |
| wake | `wake`: two background children wake the idle `--keep` root; one `[child ses_... done]` message each, `child_settled` once each, the model request after carries the text. Real model `two-bg`: "34, Paris" | PASS |
| odd input | `odd`: empty task, bad `--cwd`, task on stdin with quotes/`$HOME`/backticks, `kill` and `brief` without args, human kill of a foreground child (parent's bash gets rc 1), `ctl <root> stop` stops the foreground child process | PASS |
| steering a child | `steer`: `ctl <child> status/say/stop`, `observe` work unchanged | PASS |
| `ls --json` | `chain`: schema keys on every node, cost and cache ratio equal the logs' usage sums, `tree_cost_usd` is the sum, tree indent | PASS (except S1/S2 for briefs) |
| brief prefix | `brief` with a logging mock: the first brief request is the parent's previous request byte for byte up to the end of its messages array, everything after the array (model, session_id, stream options, tools) is byte-identical, exactly one extra user message, same 3-tool list, `session_id` is the parent's | PASS |
| brief cache, real model | `live-brief`: the brief's first request reports `cached_tokens 8320 of 8466 prompt` (98%); parent's own steps 7-8 were 8448/8749 and 8704/9074 | PASS |
| brief refusals / history | `edit` and `compact` return `refused: this is a brief...`; `bash` works; the parent's log gains exactly `child_started` + `child_settled`; the command's stdout is the answer | PASS |

## Trust model (`m7_trust.py`, real socket / pipe, plays a same-user tool child of session A)

- A's token against A: cross-tree `kill` of B's child (with a forged `human:true`) refused ("not a descendant");
  B's child stays alive. A's token on B's socket refused by the listener. No token: refused at the listener.
- Forged `admit`, `up` (with another process's pid), `settle` (sibling forgery), with no link or a guessed link:
  all `refused: this is not the link of a live child`. `settle-local`, `note-settled` (wake-text injection),
  `note-started` without the session's private key: `child: an internal operation`. Nothing forged reached A's log.
- A child's tool call has its **own** token (hash equals its token file, differs from the root's) and
  `KELIR_ROOT/PARENT/DEPTH`; no link or root-token variable. The provider key is not in a tool call's env.
- Canary provider key (`OPENROUTER_API_KEY=M7CANARY-...`) and every session token: absent from every process
  command line (child hosts' lines are unreadable on Windows anyway) and from every file under the state dir
  (events, `host.err`, blobs) except the token files themselves.
- Windows: a same-user `OpenProcess(VM_READ)` on a child host fails (error 203/denied), so its environment
  is not readable. Linux: `/proc/<pid>/mem` of the session processes is not readable (non-dumpable).
- Not defended (documented in `children.md`): a same-user process that reads another session's token **file**
  can drive that session like `kelir ctl`. Unchanged from STEER-7.
- Herdr status and OSC: root only (`prepare` skips the probe and `Herdr::detect` for a child); reviewed in
  code and in the e2e Herdr test, not driven in a real Herdr pane by me.
- Hint/memory/sources: a (non-brief) child renders its own system prompt from its cwd, not the parent's; a
  brief reuses the parent's frozen system text. Not tested beyond code reading.

## Spec items not covered

- Herdr status as seen in a real pane for a child (code and e2e only).
- A live-model kill of the root with real children (done with the mock on both OSes; the real-model `orphan`
  scenario in `m7_live.sh` was not run, to keep spend low).
- The Go provider path is not live-verified (no key), as the protocol says.
- No TUI children panel (documented: M8 / later).

## Scripts (all under `scripts/live/`)

`m7_drive.py` (scenarios: chain percap caps16 rootkill hardkill crash brief trust killtree wake odd steer; own
ancestry-based watchdog and leftover check), `m7_mock.py`, `m7_trust.py`, `m7_live.sh`, `m7_live_report.py`,
`m7_live_brief.py`, `m7_cleanup.py`, `m7_watchdog.ps1` (do not reuse, see N4).
Run: `python m7_drive.py <kelir> <scratch> <scenario...>` inside a pane; Linux: same, on the maintainer.
