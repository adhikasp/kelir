# M1 review

Reviewed `b49d244..bef6568` (implementer commit `bef6568`; `cdde926` carries only docs) against
`docs/SPEC.md` and `docs/tasks/M1.md`. Host: Windows 11, Git for Windows bash. Live model:
`deepseek/deepseek-v4.1-flash-1017` on OpenRouter (swap target `deepseek/deepseek-v4-flash-0731`).

## Verdict: PASS WITH FIXES

Every M1 live scenario passes on the release binary: `stop --call` mid-`sleep` with the turn
continuing, `say --now` obeyed with no orphan (145 ms), `model` swap at the next step, `later`,
`tools --disable`, `end`, a dead session and the Windows named pipe from a second shell. The
verbs, boundaries and N2 (suspended child) are sound. Adversarial testing found **one real bug
with two triggers (F1, F2)**: `stop --call` cuts off a sibling call that is still running.

**0 blockers, 2 should-fix, 6 nits.** `cargo test` is red on purpose until F1/F2 are fixed
(`tests/review_m1.rs`: 2 of 5 fail).

## Gates (run independently)

| gate | result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo test --lib` | 137 passed |
| `cargo test --test e2e` | 16 passed |
| `cargo test --test e2e_verbs` | 18 passed |
| `cargo test --test review_m0` | 8 passed |
| `cargo test --test review_m1` (reviewer-added) | 3 passed, **2 failed** (F1, F2) |
| release binary | 5.15 MB (limit 20 MB) |

## Findings

| # | sev | where | finding | failing scenario |
|---|---|---|---|---|
| F1 | should-fix | `src/session.rs:966`, `1000-1015` | The first `stop --call` arms a 1.5 s `deadline` for the whole batch. When it expires the loop breaks and every task still unsettled is `abort()`ed and recorded as `error: tool task for call N did not settle (panicked or cancelled)`. A sibling that legitimately runs longer than 1.5 s is destroyed; the spec says "the turn continues" and the other call finishes. The e2e test only passes because its sibling is `sleep 1`. The aborted tokio task also drops the `Child` without kill-on-drop, so the sibling's process is most likely left running until the session ends (inferred from the code, not measured). | test `stop_call_leaves_a_long_running_sibling_alone`: calls `sleep 30` and `sleep 5; printf 'b done'`, `stop --call` the first; `call_b` settles `error` after 1.55 s |
| F2 | should-fix | `src/session.rs:804`, `448` | `live_calls` holds every id of the batch until the whole batch ends, so `stop --call <already-settled call>` is accepted (`ok:true`) instead of refused, and arms the same deadline as F1: a different, still-running call is aborted. | test `stop_call_on_an_already_settled_call_does_not_harm_the_others` (`printf fast` + `sleep 5`; `stop --call` the fast one after 0.8 s; `call_slow` becomes `error`) |
| N1 | nit | `src/session.rs:437-485` | The reply is sent before the `verb` event is fsynced (`self.sync()` runs after `reply.send`). A crash right after the ack can lose a verb the client believes accepted. | crash between ack and sync |
| N2 | nit | `src/tools/mod.rs:79-81` | A `stopped` result is only `stopped by user: <reason>`: it names neither the partial output nor `out_path` (the file exists on disk and `tool_settled.out_path` is empty). Spec says "keep partial work"; the model cannot find what the stopped command already printed. | `sleep 1; echo x; sleep 30`, `stop --call` |
| N3 | nit | `src/ctl.rs:39`, `src/paths.rs` | `ctl latest <verb>` tries sessions newest first and sends to the first that answers. If the newest session is dead, `latest say/end` lands on an older live session. Reasonable, but `latest end` can kill a session the user did not mean. Document it, or restrict `latest` to the newest. | newest killed, older `--keep` session alive, `kelir ctl latest end` |
| N4 | nit | `src/ipc.rs:159-170` | No explicit permissions on the socket or the state dir. On Unix, `say` is a prompt-injection channel into a fully autonomous agent, so a umask like `002` lets group members steer it. Create the socket `0600`. (Windows default pipe DACL denies other users write; not tested.) | Unix, umask 002 |
| N5 | nit | `src/session.rs:861-873` | During the provider retry backoff (`sleep(backoff)`), the loop does not read the inbox: a `stop` waits out the backoff and a `ctl` can time out. Read code only. | provider 500, `ctl stop` during backoff |
| N6 | nit | `src/session.rs:508-518`, `src/session.rs:861` | A `model` swap to an id the provider rejects (4xx, non-retryable) ends the turn as `error`, and in a `--keep` session `run` then breaks out and ends the session. `verb` already returned `ok:true`. One mistyped `ctl model` ends a long-lived session. Read code only. | `kelir ctl s model typo/none` on `--keep` |
| N7 | nit | `src/proc.rs:77-79` | On Windows the return values of `OpenProcess`/`AssignProcessToJobObject` are ignored. If the assignment fails the child runs outside the Job Object with no error and `stop` cannot kill its tree. | assignment denied (nested job policy) |

What I checked and found **correct**: the `verb` event is logged (applied or rejected, `boundary`
and `error` fields, documented in DEVIATIONS) before it is applied; boundaries are only
`before_step`/`after_tools`/`after_step`/`turn_end` and no `user` event ever lands between a
`tool_started` and its `tool_settled` (e2e asserts roles `system,user,assistant,tool,user`); the
`tools` array is byte-identical after `tools --disable` (e2e) and a disabled call returns
`disabled by user` live; `model` swap shows in the next `step_started` and the request `model`;
`stop` and `say --now` during the stream behave (my R3, R5 pass: well-formed next request, no
dangling assistant message); a malformed socket line gets an error line, the same connection
keeps working, a half line plus hang-up does not disturb the session (R4); `ctl` validates locally
(exit 2) and reports a dead or unknown session in ~60 ms with a clear message; two `later`
messages sent in one turn are delivered together at its end as two `user` events with
`delivery: later` and one new turn; `end` logs `turn_ended`/`session_ended` and exits 0. No
secrets or personal paths in the new files. New deps (`interprocess`, `libc`) are spec-named or
justified in `docs/DEPS.md`.

## Test pyramid

- **Unit (137, +27):** parsing and validation, scheduler order, `later` queue, session
  resolution, `ToolOutcome` texts. Good. They mirror the implementation's choices but the
  scheduler is genuinely pure.
- **E2E `e2e_verbs` (18):** covers every "how you know it works" item in the task, including two
  concurrent clients and a dead session. Gaps: `stop --call` only with a short sibling (hid F1);
  no test of a `stop --call` for an id that settled (F2); nothing sends `stop` during a provider
  backoff.
- **Added `tests/review_m1.rs` (5):** long sibling (F1, fails), settled-call stop (F2, fails),
  `stop` during the stream, raw malformed socket lines, `say --now` during the stream (all pass).

## Live drive log

Panes: `hc run --bg --name live-m1-sN --cwd <scratch> bash -c 'set -a; . live.env; set +a;
scripts/live/m1_pane.sh <scratch> "<task>"'`; the key never printed. The `kelir ctl` calls ran
from the reviewer's own shell (the "second shell" the scenario asks for), so the Windows named
pipe was reached across processes. State dirs are under `kelir-scratch/M1/sN/state`.

**S1: `stop --call` mid-`sleep 120` (pane s1): PASS.** `tool_started` seq 6, `ctl latest stop
--call chatcmpl-tool-9126… --reason "wrong approach, skip the sleep"` answered
`{"ok":true,"applies":"now"}`, `tool_settled` seq 8 `outcome: stopped`, result `stopped by user:
wrong approach, skip the sleep`; step 2 started, the model said the command was stopped and
offered to proceed; `turn_ended done`.

**S2: `say --now` during `sleep 120 ; echo done` (pane s2): PASS.** `verb` seq 7 (`boundary: now`),
`tool_settled` seq 8 `stopped` / `stopped by user`, then `user` seq 9 `delivery: now` "change of
plan: just print hello", the model ran `echo hello`. Verb to settled: **145 ms**. `sleep.exe`
count 1 before, **0 after** (no orphan).

**S3: model swap (pane s3): PASS.** Sent `ctl model deepseek/deepseek-v4-flash-0731` during step 3;
`verb` seq 17 `boundary: next_step`; `step_started` step 4 shows the new model; the request was
rebuilt on the new model:

| step | model | prompt | cached |
|---|---|---|---|
| 1 | flash-1017 | 665 | 0 |
| 2 | flash-1017 | 1154 | 640 |
| 3 | flash-1017 | 1643 | 0 |
| 4 | flash-0731 (after swap) | 2167 | **0** (cache restart, expected) |
| 5 | flash-0731 | 2662 | **2048** |
| 6-9 | flash-0731 | 3157-6580 | 2560, 3072, 5376, 6144 |

Cache resumed after exactly one step. Step 3's 0 on the old model is the provider's routing
(seen in M0), not a request change. The `ok` reply's `state.model` still names the old model
(true, it applies next step).

**S4: dead session and Ctrl-C (pane s4): PASS.** `hc interrupt` (Ctrl-C) into the pane while a
`sleep 77` ran: kelir exited, `sleep.exe` count 1 -> 0 (the Job Object took the tree, no orphan),
the log ends at `tool_started` with no `turn_ended`/`session_ended` (expected until M3 `resume`;
this is the first Ctrl-C test, so the M0 gap "Ctrl-C untested" is closed for the unsettled case).
`kelir ctl latest say hello` against it: `kelir: session ses_01m7… is not running (The system
cannot find the file specified. (os error 2))`, exit 1 in 63 ms, no hang. Unknown prefix: `no
session matches`, exit 1; empty state dir: `no sessions yet`, exit 1; unknown verb and `tools
--disable spawn`: exit 2, nothing logged.

**S5: `later`, `tools --disable`, `end` (pane s5): PASS.** During a `sleep 6; echo first` call I
sent `later`, `tools --disable bash`, `later`; the replies said `applies: turn_end`, `next_step`,
`turn_end`. After `turn_ended done`, both later messages were logged (seq 15, 16,
`delivery: later`) and a new turn ran; the model's two `bash` calls settled `disabled` (`disabled
by user: the \`bash\` tool is off for this session (…--enable bash…)`) and it told the user so.
`ctl latest end`: `session_ended reason: end`, pane exit 0.

**Not live-verified:** Go provider (`KELIR_GO_API_KEY` unavailable, as in M0), the `bg`/`spawn`
interplay (later milestones), Linux/macOS (`killpg` and the Unix socket compile via `cfg` only; CI
has not run), the F1 orphan (inferred), N4-N7 (read only).

## Spec items not yet covered

- `stop --call` leaving siblings alone (F1, F2).
- "`stop` ... `[Skipped]` results for unrun calls": covered by the `skipped` outcome text, but an
  e2e that stops a batch with a call that never started is thin (the gate race is narrow).
- Unix socket permissions (N4), the Unix path generally (no CI run).
- `tools` for MCP servers (no MCP yet), `resume` of a session whose log ends mid-turn (M3).

## Fix list for the implementer (in order)

1. F1 + F2: make the `stop --call` deadline apply only to the killed call (wait for that call's
   slot, not "all slots"), never abort a call that was not forced, and drop settled ids from
   `live_calls` so `stop --call <settled>` is refused. Consider `kill_on_drop(true)` on the
   `Child`. `review_m1` goes green.
2. N2 (point the stopped result at `out_path`), N6, N1 as time allows; N4 before a Unix release.
