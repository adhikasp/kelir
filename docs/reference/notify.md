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
# bg_settled and child_settled take the same command or list of commands.
terminal = true          # default false: emit an OSC record on a TTY
terminal_kind = "osc9"   # or "osc777" for kitty / rxvt
rate_limit = 5           # seconds between two records of the same kind; default 0
title = "kelir"          # prefix of the terminal message
```

A per-job `notify` in `jobs.toml` is added to the global hook for that kind. Hooks run through the
configured shell with a **scrubbed environment** (the platform allow-list, plus `KELIR_NOTIFY_EVENT`
and `KELIR_NOTIFY_MESSAGE`), at most 30 s (`KELIR_NOTIFY_TIMEOUT_MS` shortens it in tests), with
the whole process tree killed on exit. They never hold the session: each runs in a detached task
and its failure is recorded, never raised to the model or the turn's exit code. A burst of the same
kind is rate-limited; the outcome is written as a `notify` event (`ok`, `rate_limited`, `failed`,
`no_hooks`).

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
