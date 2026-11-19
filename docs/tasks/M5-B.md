# M5-B — Herdr source line, hint, memory, attachments, observe

Prereq: M5-A landed and reviewed (sources + skills). Read docs/tasks/M5.md (the full M5 list; M5-A did items 1-3)
and SPEC *Sources, skills and memory*, *Prompt caching*.

Do the rest of M5 (pushed hint, memory, attachments, `observe`) **plus the Herdr awareness source**:
- If `HC_TERMINAL_ID` is set, render ONE line into the frozen system sources at session_start: pane address,
  space, tab and `where` label, from `hc get $HC_TERMINAL_ID --json` (labels `herdr.space`, `herdr.tab`,
  `where`). Fail open: `hc` missing, error, or > 1 s => omit the line (log a `source` note, never an error).
  Never put a live agent count in the prompt. Outside Herdr: no line. Recorded in `sources[]` like any source;
  `observe` shows it. No new tool; no env other than `HC_TERMINAL_ID` is required.
- Tests: unit (JSON shapes: labels missing, partial, malformed; fail-open paths); e2e with a fake `hc` on PATH
  (script returning crafted JSON, and one that hangs/errors); render determinism (same inputs => same bytes);
  windows (`hc.exe`/.cmd shim, no console window) and Linux.
- Live drive (reviewer): in a Herdr pane the frozen system contains the line with the right space/tab; outside
  Herdr it is absent; cache hit ratio unchanged.
Gates and verification rule as in AGENTS.md.
