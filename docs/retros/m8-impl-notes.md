# M8 implementation notes (gates, `delegation` and `herdr` skills)

Branch `job/m8`. What was built, what was decided, what to look at.

## Built

- `src/gates.rs`: declarations (`[gates]` toml, skill `kelir-gate` metadata), the
  runner (shell route via `shellcmd`, env allow-list, Job Object / process group,
  30 s timeout, 1 MiB output cap, fail open), decision parsing, `apply_rewrite`,
  `describe` for `observe`.
- `src/session.rs` hooks, all behind `self.gates: Option<Arc<Runner>>` (`None` = no cost):
  `session_start`/`session_end` in `run()`; `prompt_submit` in `apply_pending` (Say),
  `record_messages` and the held-back command-line task (`submit_task` / `start_gates`);
  `before_tool`/`after_tool` in `run_tools`; `turn_end` in `drive_turn` at the
  no-tool-calls branch; MCP calls through `gated_mcp_call` (broker task).
- `EventBody::Gate`, `ToolOutcome::denied`, `tools::denied_text`, `envs` (`KELIR_GATE_EVENT`,
  `KELIR_GATE_NAME`, test hook `KELIR_GATE_TIMEOUT_MS`), `mcp::rpc::inherited` made `pub(crate)`.
- `observe`: `== gates ==` and `== gates fired ==`.
- `examples/gates/`: `verify_before_claim.py`, `note_check.py`, `git_sync.py`, `numbered_answers.py`.
- `sources.rs`: `BUILTIN_SKILLS` (kelir, delegation, herdr); `src/docs/skill-delegation.md`,
  `skill-herdr.md`.
- Docs: `docs/reference/gates.md` (new topic), config/env/events (generated), files,
  sources-and-skills, troubleshooting, source-map, README; DEVIATIONS; SPEC note.

## Decisions worth a second look

1. **Fail open includes "non-zero exit with a decision on stdout".** A gate cannot veto by crashing.
2. **Windows job names must be unique per gate run.** First version named jobs `session+counter`; two
   runners in one process (the unit tests) got the *same* kernel object back from `CreateJobObjectW`
   and killed each other. Now a process-wide counter plus pid.
3. **`prompt_submit` holds back the command-line task** until `session_start` gates ran, so the
   `user` event is written after the gate decided (a drop writes no `user` event at all).
   Only when a prompt gate exists; otherwise the old path is unchanged.
4. **MCP gate events are logged at settle time** (the gate ran on the broker task, only the loop
   writes the log); the `gate_runs` ride in the internal settle verb (key-checked like the rest).
5. **Project-layer gates run without a trust prompt** (DEVIATIONS, same as project `mcp.toml`).
6. The `delegation` skill documents M7's CLI from the SPEC; M7 was not merged here.

## Tests

- Unit (`src/gates.rs`): decision parsing (allow/deny/rewrite/garbage/empty/huge), declarations,
  skill binding, event JSON shape, rewrite shapes, env allow-list, real-process chain semantics,
  crash/garbage/silence/missing, deny-means-nothing, timeout kills the tree, clean exit leaves no
  background child, chatty gate cannot wedge the pipe, `describe` never shows env values.
  `src/docs/tests.rs`: both builtin skills (valid, short, no private names, say what they must).
- E2E (`tests/e2e_m8.rs`, 20 tests, `Sandbox` Drop-guard): each of the six events with its document,
  fail-open (crash / garbage / silence), timeout within the limit and tree killed, hedged answer blocked
  once and the turn continues (the shipped example), second deny logged not enforced, turn_end
  rewrite, before_tool rewrite and deny (`denied by gate <name>: <reason>`, call did not run),
  after_tool note and replace, prompt_submit rewrite and drop (exit 1, nothing sent), `observe`,
  canary secrets over every event (env names and values, stdin, log, stdout, stderr), skill-bound gate,
  the four example gates, MCP `mcp:<server>.<tool>` rewrite/deny/after, a console-less (DETACHED_PROCESS)
  Windows session running gates.

## Not done / for the reviewer

- Live drive with a real cheap model and the example gates is the reviewer's layer.
- `delegation` flags are unverified against a built M7.
- the Linux gate result: see PROGRESS.

## Incident: ~1000 `sh .../Cargo.toml` processes (2026-11-20, 20:26-20:29)

What I believe happened: while debugging a Windows-only failure in the gate unit tests I ran, by
hand in Git Bash, `sh "<worktree>/Cargo.toml"` (a careless experiment: I wanted to see
whether `sh` accepts a Windows-style path and picked a file that was not a script). `sh` executed the manifest as a shell script; the call hit its 120 s limit and was moved to
the background. The times and the exact command line match the report. I cannot prove the fork growth
from the gate runner's side: it has no retry loop, and the gate tests had not started a `sh` on that
file. It is not a case I can rule out completely, so the runner is hardened anyway.

Lesson: never point an interpreter at a file to "see what happens", and never run a manual process
experiment without a count watchdog. Hardening done (and tested):

- A gate never triggers gates: a kelir started by a gate (`KELIR_GATE_EVENT` in its environment) runs none.
  E2E: a gate whose command starts kelir with the same config runs exactly once.
- At most 8 live gate processes per kelir process (`gates::MAX_LIVE`), the rest skipped, not retried.
  Unit: 24 runs piled up never exceed 8, the counter returns to 0.
- A gate that spawns itself (two children per level, three levels) is bounded and its whole tree is gone
  after the run (a flag file a descendant would write never appears).
- Every real-process unit test holds a guard that asserts the live counter is 0 when it ends; the e2e tests
  run under the `Sandbox` Job Object Drop guard, which fails the test on a leaked process.
- Spawn failure is one `gate` event with `error`; an empty command is rejected at config load.
- I ran the suites through a watchdog (`sh.exe` count, abort at +20) from then on.
