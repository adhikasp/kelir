# M0 review

Reviewed `67edf42..HEAD` (implementer commits `f0e891b`, `7996806`) against `docs/SPEC.md` and
`docs/tasks/M0.md`. Host: Windows 11, Git for Windows bash. Model for live runs:
`deepseek/deepseek-v4.1-flash-1017` on OpenRouter.

## Verdict: PASS WITH FIXES

The loop, log, fold, cache prefix and both providers' request shapes are correct, and the live
drive passes every scenario in `docs/tasks/M0.md`, including `cached_tokens > 0` from step 2 on
OpenRouter. Adversarial testing found **5 real bugs and 1 spec gap, all should-fix** (F1-F6). None
break the happy path; F1-F3 matter before M1 adds `stop`, since they decide whether a call can
hang, balloon or write outside its directory.

**0 blockers, 6 should-fix, 7 nits.** The 4 failing tests in `tests/review_m0.rs` prove F1, F3, F4 and
F5 (F2 was measured by hand); `cargo test` stays red until they are fixed, on purpose.

## Gates (run independently)

| gate | result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo test --lib` | 92 passed |
| `cargo test --test e2e` | 13 passed |
| `cargo test --test review_m0` (reviewer-added) | 4 passed, **4 failed** (F1, F3, F4, F5 below) |
| release binary | 4.7 MB (limit 20 MB) |
| idle RSS (waiting on a 6 s tool call) | 7.6 MB working set (limit 30 MB) |

## Findings

| # | sev | where | finding | failing scenario |
|---|---|---|---|---|
| F1 | should-fix | `src/tools/bash.rs:66` | The call waits for pipe EOF, so a background child that inherits stdout keeps the call open until it exits. `sleep 6 & echo started` takes 6.2 s instead of milliseconds; `server &` never returns. Spec says background work is `bg` (M4) and a clock never kills work, but a plain `&` must not block `bash`. | test `background_child_does_not_block_the_call` |
| F2 | should-fix | `src/tools/bash.rs:64` | `collected: Vec<u8>` holds the whole output in memory although the file already has it. 200 MB of output => 199 MB+ working set (idle limit is 30 MB). Runaway `yes` grew to 878 MB and kept climbing. Only a head, a tail ring and the byte/line counts are needed. | `yes abcdefghij \| head -c 200000000` (observed via a canned SSE server, `scripts/live/canned_server.py`) |
| F3 | should-fix | `src/tools/mod.rs:21` | The provider-supplied call id is joined into a path unsanitised. A call id of `../../escaped` writes `commands/../../escaped/out`, outside the session dir; ids with `:` or `\` break on Windows. Public harness, untrusted model output. | test `call_id_cannot_escape_the_commands_dir` |
| F4 | should-fix | `src/stream.rs:146` | Ids repeat across steps (kelir's own fallback `call_<position>`, and some providers send `call_0` every step), so `commands/call_0/out` is overwritten and the earlier `tool_settled.out_path` points at the wrong bytes. Spec: output files are "never truncated". Namespace the dir by step or seq, and keep the provider id only as a label. | test `repeated_call_ids_keep_every_output_file` |
| F5 | should-fix | `src/tools/edit.rs:89` | A file with mixed endings (`a\r\nb\nc\n`) is treated as pure CRLF, so a multi-line `old` copied from the LF part says "not found". Match per line ending, or try the literal text first and fall back to CRLF conversion. | test `edit_works_on_mixed_line_endings` |
| F6 | should-fix | `src/session.rs:333` | Spec: "Payloads over 10 KB are never inline: the event holds the path." `assistant.tool_calls[].args`, `tool_started.input` and `assistant.text`/`reasoning` are logged inline whatever their size (a 50 KB `edit` writes it twice). Only `tool_settled.result` is capped. Not tested because it needs a decision on the on-disk shape. | read the code; an `edit` with a 50 KB `new` |
| N1 | nit | `src/session.rs:427-432` | If a tool task panics, `handle.await` is ignored, `collected` is short and `zip` silently drops the call: a `tool_started` with no `tool_settled`, and the next request is malformed (assistant calls with no tool message). Settle a panicked call as `error`. Not reproduced. | tool task panic |
| N2 | nit | `src/proc.rs`, `bash.rs:41` | The child is assigned to the Job Object after spawn, so a child that forks instantly can escape. Create suspended or use `PROC_THREAD_ATTRIBUTE_JOB_LIST` in M1. Kill-on-close itself works (below). | race, unlikely |
| N3 | nit | `src/tools/bash.rs:94` | `out_path` mixes separators when the state dir has forward slashes (`C:/…/state\sessions\…`). Cosmetic; `spill.rs` normalises the notice. | `KELIR_STATE_DIR` with `/` |
| N4 | nit | `src/spill.rs` hint | The hint says `rg`, which is not installed on this box. Spec asks for `rg`/`sed -n`; consider naming `sed -n` first. | model without rg |
| N5 | nit | `src/session.rs:97-103` | Usage errors (exit 2) print nothing on stdout in `--json` mode, so "ends with a `result` event" holds only for runs that start a session. | `kelir run --json` with no task |
| N6 | nit | `Cargo.toml` | `time`, `toml`, `ulid`, `windows-sys` are not named in the spec and the commit message does not justify them (AGENTS.md asks for a justification). All are small and plausible. | n/a |
| N7 | nit | `docs/tasks/REVIEW-PROTOCOL.md:29` | The orchestrator's file names a private path (`C:/Users/…`) in a public repo. Not the implementer's. | grep |

What I checked and found **correct**: the system prefix is frozen and contains a date but no
time; tools list identical on every request; step N+1's `messages` begin byte-identically with
step N's (verified on the wire, below); `session_id` in the OpenRouter body and no `provider.order`;
`cache_control` only for Anthropic/Qwen ids; `stream_options.include_usage` always on; results in
call order; `[exit code N]` prefix; non-UTF-8 output is lossy-decoded and counted in bytes;
malformed tool arguments become an `error` result; closed stdin without a task is exit 2; missing
key is exit 2 with a clear message; kill of the `kelir` process (`taskkill /F`) takes the tool's
whole process tree with it (Job Object kill-on-close), leaving `tool_started` without
`tool_settled`, which is the state M3 resume expects.

## Test pyramid

- **Unit (92):** good coverage of the SSE parser, fold, edit branches, spill boundary (10240 vs
  10241), request builder rules and torn-line tolerance.
- **E2E (13):** covers all cases in the task list. Weakness: `parallel_calls_settle_in_call_order`
  proves ordering but not concurrency (a sequential implementation passes); the new
  `parallel_calls_really_overlap` closes it and passes. The mock replies with a `Content-Length`
  body, so chunked streaming is exercised only by the unit tests and the live drive.
- **Added (`tests/review_m0.rs`, 8 tests):** concurrency overlap, background child, call-id
  traversal, repeated ids, mixed line endings, malformed arguments, non-UTF-8, empty stdin.
  4 pass, 4 fail (F1, F3, F4, F5).

## Live drive log

Deviation from the protocol: **the live runs were not in Herdr panes.** `hc run` starts the
program with the core's environment, which has no `OPENROUTER_API_KEY`, and the only ways to pass
it (argv, a file) are forbidden. The scenarios ran from the reviewer's own shell with the key
inherited; the scripts are in `scripts/live/` (`m0_common.sh`, `m0_s1…s5`). Outputs below are
from `<scratch>/M0/sN/out.ndjson`.

**S1: edit + bash (`m0_s1_hello.sh`): PASS.** 3 steps, exit 0. `tool_started`/`tool_settled`
pairs for `edit` then `bash`, final text "Created hello.txt … it says: hi". `jq` works on the
NDJSON; ends with `{"type":"result","outcome":"done","steps":3}`. Cost about $0.0003.
Note: `cached_tokens` was 0 on steps 2 and 3 here (prompts 703 and 756 tokens, steps under a
second apart); the cache had not warmed. Expected for DeepSeek's best-effort cache; S2 below is
the real measurement.

**S2: add a flag and a test, suite green (`m0_s2_cli.sh`): PASS, twice.** Python CLI in a
scratch git repo, `pytest` available. Run 1: 7 steps, cost $0.00136, 5 tests pass.
Run 2: 7 steps, 4 tests pass, `git status` shows only `greet.py` and `test_greet.py` modified.

| step | prompt | cached (run 1) | cached (run 2) |
|---|---|---|---|
| 1 | 719 / 711 | 0 | 0 |
| 2 | 1204 / 1102 | **640** | **768** |
| 3 | 1504 / 1205 | 1152 | 1024 |
| 4 | 1605 / 1797 | 1408 | 1536 |
| 5 | 2153 / 1930 | 1536 | 1792 |
| 6 | 2409 / 2089 | 2048 | 1792 |
| 7 | 2946 / 2232 | 2304 | 2048 |

`cached_tokens > 0` from step 2 on OpenRouter: **met** in both runs. `cache_write_tokens` stays 0
(DeepSeek reports no writes). Only 7 steps, not the 10-20 the task expected: the task was easy.

**Wire check (through `scripts/live/logging_proxy.py`, bodies only, never headers):** two
requests of S3: keys `messages, model, session_id, stream, stream_options, tools`; the same
`session_id` (`ses_01m7qat2…`) on both; no `cache_control` for a DeepSeek id; request 2's
`messages` JSON begins byte-identically with request 1's; `tools` equal.

**S3: parallel `sleep 3` (`m0_s3_parallel.sh`): PASS.** One assistant message with 2 calls.
`tool_started` seq 6 and 7 at `…20.778Z`; `tool_settled` seq 8 and 9 at `…23.878Z` (3.1 s for two
3 s sleeps, well under 6 s); results `first\n` then `second\n` in call order. Next request's tool
messages in call order.

**S4: >10 KB output (`m0_s4_spill.sh`): PASS.** 3000 lines, 82,893 bytes: `spilled: true`,
`result` is the notice with counts, path, 1 KB head and tail and the hint. The full file is on
disk at `commands/<call-id>/out`. The model then ran `sed -n '1500,1502p'` on the path and got
exactly lines 1500-1502 (84 bytes, not spilled).

**S5: shell and CRLF (`m0_s5_crlf.sh`): PASS.** `bash` resolved to Git for Windows
(`C:\Program Files\Git\bin\bash.exe`; inside it `command -v bash` is `/usr/bin/bash`,
`uname` `MINGW64_NT-10.0-26300`, bash 5.3.15). `edit` of `beta` -> `BETA` in a CRLF file:
`od -c` shows `a l p h a \r \n B E T A \r \n g a m m a \r \n`, 20 bytes before and after,
CRLF intact. (The mixed-ending case fails: F5.)

**Break-it drives** (canned SSE server, no model):

- Kill mid-tool: `sleep 31.337` as the tool, `taskkill /F` on `kelir`. `sleep.exe` present before,
  gone after: no orphan. Log ends `… assistant, usage, tool_started` (unsettled, as designed).
  PASS.
- 200 MB output: finished in 6 s, 200,000,000 bytes on disk, working set 199 MB+: F2.
- Runaway `yes`: no cap, no promotion (promotion is M4), memory 878 MB and rising when killed: F2.
- `sleep 6 & echo started`: call returned after 6.2 s: F1.
- Missing key: exit 2, `kelir: missing API key: set OPENROUTER_API_KEY`. Closed stdin, no task:
  exit 2. Unknown flag: exit 2. PASS.
- Not exercised: Ctrl-C into a console session (the reviewer's shell has no console to signal).
  Mid-stream disconnect and HTTP 500 retry are covered only by the mock e2e tests.

**Go path: not live-verified** (`KELIR_GO_API_KEY` is not available). Verified against the mock
only: `go_headers_carry_the_session_id_and_kelir_user_agent` passes (`x-opencode-session` equals the
session id, `User-Agent: kelir/0.1.0`, no `session_id` in the body).

## Spec items not yet covered

- Go provider live run, and "passes on both providers on host B" (M0 acceptance mentions two
  hosts; only this Windows host was driven).
- Go `reasoning_effort` field remains unverified (M0 sends no reasoning parameter; the
  implementer recorded this).
- Linux and macOS: compile via `cfg` only; CI has not run (nothing pushed). `ProcessGroup::kill`
  on Unix is a stub until M1.
- Ctrl-C handling is untested.
- F6: the 10 KB rule for event payloads other than `tool_settled.result`.

## Fix list for the implementer (in order)

1. F3 + F4 together: derive `commands/<step>-<n>` (or the event `seq`) as the directory; keep the
   provider id only as the `call_id` label. Makes `call_id_cannot_escape…` and
   `repeated_call_ids…` pass.
2. F1: stop waiting on the pipe after the shell exits (drain for a short grace period, then drop
   the readers), and let the Job Object take strays at the end of the call.
3. F2: stream to the file and keep only head, tail ring and counters.
4. F5: line-ending-aware matching.
5. F6: decide the shape (path under `commands/`) and spill oversized `args`, `text`, `reasoning`.
6. N1 and the nits as time allows.

## Re-verification (fix commits `c0c334b`, `3b2e5a8`)

**Verdict: PASS.** All six should-fix findings are fixed and verified on the release binary;
no item is still failing.

**Gates** (re-run on `3b2e5a8`): `cargo fmt --check` clean; `cargo clippy --all-targets -- -D
warnings` clean; `cargo test`: 110 unit, 16 e2e, 8 `review_m0` (all 8 now pass; before, 4
failed). Release binary 4.75 MB.

**F1-F6 on the release binary** (`scripts/live/m0b_recheck.sh`: a canned SSE server drives
`target/release/kelir`; no model, no key):

| # | result | evidence |
|---|---|---|
| F1 | fixed | `sleep 6.123 & echo started`: wall 0 s, result `started`. After kelir exited no `sleep.exe` was left (the session's Job Object closes on exit). |
| F2 | fixed | `yes abcdefghij \| head -c 200000000`: 6 s -> 3 s, **peak working set 8 MB** (was 199 MB+), 200,000,000 bytes on disk, `bytes`/`lines` correct (18,181,819), `spilled: true`, notice 2.5 KB. |
| F3 | fixed | call id `../../escaped`: dir is `commands/6-1-1`, nothing outside the session dir; `tool_settled.call_id` keeps the provider id as a label. |
| F4 | fixed | two calls both with id `call_0`: `commands/6-1-1/out` = `first`, `commands/11-2-1/out` = `second`; each `out_path` points at its own bytes. |
| F5 | fixed | `a\r\nb\nc\n` edited with `old: "b\nc"`: `ok`, file is `a \r \n B \n C \n` (each line keeps its own ending). |
| F6 | fixed | 50,000-byte `edit` payload: `events.jsonl` is 4.6 KB; `assistant.tool_calls[0].args` is `{"spilled":true,"path":"blobs/4-tool_calls.0.args","bytes":50036,"preview":…}`, `tool_started.input` likewise; the file was written with all 50,000 bytes. The fold reads blobs back, and a unit test asserts the request bytes are identical to the inline form, so the cache prefix is unaffected. |

Side effect to know about, not a defect: after `cmd &` the background child is left running
until the session ends (the fix keeps the Job Object open), and its stdout pipe is dropped, so a
child that keeps writing to the inherited pipe gets a broken pipe. Background jobs that must
survive and log belong to `bg` (M4); `nohup cmd >log 2>&1 &` already works.

**Live drive in a Herdr pane** (`hc run --bg --name live-m0b …`, key from the env file, never
printed; `scripts/live/m0b_pane.sh`; pane exit code 0), OpenRouter,
`deepseek/deepseek-v4.1-flash-1017`:

- **S1 (edit + bash): PASS.** 3 steps, `tool_started`/`tool_settled` pairs, final answer,
  `jq` works. Cached tokens 0 / 640 / 640 by step (prompts 642 / 739 / 792).
- **S2 (add a flag + test): PASS.** 5 steps, cost $0.00108, 4 tests pass, only `greet.py` and
  `test_greet.py` modified. `cached_tokens` per step: 0, **512**, 1024, 1024, 0. **`> 0` from
  step 2: met.** Step 5 dropped to 0 with a growing 1866-token prompt; kelir's prefix is
  byte-identical (verified by the e2e test and the earlier wire capture), so this is the
  provider's cache routing, not a request change. Worth watching in later milestones, not a fail.
- **F1 live: PASS.** The model ran `sleep 6 & echo started`; `tool_started` 13:45:23.02,
  `tool_settled` 13:45:23.28, **0.25 s**, result `started\n`.

Still open from the first review (unchanged, not in scope of this fix round): the Go provider is
mock-verified only; Ctrl-C into a console untested; N1, N2, N4-N7 are nits not re-checked.
