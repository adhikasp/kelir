# notify — hooks and terminal records when work ends

**Summary:** `[notify]` runs shell hooks and/or emits an OSC terminal record on `turn_done`,
`error`, `paused`, `bg job settled` and `child settled` — nothing is enabled by default.

**Read this when:** you want a desktop toast when a long turn finishes, or a hook on a background
job's completion.

`src/notify.rs`, `src/config.rs`, `examples/notify/`.

## Config

```toml
[notify]
turn_done = "notify-send kelir: done"
error = ["notify-send kelir: failed", "my-error-hook"]
paused = "my-paused-hook"
# A settled background job or child takes the same command or list of commands.
terminal = true          # default false: emit an OSC record on a TTY
terminal_kind = "osc9"   # or "osc777" for kitty / rxvt
rate_limit = 5           # seconds between two records of the same kind; default 0
title = "kelir"          # prefix of the terminal message
```

A scheduled run with `--single-flight` writes a STATUS file (`src/namedrun.rs`; see `docs/reference/jobs.md`) when the run ends. It is not a `notify` hook and is not fired by the five event kinds
above. To get a hook on a settled job or child, use the `bg_settled` or `child_settled` key in `[notify]` in `config.toml`.
Hooks run through the
configured shell with a **scrubbed environment** (the platform allow-list, plus `KELIR_NOTIFY_EVENT`
and `KELIR_NOTIFY_MESSAGE`), at most 30 s (`KELIR_NOTIFY_TIMEOUT_MS` shortens it in tests), with
the whole process tree killed on exit. They never hold the session: each runs in a detached task
and its failure is recorded, never raised to the model or the turn's exit code. A burst of the same
kind is rate-limited; a `note{kind:"notify"}` event records the hook name and its detail: `<hook event>: <N> hook(s) fired` on success, or `<hook event>: <error>` when a hook failed or was rate-limited.

## Terminal records

A record is emitted only when stdout is a TTY and `terminal = true`. `osc9` is
`ESC ] 9 ; <message> BEL` (iTerm2, Ghostty, WezTerm); `osc777` is
`ESC ] 777 ; notify ; <message> BEL` (kitty, rxvt). Control bytes in the message are folded to
spaces so a hook cannot inject a second escape sequence. `kelir run --json` never emits a record.

## Examples

`examples/notify/windows-toast.ps1` (PowerShell desktop toast),
`examples/notify/linux-notify-send.sh` and `examples/notify/macos-osascript.sh` are generic
starters; copy one and adapt it. Keys and private paths never appear in a hook command — the
environment is scrubbed.
