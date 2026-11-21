# M11 implement notes — attach viewer and notifications

## What landed
- `src/tail.rs`: bounded chunked tail reader, torn-line tolerance, backward `--since N` scan,
  runaway-line cap (1 MB), rotation detection, 9 unit tests.
- `src/attach.rs`: bounded 4000-row `Viewer` reusing `tui::App` and `UiEvent`, event→row
  conversion using the same `call_head`/description model as the live TUI, replay + follow,
  `--raw` NDJSON, `--plain` text, token-gated `--steer` (`say`/`stop` only), 2 unit tests.
- `src/cli.rs` / `src/main.rs`: `Command::Attach` + `AttachArgs` with read-only default.
- `src/event.rs`: `EventBody::Notify` (kind, outcome, hooks, error) and `notify` event type.
- `src/notify.rs`: M11 `Config`, five KINDS, OSC 9/777 emitters with control-byte folding,
  per-(dir,kind) rate limit, `fire()` with scrubbed env and detached hooks, exit grace wait,
  6 unit tests.
- `src/config.rs`: `[notify]` table with event→command(s), terminal, terminal_kind, rate_limit,
  title; wrong event names are config errors.
- `src/session.rs` / `src/session/children.rs`: notify on turn_done/error/paused, bg_settled
  and child_settled; one `notify` event per dispatch.
- `examples/notify/`: generic PowerShell toast, Linux `notify-send`, macOS `osascript`.
- `docs/reference/attach.md`, `docs/reference/notify.md`, regenerated cli/config/env/events,
  drift tests green (24 docs tests).
- `tests/e2e_m11.rs`: 4 real-process tests (finished-session replay with directory-snapshot
  read-only check, `--raw`, unknown session, notify hook firing once with env), all in
  `TreeGuard`; 757 lib tests pass.

## Known gaps
- Footprint measured only on Windows dev box in units (bounded 4000 rows); no 100k-event
  RSS figure captured yet on either OS.
- Per-job `notify` is the M9 list-at-run-end, not an M11 five-kind table; documented in
  `docs/DEVIATIONS.md`.
- Live Herdr drive (`hc wait`/`hc get` with real panes) not yet run by implementer; reviewer
  to verify.
- Attach viewer has no inline viewport for interactive steering; `--steer` reads commands
  from stdin and prints rows to normal scrollback. Documented.
