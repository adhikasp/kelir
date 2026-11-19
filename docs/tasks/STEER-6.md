# STEER-6 — retro-driven improvements to kelir's own loop (from docs/retros/m5a.md)

First output of the introspective pass: kelir reading its own M5-A trajectory (320 steps, 113 min).
Read **docs/retros/m5a.md in full** (numbers, evidence with event seqs, exact proposals); implement its
cheap, high-value items. Files: `src/bg.rs`, `src/tools/bash.rs`, `src/tools/edit.rs`, `src/provider.rs`,
`src/config.rs`, `src/compact.rs`, `src/prompt.md` (base prompt, keep <= 1 KB or justify), tool descriptions.

## Build (in priority order)
1. **F2 — silent 8,191-char command cut** (value highest): on Windows write the command to
   `<call dir>/cmd.sh` and run `bash cmd.sh` (and `cmd.bat` for the cmd.exe fallback) when the command is
   longer than 6000 chars (or always); notice in `bash.rs` when the output has `delimited by end-of-file`.
   Regression test: a 40 KB heredoc writes a file and prints its line count (Windows + Linux).
2. **F1 — design thrash**: one prompt line ("Plan once, briefly; then act. Do not redesign after each tool
   result."), and an optional per-provider/model `reasoning_effort` knob (config + `build_request`; default
   `medium`; recorded in `session_started`). Verify live against the OpenCode Go gateway whether it is honoured
   (try the OpenRouter-style `reasoning: {effort}` form as the fallback); mock tests for the request body.
   The "replay previous step's reasoning" idea stays an experiment: do NOT make it a default.
3. **F3 — edits**: extend the `edit` tool with an `edits: [{old,new}]` multi-edit (atomic: all or none, in
   order, each exactly-once unless replace_all) and a prompt line preferring `edit` over python/sed heredoc
   patches; make the failure mode loud (a patch script that asserts must not look like success: say so in the
   prompt: check the exit code of each command, do not chain `python patch && cargo ...`).
4. **F4 — batching**: prompt line telling the model to issue independent read-only calls in one message
   (parallel calls are supported); no code change.
5. **F5 — earlier compaction**: configurable soft target `compact_at_tokens` (default 150000 or 25% of the window,
   whichever is smaller) in addition to the 80% trigger; tests for the arithmetic and an e2e.
6. **F6/F7**: prompt lines: verify before claiming ("a report must name the command that proved it"), and search
   hygiene (exclude `target/`, prefer `rg` when present, else `grep -rn --include`).
7. Re-measure: a before/after note in the retro file (a short scripted benchmark on the mock is enough).

## Tests and gates
Unit + e2e per item (mock server); prompt changes get a snapshot test of the rendered system text (deterministic
bytes). Windows + Linux gates, no leaked processes, explicit-path commits (AGENTS.md).
