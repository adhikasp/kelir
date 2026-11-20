# STEER-12 — fewer, bigger tool steps and an externalised plan (measured prompt change)

Evidence (2026-11-20, six live kelir sessions, events.jsonl only: 14.7 h wall, 1,725 model calls, 1,800 tool calls): model generation 676 min
(76.6% of wall), tools 210 min (23.8%), harness gaps 0.5%; inside tools: `cargo test` 127 min, gate 39, build/fmt 13, clippy 6, 1,079 read calls
only 9 min. Steps are single-call (1.02-1.46 tool calls/step; one session issues exactly one call in 686 of 744 steps) although the model can emit
2-call steps. Reads come in runs (56 runs, longest 63 steps); in those runs 270 of 325 adjacent pairs are independent (the 55 dependent ones are
`grep -n` then `sed -n` that one `grep -A/-B` would collapse). One session spent 110 min of model time issuing 439 reads that ~20-30 batched
commands would have covered. **180 of 271 steps with >= 2k output tokens (74% of model time, ~50% of all wall time) ended in ONLY a cheap read**:
re-orientation, not only re-planning (reasoning is dropped between steps).

## Build (base prompt + tooling; no other product behaviour change)
1. **Baseline first** (rule: no harness change without a before-number): write `scripts/metrics/trajectory.{js,py}` (PEP 723 / node, no deps) that
   computes from `events.jsonl` (one or many sessions): tool calls per step, read calls (sed/grep/cat/ls/head/tail/git grep/powershell reads) per
   landed commit, model time (assistant.ts - step_started.ts) vs tool time (union of tool_started -> tool_settled), the share of model time in
   "big -> cheap" steps (>= 2k completion tokens whose only calls are reads), steps per commit, model minutes per landed commit, prompt tokens per
   step. Run it over the state dirs under the kelir-wt state root for the six sessions named above and commit the numbers as the BEFORE table in
   `docs/PROMPT.md` (with the exact command).
2. **Prompt (src/prompt.md, stays <= 1 KB, byte-snapshot test updated deliberately)**: replace the current "batch independent reads" line with the
   shell-level move that always works: *one bash call carries every read it needs: several `sed -n`/`grep`/`ls`/`git grep` joined with `;`, each
   labelled `echo === name`; if you are about to issue three reads in a row you should have written one command; prefer `grep -A/-B` over `grep -n`
   then `sed -n`*. Add the externalised plan: *a step that needs real thinking writes its conclusion (plan, file:line map, decision) into a notes
   file with `edit` before acting; the next step reads the notes instead of re-reading the source.* Guardrails: batch reads, never batch a build/test
   with edits; commands stay under 8 KB; exit codes still checked.
3. **Verify the `parallel_tool_calls` capability gate** (STEER-6 S2): it must mark the default model (deepseek-v4.1-flash) capable. If it is not
   reliably on, drop the parallel-calls claim from the prompt and keep only the one-command batching rule; the prompt must stay true.
4. **After-numbers**: after landing, run the metrics script over the next one implement job and one fix job and add the AFTER table to
   `docs/PROMPT.md` plus the retro. Expected movement: read calls down 30-50%, big->cheap share from ~50% to under 35%, the same or better gate
   outcomes. If it does not move, say so and propose the next lever (a notes-file helper, a repo-map command).

## Tests and gates
Unit tests for the metrics script on a small fixture events.jsonl (known numbers); snapshot test of the rendered base prompt; `bash scripts/gate.sh
--linux=<machine from your prompt>`; no leaked processes. Commit `steer12: ...` (explicit paths); notes docs/retros/steer12-impl-notes.md; final
answer "steer12 ready: <commit>".
