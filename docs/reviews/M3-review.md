# M3 review

Reviewed `26d0477..36d38c9` (implementer commits `3252c6f..36d38c9`) against `docs/SPEC.md` and
`docs/tasks/M3.md`. Host: Windows 11, Git for Windows bash, `hc 0.1.21`, Herdr pane = ConPTY.
Live model: `deepseek/deepseek-v4.1-flash-1017` on OpenRouter. Linux gate:
`scripts/remote-check.sh the maintainer`.

## Verdict: FAIL (2 blockers, 4 should-fix, 5 nits)

Durability and resume are good and survived everything I threw at them: five real kills of a
live run (taskkill and the abort hook), a hand-torn log, a crash inside a resume, two simultaneous
resumes. No completed command was ever re-run, every unsettled call became exactly one `unknown`
result, and the task finished with a clean counter. **Compaction is broken in its default
configuration.** Every compaction e2e test sets `compact_keep = 0`, which hides it. With the
default `compact_keep = 4`, `fold` puts the summary and pins *after* the kept tail (B1), and a
second compaction close to the first keeps the old summary and re-pins the same messages (B2).
Live, that made the context grow with each compaction (`tokens_after > tokens_before` in 4 of 5
compactions) and the auto trigger fire on nearly every turn. `cargo test` is **red on purpose** until
these are fixed: `tests/review_m3.rs` has 6 failing tests, each proving one finding.

## Gates (run independently)

| gate | result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo test --lib` | 226 passed |
| `e2e` / `e2e_m3` / `e2e_tui` / `e2e_verbs` | 16 / 9 / 15 / 22 passed |
| `review_m0` / `review_m1` / `review_m2` | 8 / 5 / 2 passed |
| `review_m3` (reviewer-added, 11) | **5 pass, 6 fail** (R1, R3, R5, R6, R7, R8 = the findings below) |
| Linux (`remote-check.sh the maintainer`, HEAD `36d38c9` and again at `d79dd54`) | fmt, clippy clean; 228 unit, 16/10/15/22/8/5/2 passed (PTY tests too); `review_m3` fails the same 6 on Linux, passes the same 5 (incl. R9) |
| release binary | 5.77 MB (limit 20 MB) |
| idle RSS, TUI waiting for input | 13.3 MB working set, 3.4 MB private (limit 30 MB) |

## Findings

| # | sev | where | finding | failing scenario |
|---|---|---|---|---|
| B1 | **blocker** | `src/fold.rs` (`fold`, the `Compacted` arm) | A `compacted` event is rendered where it was *written*, which is after the `compact_keep` tail it kept. The request becomes `[system, tail..., summary, pins...]`: it can start with an `assistant` message right after the system message (providers that need a leading `user` reject it), the model's recent work sits above its own summary, and **the last message is an old pinned human message or the original task**, not the latest question. The tests hide it with `compact_keep = 0`. | `tests/review_m3.rs::r1_...` (mock, default keep): request after a compaction is `[system, <calls call_3>, step3, <calls call_c>, queued-result, SUMMARY, THE TASK]`. Live (`m3_compact.sh`, default keep): request after `/compact` was `system, assistant "Noted...", user "ZEBRA", assistant, user "Question: ...", [kelir compacted...] summary, user task, user "Human note one"`: the question in the middle, an old note last. The model answered correctly only because the question was still visible. Fix: render summary + pins at the position of `through_seq` (before the first kept event), not at the compaction's own seq. |
| B2 | **blocker** | `src/fold.rs` (`replaced`), `src/compact.rs` (`plan`, transcript), `src/session.rs` (`compact`) | A compaction is only superseded when a later one's `through_seq` is `>=` its own **seq**. With `compact_keep > 0` the later `through_seq` is usually below the earlier compaction's seq (it sits after its tail), so both stand: two summaries, and every pin rendered once per compaction. The second summarising request also does not include the first summary (its prefix stops before the first `compacted` event), so the older summary is kept only as a separate stale message. Compaction therefore *grows* the context, and the auto trigger fires again on the next turn: a death spiral of provider calls. | `r3_...`: two `compact` tool calls two steps apart, final request is `[system, SUMMARY-ONE, THE TASK, tail..., SUMMARY-TWO, THE TASK]`. Live (`m3_compact.sh "" "window = 1250"`, real model): 5 compactions in 8 steps, `tokens_before -> tokens_after` = 358->652, 426->937, 461->1384, 683->1097, 989->969; the last request held 3 summaries and 3 copies of the task and of both human notes. Fix: a compaction supersedes every earlier one whose `through_seq` it covers *or* that lies in its range; summarise from the fold of the range (earlier summary included), not from raw events. |
| S1 | should-fix | `src/resume.rs` (`interrupted`), DEVIATIONS M3 | `resume <id>` with no task is a no-op for a turn that ended `stopped` or `error`, because `turn_ended` + `session_ended` count as a clean end. SPEC verb table: "`resume` continue a stopped or crashed session"; M3.md: "A stopped or crashed turn continues from the log with the model's next step." A provider outage (turn `error`) is the commonest reason to resume, and it cannot be retried without inventing a task. The deviation is written down but it contradicts the milestone text. | `r7_...`: first run ends `error` (HTTP 500), `kelir resume <id>` exits 0 with zero provider requests. `r8_...`: `stop` mid-call, `end`, `resume <id>`: zero requests. |
| S2 | should-fix | `src/session.rs` (`maybe_compact` / `compact`) | While the summarising request is in flight no verbs are read, so `stop`, `end` and the TUI's double Ctrl-C hang until the provider answers (a real summary of a large context is tens of seconds). The ack is held too. | `r6_...`: summarising request held at a gate, `kelir ctl <id> stop` gets no answer in 3 s. |
| S3 | should-fix | `src/event.rs` (`EventLog::open`), `src/resume.rs` (`log_damage`) | A torn last line is left in the middle of the append-only log (the writer only adds a newline after it). The log now has an unparseable line and **two lines with the same seq**; `jq`, `kelir run --json | jq` readers and the TUI tools choke on it (I saw `jq: parse error` at the fragment), and every later `resume` reports the same fragment again as an `error` event. | `r5_...`: tear the last event, resume three times: 3 identical `error {where: log}` events. Live (`torn/`): `jq` fails at the fragment line. Fix: truncate the log to the last complete line (after copying the fragment to a sidecar if it matters), or report once. |
| S4 | should-fix | `src/session.rs` (`run`), `src/resume.rs` (`interrupted`) | A kill between two turns (`turn_ended` written, no `session_ended`) makes a headless `resume` call the model with a request that **ends on the assistant's final answer**. `e2e_m3::a_log_that_was_never_closed...` asserts exactly that request. Providers treat a trailing assistant message as prefill or reject it; either way it is a paid, meaningless turn. Not live-verified. | Kill a `--keep` session while idle; `kelir resume <id>`. |
| N1 | nit | `src/compact.rs` (`plan`) | Compaction is allowed when it cannot shrink anything: with a small context the summary + pins + kept tail is larger than the original (live: 478 -> 642 and 423 -> 712 tokens on a ~1 k-token session). Skip when the estimated `tokens_after >= tokens_before`, and say so. | `m3_compact.sh`, any short session. |
| N2 | nit | `src/session.rs` | An agent-called `compact` is only an in-memory request; a kill between the tool's "queued" result and the next boundary loses it and a resume does not re-queue it. | Kill after the `compact` tool settles. |
| N3 | nit | `src/ipc.rs` (`bind`), `src/session.rs` (`reopen`) | The "already running" check is connect-then-bind. Windows' first-pipe-instance rule closes the race (R9: always one winner, `Access is denied` for the other, 5/5). On Unix `bind` unlinks the socket path unconditionally ("the session dir is new" is no longer true with resume), so two simultaneous resumes can both pass. R9 passed once on the maintainer but that is not a proof. | `r9_...` on Linux, repeated. |
| N4 | nit | DEVIATIONS | The summarising request's usage and cost are not logged, so compaction spend is invisible to `kelir ls` (already documented as a known gap). | n/a |
| N5 | nit | `docs/PROGRESS.md` | The M3 row says "A log that was never closed ..." twice. | n/a |

Checked and **correct**: `EventLog::sync` is flush + `sync_all`, called after every batch, verb
ack and resume block; torn-line handling at every byte offset (unit) and live (a real session
torn mid-`tool_settled`, resumed with a real model: `error {where: log}`, `resumed`, one
`unknown` settle carrying the partial output path, the model read the output and continued); a
`tool_started` without `tool_settled` and an assistant call that never started are both settled
`unknown` exactly once, also across a crash *inside* a resume (R4); `step`/`turn`/`seq` continue
from the log (R4/R5 seq dense); a live session is refused with exit 1; the tool list is frozen
from `session_started`; the summarising request is sealed (no tools, same session id, focus
line); compaction waits for the boundary behind a running tool (e2e); a >10 KB human message
survives pinning verbatim through its blob (R10); a failed summary is one `error` event and
the turn goes on, without a summary request on every step (R11); auto trigger arithmetic is
`>=` window x 0.8 and fired live at exactly 1000/1250 tokens; the system prefix and the tool
list are byte-identical across a compaction; no secrets or personal data in the new code, tests
or docs; no new dependencies.

## Test pyramid

- **Unit (226, +31):** fold with `compacted` (single and superseded), torn line at every byte
  offset, unsettled pairing by occurrence, resume synthesis, 80 % arithmetic, sealed request.
  Good, except that the fold tests only use a compaction whose range ends right before it
  (empty tail), which is why B1/B2 passed.
- **E2E `e2e_m3` (9 on Windows, 10 on Linux):** five crash points with a side-effect counter, torn
  line, stale socket (Unix), live refusal, new task after clean end, unclosed log, human /
  agent / auto / racing-tool compaction. **Every compaction test uses `compact_keep = 0`**; the
  product default (4) is untested. Added: `review_m3.rs` R1/R3 (default keep).
- **Added `tests/review_m3.rs` (11):** R1 order, R2 no orphan tool message (passes), R3 two
  compactions, R4 crash inside a resume (passes), R5 torn line reported once, R6 stop during
  summarising, R7 errored turn resumes, R8 stopped turn resumes, R9 simultaneous resumes
  (passes), R10 spilled pin (passes), R11 failing summary (passes).

## Live drive log

Panes: `hc run --bg --name live-m3-... --cwd <scratch> "C:/Program Files/Git/bin/bash.exe" -c
'set -a; . <live.env>; set +a; bash scripts/live/m3_*.sh <scratch>/...'`; the key was never
printed. `m3_compact.sh` puts `scripts/live/logging_proxy.py` (bodies only, never headers) between
kelir and OpenRouter to read back the real requests.

- **L1 kill -9 at five points, real model, 14-step task with a counter file
  (`m3_kill.sh`, run `kill2`): PASS.** Kills and where the log stopped: K1 mid-tool
  (`tool_started` seq 11, `echo "step 2"` had already run), K2 right after a settle (last event
  `step_started` seq 24), K3 mid-stream (`step_started` seq 31, nothing committed), K4 between
  `usage` and `tool_started` (documented `KELIR_CRASH_AT=usage` abort, because a poll cannot hit a
  microsecond window; seq 35), K5 random (`tool_started` seq 46, `echo "step 5"` had run). Each
  resume appended `resumed`; three unknown settles (`unsettled` lists matched); no `turn_ended` or
  `session_ended` until the end; `step` kept counting (a killed step 5 resumed as step 6).
  `counter.txt` ended with exactly `step 1`..`step 14`, one line each. The model read the unknown
  results, ran `cat counter.txt`, skipped steps that were present and ran the missing one. No
  `tool_started` for any step ran twice (checked with `jq` over `tool_started.input.command`).
  Cache: the first request after a resume had `cached_tokens` 1024 of 1143, so the prefix survives;
  two mid-run steps showed 0 cached (provider-side; also seen in a session with no resume).
  The first attempt (`kill`) was discarded: the task was too short and K4/K5 fired after the
  session had finished.
- **L2 compaction with focus + the pinned task + a question (`m3_compact.sh`, default keep):
  functional PASS, ordering FAIL (B1).** Task with code word KUMQUAT-72, two human notes (9081,
  ZEBRA), `kelir ctl <id> compact "keep the file names and the code word"`, then "what was my
  original task's code word, the number from my first note, and the build target from my second
  note?" Log: `compacted {by: user, through_seq: 22, pinned_seqs: [2, 22], tokens_before: 478,
  tokens_after: 642, cache_restart: true}`. Answer: `KUMQUAT-72, 9081, ZEBRA.` The request that
  produced it is quoted in B1. **Cached tokens after the compaction:** 640 of 1205 (system + tools
  prefix), 640 of 1237, then 1152 of 1301 on the step after: `cached_tokens > 0`, and the
  rewritten tail restarts as the spec expects.
- **L3 auto trigger (`m3_compact.sh "" "window = 1250"`): trigger PASS, behaviour FAIL (B2).**
  First `compacted {by: auto}` at seq 23 after `usage.prompt_tokens = 1000` (the exact threshold);
  then it re-fired on four more turns and each compaction grew the context (numbers in B2).
- **L4 torn log, real model (`torn/`): PASS, with S3.** A copy of the finished session cut in
  half inside the last `tool_settled`: resume reported the torn line, settled the call `unknown`
  with `bytes 35, lines 5` and the partial-output path, the model `cat`ted that path and finished.
- **L5 TUI kill + `tui --resume` (`tui1`/`tui2`): PASS.** Killed during `sleep 40` (pane exited,
  no orphan `sleep`); `m2_pane.sh ... --resume <id>` showed "resumed from the event log ... continuing
  the turn that was cut off", the model checked `a.txt` and carried on. My `/compact` through
  `hc send` was wrapped by `hc` (`<hc from=...>` prefix) and took Git Bash path conversion, so it
  arrived as a message and the model called the `compact` tool itself (`by: agent`, 423 -> 712
  tokens, N1). The slash command is therefore **verified through `ctl compact` (L2), not typed
  into the TUI**.
- **L6 breakage attempts, mock (`review_m3.rs`):** kill inside a resume (R4, PASS), two simultaneous
  resumes (R9, PASS on Windows 5/5, once on Linux), resume of a live session (e2e + `already
  running`, PASS), stale socket (Unix e2e passes; a Windows pipe cannot go stale), compaction
  racing a running tool (e2e PASS), stop during summarising (R6, FAIL), spilled pin (R10, PASS),
  provider error during summary (R11, PASS).

**Not live-verified:** Go provider (`KELIR_GO_API_KEY` unavailable), macOS, a pane typed `/compact`
(see L5), a kill between two turns of a real model session (S4), a real power cut for the fsync
claim (only the process-kill ordering is proven).

## Spec items not yet covered

- "after a compaction the task and every human message are verbatim in the next request": true,
  but not in conversation order and not once (B1, B2).
- "`resume` ... continue a stopped or crashed session": stopped and errored turns do not continue
  (S1).
- Compaction should shrink the context toward below the trigger; with default settings it does
  not (B2, N1).

## Fix list for the implementer (in order)

1. B1: render summary + pins at `through_seq`, before the kept tail. Add an e2e with the default
   `compact_keep` that asserts the order (`tests/review_m3.rs` R1 is that test).
2. B2: supersession by range; include the earlier summary in the next summarising transcript; stop
   the auto trigger when the last compaction did not shrink the context (N1). R3, and the live
   `m3_compact.sh "" "window = 1250"`, must show `tokens_after < tokens_before`.
3. S1: `resume` with no task continues a `stopped` or `error` turn (or rewrite the task text and
   SPEC if that is wanted), R7/R8.
4. S2: read verbs during the summarising request (stop cancels it). R6.
5. S3: truncate the torn tail on open (or report once). R5.
6. S4, N2, N3, N5 as time allows.
