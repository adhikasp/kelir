# M5-A introspective retro (kelir reading its own trajectory)

Job: M5-A (sources + skills), implemented by a kelir session on the OpenCode Go provider,
model `deepseek-v4.1-flash`, window 1M. One session (`ses_01m7s18thvdvxqjma143adrvtx`), 1,907 events,
2026-11-19 04:33 -> 06:26 UTC. Sources read: `events.jsonl`, `commands/`, `blobs/` (17 spilled
reasoning blobs), `docs/retros/M5-A-impl-notes.md`, `docs/retros/FINDINGS.md`, `src/prompt.md`,
`src/tools/*`, `src/provider.rs`, `src/bg.rs`. Seq numbers below are `events.jsonl` seqs
(`tool_started` seq for a call, `assistant` seq for a step).

Caveat that shapes everything: the session ran a **pre-M5 binary**. `session_started.sources` was
`[]` and the system prompt was the old two-sentence default ("Reach for `bash` ... and `edit` ..."),
with no AGENTS.md in it. The new `src/prompt.md` was written *by this session* at step 61. So the
prompt findings below are about the prompt it ran with and what the new one still lacks.

## 1. Numbers

| | |
|---|---|
| Wall clock | 113.1 min |
| Steps (model requests) | 320 (1 final text step, 319 with tool calls) |
| Tool calls | 331: 320 `bash`, 11 `edit`; 12 steps carried 2 calls, the other 307 exactly 1 |
| Model time (request -> response) | 72.1 min (64%); tools + gaps about 41 min |
| Prompt tokens, total | 42,489,633 (context grew 1,044 -> 219,387, +682/step; 221 steps over 100K, 135 over 150K) |
| Cached tokens | 42,249,472 = **99.43%** hit; uncached 240,161; cache writes 0 |
| Completion tokens | 296,403 (mean 926/step; 49% of them in steps 38-62, see F1) |
| Cost | `0.0` on all 320 `usage` events (provider reports none) |
| Per-step cache ratio | step 1: 0%, 2: 29%, 4: 61%, 5-12: 83-89%, every step from 13 on: 97-100% |
| Latency, small step | mean 4.3 s API round trip; worst 192 s (step 59, 14,767 completion tokens, seq 311) |
| Tool results | 563,823 B (about 141K tokens); `cat`/`sed -n`/`head`/`tail` output is 426,820 B = 76% of it |
| Spilled results | 9 over the spill limit; 0 truncation problems |
| Compaction | none, ever (trigger is 80% of a 1M window) |
| User turns | 47 `user` events: 1 task, 2 `ctl say` "step limit, continue", 1 post-crash hint, 43 `job` messages |

Errors by type (331 calls):

| Class | Count | Seq / notes |
|---|---|---|
| ok | 325 | 314 bash + 11 edit (**edit: 11/11 ok**) |
| benign exit 1 (grep/`grep -c` with 0 matches) | 3 | 329, 1842, 1892 |
| real failure, loud (exit 1) | 2 | 419 python `AssertionError`; 1295 python heredoc truncated |
| **success-looking failure (exit 0)** | **5** | heredoc truncated 344, 399; python `AssertionError` hidden behind a following `cargo` 494, 582, 1137 |
| `unknown` (session died under the call) | 1 | 1380 -> `resumed` 1381 |
| cargo compile errors / failing suites seen in output | 19 outputs | normal edit-compile loop |
| provider error | 1 | 429 at seq 1632, `retryable`, recovered in 6 s |
| turn ended `error` | 2 | step cap at step 100 (seq 521) and 200 (seq 1254), resumed by `ctl say` |

Where the 113 minutes went (by step window):

| Window | Steps | Min | What |
|---|---|---|---|
| Exploration, 100% read-only | 1-37 | 1.9 | brief, AGENTS.md, spec ranges, source tour |
| **Design thrash** | 38-62 | **32.6** | 14 steps of 5.9K-14.8K completion tokens, one trivial `sed`/`grep` each, zero code written |
| Build sources.rs + wire session | 63-125 | 16.3 | includes heredoc truncation (F2) and python-patch misses (F3) |
| Env-leak diagnosis | 125-173 | 14.6 | suite inherits the live session's `KELIR_*` (real bug, fixed) |
| Skills verb + e2e tests | 186-224 | 9.2 | |
| Crash + "who killed me" hunt | 225-262 | 16.3 | `cargo test` killed the driving session at seq 1380 |
| Gates, Linux gate, docs | 263-303 | 15.3 | includes a flaky `r18` test (steps 283-303) |
| Closing | 304-320 | 4.9 | |

## 2. Findings, ranked by value / effort

### F1. The model re-designs the whole feature on nearly every step, because reasoning does not survive the step  (value: highest, effort: low for prompt+knob)

Evidence:
- Steps 38-62 took **32.6 min = 29% of the run** and **145,805 completion tokens = 49% of all completion
  tokens**. Fourteen steps (38, 39, 40, 48, 49, 50, 53, 54, 56-61) each generated 24-58 KB of reasoning
  (spilled blobs `201`..`322`, e.g. seq 258: 47,366 chars, seq 307: 48,085, seq 322: 54,779) to decide
  what to run next, and every one of those next actions was a `sed -n` or `grep`.
- Eleven of those blobs open with the same sentence ("Now I have a good picture. Let me design.", seq 265,
  282, 297, 307, 312, 317, 322, ...) and re-propose "New module `src/sources.rs`". Verbatim line overlap is
  only 2% (it re-derives, it does not copy), and the design did not converge: the names in the final code
  (`Places`, `push_source`, `Watch`) occur **0 times** in those blobs.
- Mechanism visible in the source: `fold` rebuilds assistant messages with `reasoning: None`
  (`src/fold.rs`; `ChatMessage` has no reasoning field, `src/chat.rs:33`), so each request carries the
  *results* of the previous step's thinking but not the thinking. And the request body has no reasoning
  control at all (`src/provider.rs:100`: only `model`, `messages`, `stream`, `stream_options`, `tools`).
  The model thinks at its provider-default effort, cold, every step.
- Steps 63+ show the contrast: once it started writing code the same model needed >2,500 completion tokens
  only 9 times in 258 steps.

Proposed change (do the first two, measure, then decide on the third):
1. Prompt (`src/prompt.md`), one line: `- Plan once, briefly; then act. Do not redesign after each tool result.`
   Stronger and still cheap: `Write the plan to a scratch file once; reread it instead of re-deriving it.`
2. Provider knob (`src/provider.rs` `build_request`, `src/config.rs`): optional per-provider/per-model
   `reasoning_effort` ("low"|"medium"|"high"), sent as `body["reasoning_effort"]` when set. Default
   "medium" for kelir's own agent loop. Needs a live check that the OpenCode Go gateway honours it; if not,
   try the OpenRouter-style `{"reasoning": {"effort": ...}}` form. Record the effective value in
   `session_started` so the cache key and the log agree.
3. Experiment, not a default: replay the *previous step's* reasoning only (as `reasoning_content` under the
   configured `reasoning_field`) within the same turn, drop it at the next user message and at compaction.
   Costs up to ~12K tokens per replayed step (so only worth it if (1)+(2) do not cut the thrash), but it is
   the form DeepSeek-style APIs document for tool-call turns. Unverified here; test on a mock first.

### F2. Git bash silently truncates any command over 8,191 chars, exit 0  (value: high, effort: low)

Evidence:
- Three calls were cut mid-heredoc: seq 344 (12,522 B, `cat > src/sources.rs <<'RUST'`), seq 399 (13,646 B),
  seq 1295 (14,670 B). Each result carried only
  `warning: here-document at line 1 delimited by end-of-file (wanted ...)`, and the first two had
  **exit 0** with a half-written file (`src/sources.rs` stopped at line 231 of 360).
- The model never found the cause. Its reasoning blames the content ("the content contains `RUST`?" step 66,
  "heredocs break" steps 67, 78, 209) and it switched tools three times. Total damage: steps 65-70, 76-78,
  208-210 = 12 steps, 5.1 min, 29K completion tokens, plus it pushed the model to `python -` heredocs (F3).
- Reproduced outside kelir with a plain `node` `spawnSync("bash.exe", ["-c", cmd])`: a 4.4 KB heredoc is
  fine, 8.9 KB is cut; largest working one-liner payload is 8,160 chars (command 8,190 B). Handing the
  same 60 KB script to bash as a **file** or on **stdin** works. A truncated one-liner usually dies with
  `unexpected EOF`, but a heredoc just writes less.
- Where it happens: `src/bg.rs` ~776-781, the shim runs `shell + shell_args + [cmd]` (`bash -c <cmd>`); the
  command travels as `--cmd <text>` on the shim's command line and then on bash's.

Proposed change (`src/bg.rs`, shim exec site; `src/tools/bash.rs` for the notice):
- On Windows, when `args.cmd.len() > 6000` (or always, it is cheap), write the command to
  `<call dir>/cmd.sh`, drop the trailing `-c` from `shell_args`, and run `[bash, "<call dir>/cmd.sh"]`.
  For the `cmd.exe /C` fallback, write `cmd.bat` (same 8,191 limit). cwd and env are unchanged.
- Regression test in `e2e_m4` or a new `e2e_longcmd`: a 40 KB heredoc that writes a file and prints its
  `wc -l` (Windows and Linux).
- Belt and braces in `bash.rs`: if the merged output contains `delimited by end-of-file`, append
  `[kelir: your command may have been cut at 8 KB; write large files with edit]`.
- Until it lands, tool description (see "cheap" list).

### F3. The model prefers python heredoc patches (56) to `edit` (11), and they fail silently  (value: medium-high, effort: low-medium)

Evidence:
- 56 `python - <<'PY'` patch scripts vs 11 `edit` calls, even though the prompt it ran with said "`edit` to
  change a file". 22 of the 56 do two or more replacements in one script. `edit` takes exactly one.
- `edit`: 11 calls, **0 failures** (including 4 empty-`old` file creations, seq 359, 409, 1305, 1682, and one 7.9 KB body, seq 374).
- python patches: 5 misses. Three were **exit 0** because `cargo build` ran after the script and masked it:
  seq 494 (`AssertionError` then a normal `Finished`), 582, 1137. Two were exit 1: 419, 1295.
  Plus escaping bugs that `edit` cannot have: seq 494-504 (`\\n` inside a Python string vs Rust `\n`, 4 steps),
  seq 1242 (`"a\b"` became a backspace: rustc `unknown character escape: b`, steps 198-202).
  Recovery windows: steps 80-86, 95-98, 112-115, 177-180, 198-202 = 24 steps, 3.7 min.
- Reasoning at step 68: "write via the `edit` tool ... reliably"; step 75: "python script to avoid heredoc
  issues". The model treats python as the multi-edit tool because `edit` has none.

Proposed change:
- `src/tools/mod.rs` spec + `src/tools/edit.rs`: accept `edits: [{old, new, replace_all?}]` as an alternative
  to `old`/`new` (all-or-nothing, applied in order against the evolving text, one result line per edit,
  same error text style as today). Description addition: "Several changes to one file: pass `edits`; all
  apply or none do." Keep `old`/`new` working; the tool list is fixed per session so the cache prefix only
  changes once, at release.
- Prompt line (already in the proposed text below): `Make changes with edit (it fails loudly); a python/sed
  patch can fail with exit 0.`
- Optional, low effort: in `bash.rs`, when the command is a `python -`/`sed -i` heredoc whose output contains
  `Traceback`, set the outcome to `error` regardless of the exit code.

### F4. 47% of steps are read-only and serial; the prompt says "small steps", the request never asks for parallel calls  (value: medium, effort: low)

Evidence:
- 150 of 319 tool-calling steps ran only `cat`/`sed -n`/`grep`/`ls`/`git log`. Eleven runs of 3+ consecutive
  read-only steps cover 90 steps; **steps 1-44 are one unbroken run** (a tour of the code base; steps 1-37 take 1.9 min,
  but each step re-sends a growing prefix). Batching every run 3-wide would remove
  59 steps = 5.0M prompt tokens (11.8% of the total, measured on the real per-step prompt sizes) and
  about 4 min.
- Only 12 of 319 steps used 2 calls, so the model *can* do it; nothing tells it to. `build_request` does not
  set `parallel_tool_calls`, though `models.rs` already parses it from `supported_parameters`.
- The prompt line "Keep steps small and verified" can be read as one call per step.

Proposed change: `src/provider.rs`: set `body["parallel_tool_calls"] = true` when the model's
`supported_parameters` has it. Prompt: replace "Keep steps small and verified" with
`Batch independent reads in one step; sed -n a,bp or git grep, not cat on big files.` and put the
"small and verified" half on edits (it is `- Make changes with edit ...`).

### F5. No compaction: every step re-sends up to 219K tokens, 76% of which is old file reads  (value: medium, effort: low-medium)

Evidence:
- 42.5M prompt tokens for a ~300K-token-of-output job; 135 steps above 150K. Cache hit is 99.4%, so the
  *price* is fine, but the **rate-limit footprint** (the 429 wave in `FINDINGS.md`) is raw prompt volume.
- Context composition at the end: tool results 564 KB (read-type output 427 KB), assistant tool-call args
  181 KB (the code it wrote), user 21 KB. The `cat`/`sed` output is dead weight once the code is written.
- Simulation on the real per-step prompt sizes: compacting whenever the prompt passes 120K down to 25K gives
  23.4M total vs 42.5M (-45%). A one-time cache miss per compaction is already the design.

Proposed change (`src/compact.rs` `AUTO_FRACTION`, `src/config.rs`): add `compact_at_tokens` (absolute soft
target, default 150000), compare against `min(window * auto_compact_at, compact_at_tokens)`. Pair with a
`compact` nudge in the prompt only if it does not fire (the model never called `compact` on its own, 0 of
320 steps). This is item (b) of the 429 finding, now with a measured effect.

### F6. Reports that sound delivered but were not verified  (value: medium, effort: trivial)

Evidence:
- `hc send t9m` (the id in the brief) returned `{"error":{"hint":"hc read t9m","kind":"finished",
  "message":"t9m has finished"}}` with **outcome ok, exit 0** (seq 1897). The model then sent to
  `$HC_TERMINAL_ID` (printed as `t10z`, seq 1902; per AGENTS.md "Report with `hc send $HC_TERMINAL_ID`")
  and its final text says "The report was delivered to the orchestrator." The log cannot show who `t10z` is;
  for a worker pane `$HC_TERMINAL_ID` is plausibly its *own* id. Nothing in the run checked.
- The brief asked for one short live pane run against OpenRouter. The model ran the release binary at a dead
  local endpoint just to print `session_started` (seq 1887). Its PROGRESS row and notes say "pending
  reviewer live drive", which is honest, but the reason (no live env access, or a choice) is not given.

Proposed change: AGENTS.md "Rules for everyone": `Report with hc send <the orchestrator id in your task>
(never $HC_TERMINAL_ID: that is your own pane). If hc prints an error, say so; do not claim delivery.`
Prompt, last bullet: `Say what you did, and what failed, when done.` The orchestrator should inject its own
pane id into the brief template, not leave a constant that goes stale.

### F7. Search hygiene: whole-tree `grep -r .` walked `target/`; no `rg` on the box  (value: low-medium, effort: trivial)

Evidence: seq 1489 `grep -rn '/IM\|//IM' --include=* -r . 2>/dev/null | grep -v "^./target"` ran **197 s**
(the filter is applied after the walk). seq 1636 `git grep` took 107 s while `remote-check.sh` ran in the
sibling call. `rg` is not installed (0 uses in 331 calls). Re-reads: 27% of the ranged-read lines were reads
of lines already in context (`src/session.rs` read 20 times, 850 of 2,420 lines repeated; `tests/support/mod.rs`
11 reads, 41% repeated). Big-file hygiene was otherwise good: `DEVIATIONS.md` (44 KB) was never `cat`ed whole,
SPEC read as three ranges, because the brief named the sections.

Proposed change: AGENTS.md "Rules": `Search with git grep (or grep -rn --exclude-dir=target); never grep -r .
at the repo root.` Prompt already carries `git grep` in the proposed batching line. Optional: a generated
`docs/CODEMAP.md` (module -> key fns with line numbers) so range reads start from known offsets.

### F8. A test child could impersonate the session, and one run killed its own driver  (value: medium, effort: medium)

Evidence (already fixed in the tests; the harness is still exposed): `cargo test` from inside the session
inherited `KELIR_SESSION_DIR`/`KELIR_SOCK`/`KELIR_CALL_ID`/`KELIR_BIN`. 43 `bg job ... was lost` messages
were injected into the session's context as `user` events (seq 667-1108; 17.8 KB in all), `e2e_m4` and
`review_m4` failed with the session's own jobs in the assertion text, and at seq 1380 a test cancelled a job
in the running session, killing it (`resumed`, seq 1381; resume worked: the call came back as
`unknown outcome ... Check before retrying`, which is good behaviour). Cost: steps 125-173 and 225-259 =
29 min of diagnosis.

Proposed change: the socket server (`src/ipc.rs` / `src/session.rs`) should refuse `bg` reports and
`cancel` requests for job ids it did not create or whose status file is not under its own `bg/` dir, and say
so in the reply. AGENTS.md: `Test helpers must build commands through support::command() (scrubs KELIR_*).`
(The impl notes record the helper; the rule is not yet written down.)

### F9. Cost and status are blind  (value: low-medium, effort: low)

All 320 `usage` events say `cost: 0.0` and `cache_write_tokens: 0`; the step cap (twice, seq 521 and 1254)
read to the orchestrator as an `error` turn and the second `ctl say` had to explain "your process still has the
old 100 per turn". STEER-4 covers the cap. Remaining: derive `cost` from a price table when the provider does
not report one (`models-<provider>.json` lists `id`/`context`/`tools` per model today and has no prices), and show
`steps used/limit` and the last prompt size in `kelir ctl status`, so the orchestrator can see "170K and
climbing" before a 429 does.

## 3. Cheap ones, safe to land now (none touch behaviour that tests pin, except where noted)

1. **`src/prompt.md`**: replace the last paragraph with the text below. 937 B raw against the 1,024 B budget
   (`src/sources.rs:695`); the skills directory lines add about 200 B at render time, outside the check.
   ```diff
   -The instruction files (AGENTS.md, CLAUDE.md) of this project and this machine
   -are already in this system prompt. Keep steps small and verified, and put long
   -output in a file rather than in the conversation.
   +The instruction files (AGENTS.md, CLAUDE.md) of this project and this machine
   +are already in this system prompt.
   +
   +- Plan once, briefly; then act. Do not redesign after each tool result.
   +- Make changes with `edit` (it fails loudly); a python/sed patch can fail with exit 0.
   +- Batch independent reads in one step; `sed -n a,bp` or `git grep`, not `cat` on big files.
   +- Put long output in a file. Say what you did, and what failed, when done.
   ```
   Check `base.len() <= 1024` and the render-determinism tests still pass.
2. **`src/tools/mod.rs` bash description** (string only): append
   `On Windows, a command over about 8 KB is silently cut by the shell; create big files with edit (empty old).`
3. **`src/tools/mod.rs` edit description** (string only): append
   `Prefer this to python/sed patches; they can fail with exit 0.` (and `edits` once F3 lands).
4. **`AGENTS.md`**: the F6 report-target rule, the F7 `git grep` rule, the F8 test-helper rule.
5. **`src/provider.rs`**: `parallel_tool_calls: true` when the model lists it (F4). One line plus a unit test
   next to `build_request`'s existing ones.
6. **`src/tools/bash.rs`**: append the F2 notice when output contains `delimited by end-of-file`.
7. **Config only**: `compact_at_tokens` is a new knob (F5), but the user config can already set
   `auto_compact_at = 0.15` on a 1M window today (150K) with no code change; use that on implementer jobs now.
8. **Brief template**: put the orchestrator's pane id in the brief at launch (F6); say plainly whether the
   implementer is expected to do the live drive or only the reviewer.

Do not land yet (need a live check): F1 items 2 and 3 (reasoning effort and replay), F2's shim change (needs
the Windows `e2e` and the Linux gate), F5's default value.

## 4. What went right (keep it)

- **Cache discipline held**: 99.43% hit from step 13 on, 0 cache writes, request prefix stable across 320
  steps, so rendering the system once (the point of M5-A) is validated by its own implementation run.
- **`edit` is reliable**: 0 failures; creating files with an empty `old` worked every time.
- **AGENTS.md rules followed** (one nit: the fourth commit is `docs: M5-A ...`, not `M5: <what>`): commits with explicit paths (no `git add -A`/`.`, 0
  history rewrites, 0 pushes), both Linux and Windows gates run, `tasklist` checked for leftovers (steps
  307-317), a scrub check against hostnames/usernames before reporting (steps 266-270), no key material
  printed, no kill-by-image-name in its own commands (4 hits for `taskkill` were `grep`s).
- **Recovery paths worked**: 429 retried and continued in 6 s (seq 1632); the crash resumed with an
  honest `unknown outcome` instead of re-running the call (seq 1381-1383); the step cap was resumed by
  the orchestrator with a clear sentence.
- **Big-doc discipline** (`FINDINGS.md` item c) held here: sections by range, `tail -80` of DEVIATIONS,
  no full `cat` of anything over 20 KB after step 19.

## 5. Open questions for the maintainer

- F1 item 3 (replaying reasoning) trades tokens for coherence; it is only worth testing if the effort knob
  and the prompt line do not remove the thrash. Which provider behaviour do we want to rely on?
- Should the shim write every command to a file on every platform (uniform, one code path), or only on
  Windows over a threshold (smaller change, two paths)? I lean uniform: the 8 KB cliff is exactly the kind
  of thing that must not depend on a size nobody tests.
- The session limit on concurrent workers is "3" by experience (FINDINGS); F5 would likely raise it.
  Worth re-measuring after a soft compaction target lands.
