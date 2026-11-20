# steer12fix: implementer notes

Fixes for `docs/reviews/steer12-review.md` (PASS WITH FIXES: S1-S4, N1-N4).

| id | what changed | where |
|---|---|---|
| S1 | A spilled output now lists every `=== label` section with its byte offset (one line, <= 40 labels), built while the output streams; the prompt also states the budget ("under 10 KB (`head`)"). Tested with a 40 KB, four-section batch: the model sees the first KB, the last KB and `spec@0, deviations@10010, grep@20026, tail@30036`. A header split across two pushes and an unterminated last header are covered; a plain spill has no extra line. | `src/spill.rs`, `docs/DEVIATIONS.md` |
| S2, N3 | The classifier no longer counts writes as reads: file redirects, `sed -i`, `find -delete/-exec`, `sort -o`, `git branch -D`/`config k v`/`remote add`, `sleep`, heredoc writes. It looks into `bash -c`, `powershell -Command "..."` payloads and `for ... do ... done`, drops heredoc bodies, and honours `\"` in quotes. The reviewer's two ignored tests are now plain tests with more cases (and no longer share one temp dir by pid, which made them race when run in parallel). Read total stays 711 (two sessions moved by two calls each). | `scripts/metrics/trajectory.py`, `tests/review_steer12.rs` |
| S3 | `--by-prompt` groups sessions by the prompt in `session_started.system.text`; the BEFORE table is split into `batch-reads` (1 session), `pre-m5` (3), `no-batching` (2) and says only the first is the replaced prompt. | `scripts/metrics/trajectory.py`, `docs/PROMPT.md` |
| N4 | The brief's 271/180 counts are the >= 1000 completion-token population (272 here); 2000 is the default, with both rows in the doc. Commits: stays `n/a` with the by-hand recovery described. | `docs/PROMPT.md` |
| S4 | `parallel_tool_calls` is OpenRouter-only; recorded in DEVIATIONS and PROMPT.md, the retro's wrong sentence is corrected. Not enabled for opencode-go (nothing shows the gateway honours it); the prompt never claimed it. | docs |
| N1 | `<8 KB` dropped; replaced by an output budget. | `src/prompt.md` |
| N2 | Notes go to `.kelir/notes.md`, "uncommitted"; `/.kelir/notes.md` added to `.gitignore`. | `src/prompt.md`, `.gitignore` |

Prompt: 1020 bytes (limit 1024). To fit the budget line and the path, "briefly", "no redesign",
"never `cat` a big file" (now "no big `cat`") and the `Batch reads` lead-in were cut; the retro rule
list in `src/sources.rs` and the byte snapshot follow the wording.

## A/B (OpenRouter cheap model, 3 runs x 2 tasks x 2 arms)

Tool calls 29 -> 20 (-31%), two-call parallel steps 9 -> 0, steps 26 -> 26, prompt tokens
-1%, completion tokens +7%. A call-count win, not a trajectory win, on tasks of 3-7 steps; n = 3,
not significant. Full table in `docs/PROMPT.md`. One NEW run spilled and the section line was
visible to the model.

## For the next worker

- The AFTER column in `docs/PROMPT.md` is still empty: run the first implement and fix jobs on the
  new prompt and compare against the `batch-reads` row, not only the all-six total.
- Writing escapes through the Bash tool in this environment eats backslashes in heredocs: edits with
  `\n` in Rust or Python source were mangled twice; the Edit tool is safe.
