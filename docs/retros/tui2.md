# tui2 retro (introspective): kelir implementing kelir

Subject: one implementer session, OpenCode Go provider, model `deepseek-v4.1-flash`, job tui2
(STEER-10 + STEER-3, TUI v2). Source: that session's `events.jsonl` (3069 events), its `commands/`
and `blobs/`, plus `docs/retros/tui2-impl-notes.md`, `docs/retros/FINDINGS.md`, `src/prompt.md`,
`src/tools/`. Seq numbers below are event `seq` values in that log. Nothing here quotes a secret.

## 1. Numbers

| Metric | Value |
|---|---|
| Wall clock | 12,884 s (3 h 35 m), 25 commits, +6,957/-490 lines, 534 tests green at the end |
| Steps / tool calls | 606 steps; 593 `bash`, 31 `edit`, 0 `compact`; 1.03 calls per step (19 steps had more than one call) |
| Time split | model 6,229 s (48%), tool execution 6,622 s (51%) |
| Tokens | prompt 145.9 M, of which cached 145.5 M (**hit ratio 99.7%**, uncached 0.42 M, max 6 k on one step); completion 335 k |
| Cost | `0.0` on every usage event: the provider reports none, so cost is not measurable from the log |
| Context growth | 2.9 k (step 1) -> 65 k (step 50) -> 115 k (100) -> 188 k (200) -> 265 k (300) -> 312 k (400) -> 385 k (606). Never compacted |
| Step latency by context | model time per step: 4 s (<50 k), 25 s (50-100 k, whole-file writes), 12 s (100-200 k), 8-9 s (>200 k); tool time per step 15 s above 200 k |
| Tool outcomes | 578 ok/0, 30 ok (no exit code, long-running), 15 exit 1/2, 1 edit error. 9 of the 15 are `grep`/`ls` finding nothing, not real failures |
| Real failures | 36 rustc `error[E...]` lines, 42 `test result: FAILED` lines (about 10 distinct failing tests, most re-run), 9 `cargo fmt` diffs, 6 heredocs that arrived truncated (finding 1), 4 python-heredoc syntax errors, 1 `edit` miss |
| `cargo test` | 181 invocations, 4,719 s = **37% of wall clock**; 48 commands ran the suite twice in one command (finding 2) |
| Editing style | `python - <<PY` patch scripts 143 times (1,169 s), `cat > file <<EOF` 34 times, `edit` 31 times |
| Linux gate | `remote-check.sh` 11 times, about 560 s |

Prompt-cache behaviour is excellent and needs no change: only step 1 missed, and the one
`request_hash`-stable prefix held for 3.5 hours. The cost of this run is therefore dominated by
**context size times step count** (145.9 M cached tokens read), not by cache misses.

## 2. Findings, ranked by value / effort

### F1. Git Bash truncates `bash -c` commands at about 8 KB, silently (HIGH value, LOW effort)

Evidence: six heredoc writes arrived cut mid-file: seq 399/400, 424/425, 499/500, 704/705,
949/950, 1094/1095. The tool result is exit 0 with a single stderr line
`warning: here-document at line 1 delimited by end-of-file (wanted EOF)`. In each case bash had
read only 192-302 lines, about 8.4-8.6 KB of the 10-20 KB command (388, 517, 336 lines in the
logged input). The file got the first 8 KB, the `echo written` that followed never ran, and one
cut `match` arm still compiled. The implementer saw it as "tool input is cut with no error line"
(its notes) and re-wrote files in chunks with `edit`; I count 6 truncated writes plus the
re-writes they forced.

Root cause, reproduced outside kelir with plain `spawn(bash.exe, ["-c", cmd])`: Git for Windows
bash cuts its `-c` argument at about 8,190 characters (8,000 ok, 8,500 truncated, 30,000
truncated). kelir passes the whole command as `--cmd <argv>` to `bg-shim` (`src/bg.rs:416`)
which then hands it to the shell as `-c`, so every long command hits it. `edit` is in-process and
took 10-23 KB inputs fine (seq 454, 884, 929, 964, 1109-1149).

Proposed change (loop/tool, `src/bg.rs` + `src/tools/bash.rs`): when the command is longer than
about 6,000 bytes, or always on Windows, write it to `<call_dir>/cmd.sh` and run
`<shell> <path>` instead of `<shell> -c <cmd>` (keep the logged command unchanged). Fallback if
that is too invasive for now: refuse, loudly, any command over 7,500 bytes on Windows with
`command too long for the shell (N bytes, limit about 8000 on Windows); write the file with the
edit tool (old: "") and run it`. Add a unit test with a 20 KB heredoc.

Prompt text (add to the `bash` bullet in `src/prompt.md`):

```
- `bash` runs a command (read, search, run). Keep each command under about 6 KB: write
  anything longer with `edit` (an empty `old` creates a file), never with a heredoc.
```

### F2. Long commands re-run to get a second view of the same output (HIGH value, LOW effort)

Evidence: 48 commands contain two `cargo test` runs, for example
`cargo test 2>&1 | grep FAILED ...; cargo test 2>&1 | grep -c "^test result: ok"`
(seq 424, 459, 704, 734, 739, 849, 899, 1059, 1209, 1214, 1219, 1259, 1505, 1610, 1675...).
They cost 2,656 s; half of that, about 1,300 s (10% of wall clock), is pure repetition. Three more
used `for i in 1 2 3` to chase a flake (seq 1675, 246 s). The model pipes through `grep` because
the spill notice is not what it expects, so it never keeps the output.

Proposed prompt text (new bullet, `src/prompt.md`):

```
- Run a slow command (build, test suite) **once**, send its output to a file
  (`cargo test > .t.log 2>&1; echo $?`), then query the file with `grep`/`tail`. Never run the
  same slow command twice to see a different slice. Re-run only the failing test by name.
```

Tool change (cheap): the `bash` result already lives in `commands/<n>/out`; append its path and
exit code to every result header (`[exit 0, 4.2 s, out: <path>]`), so the model learns it can grep
the file instead of re-running.

### F3. Context grew to 385 K tokens and compaction never ran (HIGH value, MEDIUM effort)

Evidence: step 606 sent 385,316 prompt tokens; zero `compact` calls; `auto_compact_at` is a
fraction (`src/config.rs:61`) of a 1 M window, so it would fire at 800 K. 145.9 M prompt tokens
for a 335 K completion run; per-step tool latency also doubled above 200 K. This matches the
FINDINGS.md 429 note (rate limits are token-rate limits).

Proposed change: add `compact_at_tokens` (absolute soft target, default 150,000) next to
`auto_compact_at` in `src/config.rs` and honour whichever is lower in `src/compact.rs`. Prompt
text (add after the `compact` bullet):

```
- `compact` when you finish a milestone or the history passes about 100 K tokens; the summary
  keeps the task and your commit list. A short context is faster and cheaper than a long one.
```

Related, cheap: `cost` is `0.0` for the OpenCode Go provider on all 606 usage events, so cost
cannot be audited. Let the model table in `src/models.rs` carry per-token prices (even rough ones)
and have `usage` compute cost when the provider omits it.

### F4. Destructive git in a worktree with uncommitted work (HIGH value, LOW effort)

Evidence: seq 1944 `git stash -u -q; git checkout -q ea9fc54 -- .` "to check the baseline flake";
the model's own reasoning at seq 1947: "Oops". It then ran `git checkout -- .`, `git stash pop`,
and `git reset --hard HEAD` (seq 1959-1969) and lost the uncommitted e2e test additions (about
390 lines), recovered from the dangling stash with `git show <sha>:path`. The prompt says
nothing about git safety, and AGENTS.md only forbids force-push.

Proposed prompt text (`src/prompt.md`):

```
- Never run `git stash`, `git checkout <rev> -- .`, `git reset --hard`, `git clean` or
  `git restore .` in a tree with uncommitted work. Commit a WIP commit first. To look at an older
  state, use `git show <rev>:<path>` or `git worktree add ../scratch <rev>`.
```

Loop option (medium): `bash` refuses those exact command prefixes when `git status --porcelain` is
non-empty, with a message naming the safe alternative.

### F5. Editing by python heredoc instead of the `edit` tool (MEDIUM value, MEDIUM effort)

Evidence: 143 `python - <<PY` patch scripts (1,169 s) against 31 `edit` calls. Syntax errors from
nested triple quotes and `$` handling: seq 735 and 790 (unterminated triple-quoted string), 1890,
2290, 2339. They are chosen because `edit` takes one `old`/`new` pair, so a multi-hunk change
needs several calls, and because `edit` cannot append (empty `old` on an existing file errors,
`src/tools/edit.rs:83`). The model also loses a step on the one miss (seq 2735: `old` not found)
because a failed `edit` returns no nearby text.

Proposed tool change (`src/tools/mod.rs` spec + `src/tools/edit.rs`):
1. Accept `edits: [{old, new}, ...]`, applied in order, all-or-nothing, in one call.
2. Add `append: true` (or accept empty `old` on an existing file when `append` is set).
3. On a miss, return the closest 3 lines (first line of `old` matched by prefix) in the error.

Prompt text: "Use `edit` for every change to an existing file; do not patch files with a script."

### F6. One tool call per step, many in strict sequence (MEDIUM value, LOW effort)

Evidence: 593 bash calls in 606 steps, 1.03 per step. 198 calls are read-only (`sed -n`, `cat`,
`grep`, `ls`, `git log`), and 145 steps sit in 33 runs of two or more consecutive read-only
single-call steps (longest run 30). At 8 s per step with a 300 K prompt, batching or chaining
even half of those saves about 10-15 minutes and 70 steps of prompt reads.

Proposed prompt text:

```
- Several independent tool calls in one step run together: read all the files you need in one
  step, not one per step. Chain `git add <paths> && git commit` and the test run in one command.
```

### F7. Misleading statements in the system prompt and the job brief (LOW-MEDIUM value, LOW effort)

- The job brief says "kelir does not auto-load instruction files yet, so FIRST read AGENTS.md".
  The session's `sources` show AGENTS.md loaded (hash 9de7d175...), and the model still ran
  `cat AGENTS.md | head -5` at seq 7. Fix in the orchestrator's brief template, not in kelir.
- `src/prompt.md` says the instruction files "of this project and this machine" are already in
  the system prompt, but `sources` lists only `prompt.md` and the repo `AGENTS.md`. The machine
  context (the shell is Git Bash, hostnames, Windows quirks) is absent, so the model had to
  rediscover that `python`, `sed -i` and `/c/...` paths work. Either load a machine file from
  the user's config dir by default, or reword to "of this project".
- The system prompt has no hint about the shell: on Windows it is Git Bash (`/c/...` and
  `C:/...` both work, CRLF in python errors). One line from the `# Environment` block would do:
  `shell: bash (Git for Windows), command length limit ~8 KB`.

### F8. Process hygiene rules followed loosely (MEDIUM value, LOW effort)

Followed: `tasklist` checked 4 times (seq 1999-2039); commits were `tui2: ...` with explicit
paths; no `taskkill /IM`; the Linux gate ran 11 times; notes written.
Not followed:
- Seq 2539, 2964, 2979: `Get-CimInstance Win32_Process | Where CommandLine -like '*srv.py*' |
  Stop-Process`. That matches **any** worker's `srv.py` mock server, which breaks the rule "kill
  by exact PID" and could kill a sibling job's mock.
- Seq 2824: `git add -A src/ tests/` against the brief's "explicit paths, never git add -A".
- Scratch dirs outside the worktree (`tui2-smoke*`, `t2-smoke`, `kgate-tui2-base`) were left
  behind in the home directory.

Prompt text: "Start helper processes with their PID written to a file, and kill exactly that PID
(`kill $(cat pid)`); never match on command line or image name. Keep scratch under the job's
state dir, not the home directory."

### F9. Gate repetition and flaky tests (MEDIUM value, MEDIUM effort)

Evidence: "three full runs" per gate (notes), `for i in 1 2 3` loops (seq 1675, 1929), `sleep 60`
then re-run (seq 2835); the same few tests flake under load (`a_ctrl_c_before_the_first_message_
does_not_stop_the_turn` x3, `kelir_view_refuses_a_missing_image...` x3), with five workers
contending for CPU. Roughly 600-900 s went to re-proving green. About 76 minutes of the 215 happened after the
notes and PROGRESS row were written (commit 89ecf2a at 20:10, last commit 21:26), mostly
polish and gates repeated.

Proposed: AGENTS.md "Gates" gets one line: "A gate is one full run per platform. If a test fails,
run that test alone by name; a test that passes alone and fails under load goes in
docs/DEVIATIONS.md as a flake, it is not retried in a loop." Harness side (later): a
`KELIR_TEST_THREADS` default of 4 in `scripts/` when more than one worker is active.

### F10. Exit codes of "found nothing" read as errors (LOW value, LOW effort)

Evidence: 9 of the 15 error outcomes are `grep`/`ls`/`find` with no match (seq 259, 685, 990,
1760, 1975, 2515, 2650...). The result is `error` with `[exit code 1]`; the model reads it as a
failure and often issues a second check. Tool change in `src/tools/bash.rs`: when the output is
empty and the exit code is 1, report `ok (exit 1, no output)`; leave exit 2+ as error.

## 3. Cheap ones safe to land now

Prompt only (`src/prompt.md`, no code):
1. "Keep each command under about 6 KB; write longer files with `edit` (empty `old`)". (F1)
2. "Run a slow command once, send output to a file, query the file". (F2)
3. "Never `git stash`/`checkout <rev> -- .`/`reset --hard`/`clean` with uncommitted work". (F4)
4. "Batch independent tool calls in one step; read all needed files in one step". (F6)
5. "Use `edit` for existing files; no patch scripts". (F5)
6. "Kill by exact recorded PID, never by command-line match or image name". (F8)
7. "`compact` at a milestone or about 100 K tokens". (F3)

Code, small and local:
8. Reject commands over 7,500 bytes on Windows with the explicit message in F1 (`src/tools/bash.rs`,
   a 10-line check plus a test). Then the file-based launch can follow.
9. Append `exit`, duration and `out` path to every bash result header (F2).
10. Report `grep`-style "no match" exit 1 with empty output as `ok` (F10).
11. Orchestrator brief template: drop "kelir does not auto-load instruction files yet" (F7).
12. AGENTS.md gates wording (F9) and the "scratch under the state dir" line (F8).

Needs design: file-based command launch (F1), `compact_at_tokens` (F3), multi-hunk and append
`edit` (F5), cost table (F3), git guard (F4).

## 4. What went well (keep)

- Cache discipline: 99.7% hit across 606 steps and 3.5 hours; tail-source design (image arrives
  as a tail event, prefix byte-identical) held up in the live log.
- Output-to-file habit: 14 results spilled and the model read them with `sed -n` ranges rather
  than re-running; big docs (SPEC, DEVIATIONS) were read in ranges, not with `cat` (compare the
  M5-A finding in FINDINGS.md).
- Recovery: the F1 truncation and F4 stash incident were both detected within 1-2 steps and
  recovered without losing committed work; every failure class in the table above ended green.
- Docs and notes were written in the same pass, and the notes name the truncation problem
  accurately, which is how it was found here.
