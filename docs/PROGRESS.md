# Progress

| Milestone | Date | Commit | Tests (unit/e2e) | Live drive | Known gaps |
|---|---|---|---|---|---|
| M0 | 2026-11-18 | f0e891b | 92 / 13 | pending reviewer | Live drive not run by the implementer (no real provider calls). Go's reasoning-effort field still unverified; M0 sends no reasoning parameter. `stop`/`resume`/`bg` are still M1-M4, so an interrupted turn leaves started-but-unsettled calls for the M3 resume path. Background children of a `bash` call are killed when the call ends (no `bg` yet). |
