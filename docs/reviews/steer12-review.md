# STEER-12 review (job/steer12 @ b16f634, diff b7b60c6..HEAD)

**Verdict: PASS WITH FIXES.** 0 blockers, 4 should-fix, 4 nits. The prompt change is true, deterministic,
1018 bytes, and on a small live A/B it does what it says (fewer read calls). But the BEFORE baseline is not
a baseline of the old prompt, the batching advice collides with the 10 KB spill limit, and the doc's
`parallel_tool_calls` reasoning rests on a false premise. None of these breaks the product; all of them
affect how far the AFTER comparison can be believed.

## Gates (run by me, own target dir)

| gate | result |
|---|---|
| `cargo fmt --check` | ok |
| `cargo clippy --all-targets -- -D warnings` | ok |
| `cargo nextest run --no-fail-fast -j 4` (Windows) | 868 run, 868 passed, 3 skipped, 140 s |
| `bash scripts/remote-check.sh the maintainer kelir-verify-steer12-rev` (Linux, HEAD b16f634) | rc=0; nextest 868 passed, 3 skipped; doctests ok |
| leaked processes | none of mine (canned servers killed by exact PID; panes removed) |

## Findings

| id | sev | where | finding |
|---|---|---|---|
| S1 | should-fix | `src/prompt.md:13-17`, `src/spill.rs` | **The batching rule collides with the 10 KB spill limit.** Over a spill the model sees only the first 1 KB and last 1 KB. A 42 KB labelled batch (SPEC + DEVIATIONS + `git grep` + `tail`) came back as 2.5 KB: only `=== spec` is visible, three of four sections are gone, and the visible tail looks like a complete answer to the last read. On the six real logs, 3 consecutive reads sum to >10 KB in **58%** of windows (median 12.5 KB), 4 reads in 79%, 5 in 89%. Following "three reads in a row is one command" would spill more than half the time, and each spill costs a recovery step (`sed -n` on the spill file), the opposite of the goal. The prompt gives no output budget. Fix: say "batch output under ~10 KB (`head -n`, ranged `sed`)" in place of the `<8 KB` (see N1), or make the spill keep the head of each labelled section. |
| S2 | should-fix | `scripts/metrics/trajectory.py:196-226` (`classify`, `is_read_call`) | **Writes count as read calls.** Redirects, `sed -i`, `find -delete`, `git branch -D`, `git config k v`, `sleep` are classified as reads. Real data: `cat > file <<'EOF'` x3 and `git show X:f > /tmp/f` counted as reads (711 vs my 708). The new prompt tells the model to write notes, so a model that writes them with `cat > notes.md <<EOF` inflates exactly the metric the AFTER table is judged on. Failing test: `cargo test --test review_steer12 -- --ignored a_write_is_not_a_read_call`. |
| S3 | should-fix | `docs/PROMPT.md` ("BEFORE — six sessions on the old prompt") | **The BEFORE sessions did not run the prompt being replaced.** From `session_started.system.text`: only `buildinfrafix` (203 of 1727 steps, 12%) ran the "Batch independent reads (parallel calls work)" prompt. `steer1fix`, `steer3`, `steer5` ran a 277-char pre-M5 prompt ("Reach for `bash` to read, search ..."), and `tui2`, `tui2fix` ran a third wording with no batching or parallel line at all. The table is a baseline of mixed older prompts: it does measure the harness before STEER-12, but "old prompt vs new prompt" cannot be attributed from it, and the doc should say so. |
| S4 | should-fix | `docs/PROMPT.md` (The change), `docs/retros/steer12-impl-notes.md` section 2 | **False premise: "sessions that already ran with the flag on".** All six BEFORE sessions are `provider: opencode-go`, and `src/provider.rs:209` sends `parallel_tool_calls` only for `ProviderKind::OpenRouter`. Verified on the real binary with a logging proxy (bodies only): opencode-go + default model -> field **absent**; OpenRouter + `deepseek/deepseek-v4.1-flash-1017` or `deepseek/deepseek-v4.1-flash`, clean state dir (no model cache) -> `true` on the first request; OpenRouter + unknown model `acme/mystery-1` -> absent. So the 1.04 calls/step figure says nothing about whether the flag "bought nothing". (On OpenRouter with the flag, my A/B model issued 2-call steps in 10 of 25 steps under the old prompt.) The prompt makes no parallel claim, so it is truthful; the rationale in the docs is not. Brief item 3 ("must mark the default model capable") holds only on OpenRouter; on Go it is never sent. Record that in DEVIATIONS or PROMPT.md. |
| N1 | nit | `src/prompt.md:17` | `<8 KB` is ambiguous (command size or output size) and obsolete: on Windows a command always travels as a file (`src/shellcmd.rs`), and my 11 KB, 259-segment batch ran intact (18.8 KB output, exit 0). |
| N2 | nit | `src/prompt.md:12-13` | "a notes file" names no location. `edit` paths are relative to the repo cwd, so the default is the repo root; `.gitignore` only has `/scratch`. See section 3. |
| N3 | nit | `scripts/metrics/trajectory.py:188-210` | `powershell -Command "Get-Content a.txt"` (the double-quoted payload is one word, quotes kept) is not classed a read; `bash -c 'cat f'` is not a read; a `\"` inside a double-quoted grep pattern breaks the quote walk (a call that also ran `cargo build` counted as a read). Failing test: `a_quoted_powershell_read_is_a_read_call` (ignored). |
| N4 | nit | `docs/PROMPT.md` | The brief's "271 steps with >= 2k tokens, 180 big->cheap" reproduces at **>= 1000** completion tokens (my count: 272 big steps), not 2000 (152). The doc explains the 74% -> 42% gap by the classifier only; the threshold is the larger part. Say so, so nobody re-derives a 2k figure from the brief. Also "commits: n/a (worktrees gone)": commits are recoverable from `main` history by subject prefix and time window (about 55 commits, ~8.6 model min/commit, rough). `n/a` is defensible, not necessary. |

## 1. BEFORE numbers, recomputed independently

Own script `scripts/live/steer12_recompute.py` (does not import `scripts/metrics`) over the six named sessions
(`steer1fix`: only `ses_01m7s6mkp9...`). Model time = `assistant.ts - step_started.ts`; tool time = union of
`tool_started..tool_settled`; steps = `step_started`; `usage` rows carrying a `kind` ignored.

| metric | doc | mine | note |
|---|---|---|---|
| steps | 1727 | 1727 | 3 started steps never got an `assistant` (killed/failed requests): counted as steps, no model time |
| tool calls | 1800 | 1800 | |
| calls/step | 1.04 | 1.042 | 76 steps have >= 2 calls |
| read calls | 711 | **708** | the doc counts `cat > f <<EOF` x3 and `git show X:f > f` as reads (S2); I count one quoted grep it misses |
| read share | 39.5% | 39.3% | |
| model min | 470.9 | 470.9 | |
| tool min | 371.3 | 371.3 | |
| model share of model+tool | 55.9% | 55.9% | |
| big steps (>=2000) | 152 | 152 | |
| big -> cheap | 64 (42.1%) | 64 (42.1%) | |
| big -> cheap model min / share of model | 149.6 / 31.8% | 149.6 / 31.8% | |
| prompt tokens/step | 181,590 | 181,591 | |
| wall | 14.18 h | 14.18 h | first to last event per session |
| model min per landed commit | n/a | ~8.6 (rough) | commits on `main` by subject prefix inside each session window: buildinfrafix 1, steer1fix 0, steer3 0, steer5 14, tui2 28, tui2fix 12 = 55; sensitive to merges and prefixes, order of magnitude only |

The doc's table reproduces. The only definition differences are the classifier (S2, N3) and the brief's threshold (N4).
Sensitivity of big->cheap to the token floor (my script): >=500: 117 of 494, 35.0% of model time; >=1000: 87 of 272, 33.6%;
>=2000: 64 of 152, 31.8%; >=3000: 57 of 120, 30.8%. The brief's 74% of model time is not reproduced at any floor; the
share is about a third of model time under every definition I tried (the brief's "~50% of wall time" likewise).

Read classifier on tricky commands (34 cases, run against `trajectory.is_read_call`):
* correct: quoted pipe `grep -n 'a|b'` plus labelled `sed`; `cargo build 2>&1 | tail`; `cargo test | grep`; `git grep -n`;
  `cd X && cat f`; `cd X && cat f && cargo build`; heredoc `cat > f <<'EOF'` and `cat <<'EOF' > f` when run as a unit string
  (the three heredoc writes in the data are the exceptions, because the word split stops at the first newline segment); `ls; rm -rf target`; `git log && git push`; `cat a | tee b`; `powershell -Command "Get-Content a | Set-Content b"`.
* reports read, truth is write: `sed -i`, `echo hi > f; cat f`, `sed -n 1,5p f > out`, `awk ... > g`, `find -delete`, `git branch -D x`, `git config user.name x`, `sleep 5`, `echo x >> notes.md`, `grep "a\"b" f; cargo build`.
* reports non-read, truth is read: `powershell -Command "Get-Content a.txt"`, `bash -c 'cat f'`, `for f in src/*.rs; do wc -l $f; done`.

## 2. The prompt

* `src/prompt.md` is 1018 bytes (<= 1024), pinned by `tests/fixtures/prompt.md.snapshot` and the rule list in `src/sources.rs`; the whole
  suite passes, including `review_steer6` F1 (the `Directories:` cut).
* **Truthful:** no parallel claim anywhere. Capability gate verified on the real binary (S4).
* **Does "one bash call carries every read" cause harm?**
  * Exit codes (canned server, real tool): `echo === a; sed -n 1,5p nonexistent.rs; echo === b; ls | head -3; echo === c; grep zzz AGENTS.md; echo === d; head -2 Cargo.toml` -> exit **0**; the `sed: can't read` line is in the merged output. A batch's exit code is the last command's, so "check exit codes" does not protect a batched read; the failure is only visible as text.
  * Spill: S1. A `grep` with no match prints nothing, so an empty section looks the same as a section cut by the spill.
  * 8 KB: no effect on Windows (N1).
  * Windows quoting: batches with single quotes, `\|`, `;` and `echo ===` ran byte-exact through the Git-bash file route. If `shell` is cmd.exe or PowerShell the advice (`;`, `sed`, `grep`) does not apply and the prompt does not name the shell. Nit, not a regression (the old text also said `sed -n` / `git grep`).
* **A/B on the real model** (`deepseek/deepseek-v4.1-flash-1017`, OpenRouter, Herdr pane `live-steer12-ab`; 3 runs x 2 tasks x 2 arms;
  OLD = `git show b7b60c6:src/prompt.md` via config `prompt =`, same repo clone, fresh state dir per run; scripts `steer12_ab.sh`, `steer12_abstats.py`).
  The tasks were short (2-6 steps), so this measures the nudge, not trajectory-scale effects.

| task / arm | steps (3 runs) | tool calls | read calls | 2-call steps | prompt tokens (sum) | completion tokens (sum) |
|---|---|---|---|---|---|---|
| T1 gate lock, OLD | 4,5,3 | 14 | 14 | 5 | 65,077 | 1,998 |
| T1 gate lock, NEW | 3,2,3 | 7 | 7 | 2 | 38,143 | 1,154 |
| T2 retries, OLD | 5,4,4 | 15 | 15 | 5 | 80,636 | 1,663 |
| T2 retries, NEW | 4,6,4 | 11 | 11 | 0 | 77,424 | 1,657 |
| all, OLD | 25 | 29 | 29 | 10 | 145,713 | 3,661 |
| all, NEW | 22 | 18 | 18 | 2 | 115,567 | 2,811 |

  Reading: tool and read calls drop ~38% (T1 -50%, T2 -27%), steps -12% overall but T2 is flat (4.3 -> 4.7), prompt tokens -21%. The mechanism is what the
  brief wants on T1 (labelled `;` batches: 1-4 calls instead of 3-6) and the old arm's parallel 2-call steps nearly vanish. n=3 per cell: directional, not
  significant. Quality: NEW T1 run 2 answered after a single grep pass and never opened `scripts/gate-lock.sh` (plausible answer from doc lines; the OLD run opened the
  script and said "verified by reading"); NEW T2 run 2 still wasted a call on a bad pathspec (`'src/**/*.rs'` -> 0 bytes), so batching did not prevent that. No run
  spilled (largest output 8.6 KB), so S1 is shown by the canned drive and the log statistics, not by the A/B.

## 3. The externalised-notes rule

The prompt says "a notes file with `edit` first" and gives no path. `edit` resolves relative paths against the session cwd (the repo) and `.gitignore` ignores only
`/scratch`. Live test (`steer12_notes.sh`, pane `live-steer12-notes`, NEW prompt, 3 fresh clones, a task that invites a plan and ends with "Commit the result"):
0 of 3 left an untracked file or committed a notes file (`git status` clean, the commit touched only `src/spill.rs`). Runs 1 and 2 wrote no notes; run 3 wrote
`plan-rename-head-bytes.md` into the **parent of the repo** on its own initiative. No pollution observed, but it rests on the model's choice on a short task; a longer
task that plans more and runs `git add -A` could commit one (N2). Suggested: name a place that is never committed, e.g. the session dir
(`$KELIR_SESSION_DIR` is set for bash children) or `.kelir/notes.md` with a `.gitignore` entry.

## Acceptance lines of the brief

| brief item | status |
|---|---|
| 1. trajectory script, BEFORE table with exact command | done; reproduces (S2, S3, N4 caveats) |
| 2. prompt <= 1 KB, byte snapshot updated deliberately, shell-level batching + notes file + guardrails | done; S1: the guardrails lack an output budget |
| 3. verify the `parallel_tool_calls` gate for the default model | OpenRouter yes; **never sent on opencode-go** (S4); prompt makes no claim |
| 4. AFTER numbers | deliberately empty and the doc says why; when filled, compare same-provider sessions (S3, S4) |
| tests: metrics fixture, prompt snapshot, gate + Linux | present and green |

## Files added by this review

`tests/review_steer12.rs` (1 passing, 2 ignored finding tests), `scripts/live/steer12_recompute.py`, `steer12_ab.sh`, `steer12_abstats.py`, `steer12_notes.sh`.
