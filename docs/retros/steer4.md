# steer4 introspective retro: kelir reading its own trajectory

Subject: the STEER-4 implementer run (kelir on OpenCode Go, `deepseek-v4.1-flash`, one session,
`ses_01m7s6mm2q4p49r2rcnm7haeb3`, 2026-11-19 06:06:57Z to 07:14:55Z). Sources: `events.jsonl` (1038
events), `commands/`, `blobs/`, `bg/*.status`, `docs/retros/steer4-impl-notes.md`, `docs/retros/FINDINGS.md`,
`src/prompt.md`, `src/tools/*`, `src/provider.rs`, `src/fold.rs`, `src/bg.rs`. Event numbers below are
`seq` values from that log. Nothing in `src/` was edited.

Caveat that bounds every prompt finding: the run used a **pinned binary older than M5**. Its system
prompt (hash `1116f4181d947c56`, `sources: []`) is the three-line M0 prompt, not the current
`src/prompt.md`, and the orchestrator's first user message did the instruction-file loading by hand
("kelir does not auto-load instruction files yet, so FIRST read with bash: AGENTS.md ..."). So this
trajectory tests the tool surface and loop well, and the *current* prompt not at all. Finding 9 covers that.

## Numbers

| | |
|---|---|
| Wall clock | 67 min 58 s (4078 s) |
| Model time (step_started to assistant) | 2981 s, 73% |
| Tool time | 1007 s, 25%: 950 s of it is `cargo build/test/clippy` and `remote-check.sh` |
| Dead time after the 429 | 96 s (seq 89 to 93), then a human-sent `say` was needed |
| Steps / turns | 189 steps (188 with usage; step 14 never got a response), 2 turns (`error`, `done`) |
| Tool calls | 232: 211 `bash`, 21 `edit`; 45 steps issued 2 calls, 142 issued 1 |
| Prompt tokens, total over all steps | 17,782,617 (peak 151,889 at step 187; window is 1M, 15%) |
| Cached tokens | 17,618,688, **99.08%**; fresh input only 163,929 |
| Completion tokens | 196,449 (all 188 steps) |
| Cost | **0.0 on all 188 `usage` events**: the provider returns none and kelir cannot say what the run cost |
| Compactions | 0 (no `compact` call, no auto trigger: the context never got near 80% of 1M) |
| Provider errors | 3 x `http 429`, all inside step 14, 0.8 s apart, then `turn_ended outcome=error` |
| Tool outcomes | 232 settled: 4 non-ok. seq 607 benign (`grep` exit 1), 628 (truncated heredoc, finding 2), 633 and 648 (`edit` with empty `old` on an existing file) |
| Spills | 5 outputs spilled (114 KB of 499 KB total tool output); all five were `cat` of whole files |
| Reasoning | 105 of 188 assistant events carry `reasoning`; 26 spilled to `blobs/` (480 KB); assistant `text` is non-empty in only 39 of 188 steps |

Per-phase completion tokens (the big one):

| steps | completion tokens | per step | model time | reasoning chars |
|---|---|---|---|---|
| 1-24 (orient, one 429) | 4,077 | 177 | 77 s | 4.4 K |
| **25-63 (design + first edits)** | **129,029** | **3,308** | **1750 s** | **449 K** |
| 64-103 | 24,895 | 622 | 419 s | 63 K |
| 104-189 | 38,448 | 447 | 735 s | 56 K |

Steps 25-63 are 21% of the steps, 66% of all completion tokens and 59% of all model time. The first
`edit` landed at seq 306 (step 46, 06:28:05Z), 21 min in, 31% of the wall clock, after 45 read-only steps.

Cache behaviour is not a problem. Hit ratio per step averages 97.9% from step 3 on; the only steps under
90% are 3-8 and 23, where a large fresh tool output entered the context. Nothing in the loop or prompt
needs a cache fix. `request_hash` never repeats (no wasted duplicate requests).

## What it did well (keep)

- Spilling worked as designed: `cat docs/SPEC.md` (26 KB) and `cat docs/DEVIATIONS.md` (44 KB) came back as
  a notice + 1 KB head + a `sed -n`/`rg -n` hint (seq 15, 16, 70), and the model immediately switched to
  ranged reads. The hint line is the reason; it should stay.
- Gates and rules: eight small `steer4: ...` commits with explicit `git add <paths>`, no `git add -A`, no
  force, no `taskkill`/`pkill` anywhere, `tasklist` leak checks at seq 863, 868, 978, 1028, Linux gate with the
  private work dir (`remote-check.sh the maintainer kelir-verify-steer4`) three times, mock providers only, no
  secrets (the only `oc_sk_` hit is the SPEC text `oc_sk_…`). It spawned nothing
  unbounded. It also wrote the impl notes and an honest final summary.
- Rule it bent: AGENTS.md says read SPEC fully. The spill made that impossible by `cat`; it read about
  half of SPEC.md and ~10% of DEVIATIONS.md by ranges, targeted at the job. Reasonable, but see finding 9.
- The python patch scripts it wrote all carry an `assert old in s` check, so no silent no-op patch.

## Findings, ranked by value / effort

### 1. Reasoning is thrown away between steps, so the model re-derives the same design every step (highest value)

Evidence. `src/fold.rs` rebuilds history with `ChatMessage::assistant(&text, &calls)`; `reasoning` is logged
(`event.rs:284`) but never sent back. Steps 25-63 had 3.3 K completion tokens each, almost all hidden
reasoning, while the prompt grew only ~1.4 K tokens per step (seq 163..402; step 44 to 48:
68,632 -> 70,775), which proves the reasoning is not in the next request. The assistant `text` is empty in
almost all of those steps (a one-line sentence at steps 25, 40, 45, 60 and 63), so **the plan lives nowhere**. In the 26 spilled
reasoning blobs, 23 contain `Option<u32>`, 24 contain `CommonArgs`, 24 contain `TurnEnd::Paused`, and
all 26 contain `continue_a_turn_that_ends_on_an_answer`: the same config/CLI/session design restated
from scratch in blobs 199, 206, 213, 220, 227, 234, 241, 248, 262, 269, 276, 283, 290, 297, 304 and again
in 353-387 while already editing (compare the openings of 206, 220 and 304: "Now I have a good picture. Let
me design", "Now I have a good picture. Let me plan", "Now I have a clear design. Let me write out the
plan"). That is ~29 minutes and ~129 K output tokens before and during the first edits.

Proposed change, in two parts, cheapest first.

(a) Prompt, `src/prompt.md`, land now. The model has no way to know its reasoning is dropped; tell it, and
tell it where to put the plan, which is the one place that is kept:

```diff
 You are kelir, an autonomous agent harness: there are no approvals and no human
 in the loop. Act, verify what you changed, and say what you did when you are
 done.

+**Your reasoning is not kept from one step to the next; your messages are.**
+Before the first tool call of a task, write the plan as plain text (files to
+touch, the decisions you made, how you will verify). Add a line after each
+discovery that changes it. Then follow it: do not re-derive a decision that is
+already written above.
+
 - `bash` runs a command (read, search, run); `edit` changes one file;
```

(b) Loop, `src/fold.rs` (carry `reasoning` into the folded assistant message of the *current turn only*) and
`src/chat.rs` (serialise it under the provider's `reasoning_field`, which config already has), dropping it at
a turn boundary and at compaction. Needs an A/B before it ships: replay would add ~3 K tokens per step to a
context that is 99% cached (so cheap in dollars, not free in the 429 rate budget), and DeepSeek's own thinking
mode expects reasoning to be passed back inside a tool-call turn, which would be the reason to do it.
Measure with the same STEER-4 brief both ways: completion tokens per step in steps 25-63 is the metric.
If (a) already collapses that phase, (b) is optional. Unverified: whether Go's endpoint honours replayed
reasoning.

Also worth one experiment: `reasoning_effort` low/medium for implementer jobs (SPEC lists the Go field name
as unverified, M0).

### 2. A bash command over ~8 KB is silently truncated on Windows (a tool bug, verified)

Evidence. seq 613: the model sent a 9,033-byte `cat > tests/e2e_steer4.rs <<'EOF' ... EOF`. The `bg/613-104-1`
status file shows the shim received all 8,785 chars (including the closing `EOF` and `echo written`), yet the
file on disk is 8,118 bytes, 223 lines, cut mid-statement (`assert_eq!(run.code, 0, "{}", `). Output was
`warning: here-document at line 1 delimited by end-of-file (wanted EOF)` with **exit 0** (seq 613, then 618,
623). The model retried with a python heredoc, 8,849 chars, truncated again (seq 628: `SyntaxError:
unterminated triple-quoted string`). I reproduced it outside kelir: `"C:\Program Files\Git\bin\bash.exe" -c <N
chars>` returns the right length for N=8000 and a truncated, unterminated command for N=8200, 9000, 20000.
Same for `usr\bin\bash.exe`. Passing the same script as a file (`bash big.sh`) or on stdin (`bash -s`) works
for 20,000 chars. The cut is in the Windows argument hand-off from the shim to Git's bash, `bg.rs:781`
(`.arg(&args.cmd)`), not in kelir's event or status path.

Cost to the run: 9 steps / 71 s (seq 611-653), two generations of the same 9 KB file (4,165 and 2,722
completion tokens), an `edit` detour that failed twice (633, 648: "`old` is empty but file already exists"),
an `rm`, then five `cat >>` appends. The impl notes blame "the tool's command length limit" without knowing
where; the next worker will hit it too.

Proposed change, `src/bg.rs` shim (around line 781) and `src/tools/bash.rs`: when the command is longer than
~6000 bytes (any platform is fine, Windows is where it bites), write it to `<call dir>/cmd.sh` and run the
shell with `-c '. "<path>"'` (sourcing keeps cwd/env semantics), or feed it to stdin for bash-like shells.
Only for shells whose spec is `[sh, "-c"]`; keep the argument form for `cmd.exe`/agshell. Add a unit test with
a 20 KB heredoc that must round-trip byte-exact (Windows-gated if needed). Until it lands, put the
workaround in the prompt (cheap list, item 2).

### 3. A 429 ends the turn in 1.5 s and then waits for a human

Evidence. seq 89, 90, 91 are the three `429`s at 06:07:37.517, :38.281, :39.068, then `turn_ended outcome=error`
(seq 92). `provider.rs:49-54`: `MAX_ATTEMPTS = 3`, backoff 200/400/800 ms, no `Retry-After`. The session sat idle
96 s until the orchestrator's `say` (seq 93); the worker would have sat forever otherwise. This is the same
failure as FINDINGS item (a) in the main repo, now with timestamps: a rate limit lasting seconds to minutes
cannot be survived by a 1.4 s budget.

Proposed change, `src/provider.rs` (`ProviderError` gets `retry_after: Option<Duration>`, parsed from the
header) and the retry loop at `src/session.rs:2339-2416`: for 429/503 use `max(Retry-After, 5 s, 15 s, 45 s,
2 min, 5 min)` for up to ~10 min total, emit a `status`/UI line ("rate limited, retrying in 45 s"), and if the
budget runs out end the turn `paused` (the outcome STEER-4 just added), not `error`, so `resume`/`say
continue` is the documented path. 5xx and transport errors keep the short schedule.

### 4. Multi-hunk edits go through python heredocs; `edit` is one hunk per call

Evidence. 26 `python - <<'EOF'` patch calls against 21 `edit` calls (every python patch has the same
`io.open(p, newline='')` / `old` / `new` / `assert` skeleton). The recurring failure of the pattern: a `\n`
inside a `'''...'''` literal is interpreted, so Rust string literals got real newlines. seq 773-833 is that
cleanup: 15 steps, 97 s, `cat -A` x3, three `cargo fmt --check` rounds, two more python repairs (783, 788, 813,
818) and two `edit` fixes (798, 803, 833). The first-time quoting also costs: each patch call is two steps
(patch, then a build to see if it applied).

Proposed change, `src/tools/mod.rs:194` and `src/tools/edit.rs`: accept an `edits: [{old, new, replace_all?}]`
array as an alternative to `old`/`new`, applied in order against one read, all-or-nothing, one result listing
the changed line numbers per hunk. Description diff:

```diff
-"description": "Replace an exact string in one file. `old` must match exactly once unless `replace_all` is true. An empty `old` creates a missing file with `new`. Line endings are preserved.",
+"description": "Replace exact strings in one file. One change: `old` + `new`. Several: `edits: [{old, new}, ...]`, applied in order, all or none. Each `old` must match exactly once unless `replace_all` is true. An empty `old` creates a missing file with `new`; with `append: true` and an empty `old` it appends `new` to an existing file. Line endings are preserved.",
```

and the empty-`old` error text (`edit.rs`) becomes: "`old` is empty but PATH already exists; send the exact text to replace, or `append: true`
to add to the end". This also removes the main reason to write long files with heredocs (finding 2).

### 5. Exploration is by sequential `grep` then `sed -n` against 3000-line files; parallelism fades

Evidence. 71 `grep` calls, 67 `| head`, `session.rs` read 23 times by line range (585 lines re-read
because ranges overlap: 2060-2200 at seq 41, 134; 2160-2190 at 414; 2120-2160 at 419, and so on), `tui.rs` 15
reads, `config.rs` 9. Calls per step: 1.83 (steps 1-24), 1.62 (25-63), **1.01 (64-189)**. In the last 126 steps
the model waits one round trip (avg 9.2 s of model time, 3-7 s even for a 70-token reply at 150 K context)
for every single grep or range. Nothing in the prompt tells it to batch.

Proposed change. Prompt (cheap list, item 3). Tool, optional: a `read` mode in `bash`'s neighbourhood is more
than needed; the prompt line plus `rg -n -C` covers most of it. Not recommended yet: a new tool, because the
spill hint plus ranges already works.

### 6. Every bash call starts with `cd <cwd> &&`

Evidence. 207 of 211 bash calls begin `cd C:/Users/the maintainer/kelir-wt/steer4 && `. The cwd is the session cwd
and every call starts there. The environment block gives `cwd:` but nothing says it is the start dir, and
nothing says calls do not share shell state. ~55 chars x 207 = 11 K chars of output tokens, and each one is an
absolute path that has to be right (a quoting hazard in a Windows path with spaces). The prompt is silent.

Proposed change: prompt line (cheap list, item 1), plus `shell: <name>` in the Environment block
(`src/sources.rs:452`), so the model knows it is Git bash and not cmd.exe.

### 7. Gates re-run in full after docs-only changes

Evidence. The remote check took ~110 s each time and ran three times (seq 918, 973, 1023), full `cargo test`
four times (843, 858, 963, 1018: 59, 76, 99, 78 s). The last pair (1018, 1023: 190 s) ran after a commit that
only added the retro notes. Code changed after the first green remote check (the `state` verb, seq 938-968), so
two runs were legitimate; the third was not. 950 s of tool time was gates, 23% of the wall clock.

Proposed change, `AGENTS.md` (this repo): "Gates are per code change. A docs-only commit needs
`cargo fmt --check` only. Run the full Linux check once after the last code change." Nothing in `src/`.

### 8. Observability: cost is 0.0, no per-run token total, no 'how big is my context' signal

Evidence. All 188 `usage` events have `cost: 0.0` and no `allowance`; the run's price cannot be reconstructed
from the log without an external rate table. At peak the prompt was 152 K tokens at a 99% hit rate, 17.8 M
prompt tokens in 68 minutes (~260 K/min), and a rate limit on tokens would see that whole figure. FINDINGS
already proposes `compact_at_tokens`; this run suggests a compaction around 100 K would have cut the total
processed by roughly a third (steps 105-189 averaged ~129 K), but it also would have thrown away the file
reads the model kept needing, so treat it as a rate-budget lever, not a cost lever. It is not evidenced as
the cause of the 429 (five workers ran at once).

Proposed change: `ctl state`/the TUI footer should show cumulative `prompt/cached/completion` tokens and the
last prompt size next to the window; fall back to "cost n/a (provider reports none)" instead of `$0.00`.
Files: `src/session.rs` (state verb), `src/tui.rs`. Whether Go's `/models` carries a per-token price to compute
a cost is **unverified here** (no key used); if it does, `src/models.rs` can read it, otherwise keep tokens only.

### 9. This trajectory does not test the current prompt; fix the harness before the next retro

Evidence. `session_started.system` is the M0 text and `sources: []`; the current `src/prompt.md` says "The
instruction files ... are already in this system prompt". The orchestrator's user message told the worker the
opposite. The worker read `AGENTS.md` and the brief in its first step, so nothing broke, but one of the two
statements is false for any given binary, and the worker was never given the SPEC/DEVIATIONS section map that
FINDINGS item (c) asks for (it cat'd both, got heads, then grepped them: seq 13-28).

Proposed change: (a) rebuild the pinned worker binary from current `main` before the next job so the real
prompt is exercised; (b) briefs should name SPEC/DEVIATIONS sections (`sed -n 100,175p` ranges) instead of
"read docs/SPEC.md" (orchestrator-side, no `src/` change); (c) if both coexist, make the prompt sentence
conditional on `sources` being non-empty.

### 10. Visibility of a quiet failure mode in the bash tool result

Evidence. The truncated heredoc exited 0, so the tool said `outcome=ok` (seq 613). The only signal was a
`warning:` line the model had to read in the output. Related: the `python` patch calls print `ok\r\n` (CRLF on
Windows), harmless.

Proposed change: in `bash.rs`, after the call, if the output contains `here-document at line` and `delimited by
end-of-file`, prepend `[kelir: your command was cut off: it ended inside a heredoc]` to the result. Belt and
braces next to finding 2; also catches a truly unterminated heredoc written by the model.

## Cheap ones, safe to land now

1. `src/prompt.md`, plan-in-text line (finding 1a, diff above).
2. `src/prompt.md`, tool notes (findings 2, 5, 6):

```diff
 - `bash` runs a command (read, search, run); `edit` changes one file;
   `compact` shrinks this conversation.
+- Every `bash` call is a fresh shell that starts in the session cwd (see
+  Environment); do not `cd` into it first. Put independent reads and searches
+  in the same step as parallel tool calls.
+- Big files: `rg -n` first, then `sed -n 'A,Bp'` ranges (several per call). A
+  `cat` of a long file is spilled and you only get its head.
+- On Windows keep one `bash` command under ~6 KB: a longer one is silently cut
+  off. Make a long new file with `edit` (empty `old`) and grow it with further
+  edits, not with one large heredoc.
```
   (Remove the last bullet when finding 2 lands.)
3. `src/sources.rs:452`: add `shell: <first element of config.shell>` to the Environment block.
4. `src/tools/edit.rs`: reword the "`old` is empty but the file exists" error to say what to do instead
   (finding 4). Error text only.
5. `src/tools/bash.rs`: the heredoc-cut-off marker (finding 10). A few lines plus a unit test.
6. `AGENTS.md`: gates-per-code-change rule (finding 7).
7. `docs/retros/FINDINGS.md`: append the 429 timing (finding 3) and the 8 KB truncation (finding 2) so the
   orchestrator sees them in the running list.

Not cheap, in value order: 1b (reasoning replay, needs an A/B), 2 (script-file launch in `bg.rs`), 3 (429
policy), 4 (batch `edits`), 8 (usage/cost display).

## Process notes for the orchestrator

- Steps 25-63 are where the next prompt change will show or not show: compare completion tokens per step and
  the seq of the first `edit` against this run (step 46 / 21 min / 129 K tokens for this brief).
- The `say_mid_turn_is_in_the_next_request` flake from the impl notes is visible in this log: seq 848 (a
  re-run of `e2e_verbs`) failed with `the message must come after the tool result` (left `[system, user,
  assistant, tool]`, right `[..., tool, user]`, `e2e_verbs.rs:78`), and passed on the re-run at seq 853 and in
  the full runs at 858, 963 and 1018. A timing assumption in the test, not a kelir prompt item: a reviewer
  item.
- The stopgap `max_steps = 4294967295` in the user config was not exercised here (the run never hit a cap);
  the 189 steps are the first evidence in this repo that a no-cap session goes past 100 steps cleanly.
