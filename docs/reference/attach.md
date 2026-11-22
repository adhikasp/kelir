# attach — watch a session from another terminal

**Summary:** `kelir attach <session|name>` replays a session's event log through the same row
model as the TUI, then (with `--follow`) tails it, without writing to the session or owning a
daemon.

**Read this when:** you want to watch a long turn from a second pane, inspect a finished
session's transcript, or lightly steer a session with `say`/`stop`.

`src/attach.rs`, `src/tail.rs`.

## What it does

```
kelir attach <session|name> [--follow] [--since N] [--raw] [--plain] [--steer]
```

- `<session>` is an exact id, a unique prefix, `latest` (the newest session), or an M9 name.
- `--since N` replays only the last N events (`0`, the default, is the whole log).
- `--follow` keeps reading appended events and stops when the session ends or its owning
  process dies. The tail reader keeps a partial last line until the writer finishes it (torn-line
  tolerance), skips a line over 1 MB, and never loads the log whole.
- `--raw` prints the original NDJSON lines. `--plain` prints unstyled text for pipes. By default
  each log event is fed to the same `App` as `View::Log`: the state row, collapsed tool
  calls (including `bash` descriptions), markdown prose and outcome lines are the same. Human-only
  announcements use a synthetic note view, so they add no log records.
- The viewer is **read-only**: it never writes to the session dir, it opens no socket unless
  `--steer` is given, and disconnecting cannot affect the session. A bounded 4,000-row backlog
  keeps the working set under 10 MB even on a 100k-event log.
- `--steer` accepts `say <text>` and `stop` on stdin. They are sent over the session socket with
  the session's own token (the `kelir ctl` path); every other verb is refused. Without `--steer`
  no request is ever sent. `--steer` **requires `--follow`**: a replay-only viewer has no live
  stdin loop, so the combination is a usage error (exit 2) with a clear message, never a silent
  no-op.

## Herdr

`kelir attach` writes **no** OSC 7501 status record: it is a viewer, not an agent. The session it
watches reports its own `working`/`idle`/`done`/`error`/`blocked` status; `hc wait` on the pane
that runs the session sees that status, not the viewer's. Children (M7) never report status.

## Exit codes

`0` a clean replay (or a clean end of follow), `1` a read/steer failure, `2` a bad invocation
(no session, no log, ambiguous id).
