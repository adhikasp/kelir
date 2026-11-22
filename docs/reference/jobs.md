# Named and scheduled runs

**Summary:** `kelir run --name`, `--single-flight`, `--timeout`, `--task-file`,
`--fresh`, `kelir ls sessions`, the name lock, `STATUS.json`, exit codes and
recipes for cron, a systemd timer and Windows Task Scheduler.
**Read this when:** you want a scheduler to run a task in a named session, you
are debugging a run that skipped (exit 75) or timed out (exit 124), or you read
`<state>/names/<name>.status.json` from a script (`src/namedrun.rs`,
`src/names.rs`, `src/joblock.rs`).

kelir has no daemon, no scheduler and no job configuration file. A scheduled
run is just `kelir run --name <name>` with scheduling flags; something else
(cron, a timer, CI) decides when to call it. Runs with `--name` reuse one
session so the model keeps its context, and `--single-flight` makes overlapping
runs skip instead of fork the log.

## Named sessions

```text
kelir run --name <name> [--single-flight] [--timeout D] [--task-file PATH] [--fresh] "task"
kelir ls sessions [--json] [--named] [--all]
kelir ls rename <old> <new>
kelir ls forget <name>
```

- A name is `[a-z0-9._-]{1,64}`, starts with a letter or digit, has no trailing
  dot, and is not a Windows device name. It may not end in `.status.json` or
  `.lock`, or contain `.stale.`: those suffixes belong to the scheduled run's
  STATUS, lock and takeover-tombstone files in the same directory.
- `names/<name>` in the state dir holds `{"session", "created", "updated"}`.
  It is published create-or-fail, so two processes cannot both bind one name.
  An unreadable name file reads as free and is replaced by the next run.
- A name that already has a session is **resumed**: log, model, tools, system
  prompt and cwd come from the log; the new task is a `user` event with
  `from: "job"`. The first task of a new named session has `from: "cli"`.
- A name whose session is running is refused, naming the holder's pid from the
  session's `owner` file (pid plus start time, so a recycled pid is not a
  holder).
- `kelir ls sessions` lists named sessions and the newest unnamed roots: name, id,
  state, last activity, tokens and cache-hit ratio, and (for a named run) `outcome`. `--json` adds
  `model`, `steps` and `pid`.

## Flags

| flag | effect |
|---|---|
| `--name NAME` | create or resume the session bound to `NAME` |
| `--single-flight` | take `names/<name>.lock`, write STATUS, skip with 75 if held |
| `--timeout D` | wall-clock deadline (`90`, `90s`, `30m`, `2h`, `1d`, `1w`); stop the turn, write `timeout`, exit 124 |
| `--task-file PATH` | read the task from a file instead of the positional words (or stdin) |
| `--fresh` | rebind the name to a new session; the previous session's last summary or answer is added as a `carry` source |
| `--max-steps N` | cap the turn at N steps |

`--json` works on a scheduled run too: the event log streams as NDJSON and
ends with a `result` event. The plain final-answer line is suppressed in that
mode; STATUS still records the answer from the log.

`--fresh` is for a schedule that wants a weekly or monthly session; the old
log stays available. `--timeout` requires `--single-flight`. Durations are
case-insensitive and accept `s`, `m`, `h`, `d`, `w`; a bare number means
seconds.

## Single flight

One lock per name: `names/<name>.lock`, containing `{token, pid, start, cmd,
started}`. It is published atomically with its record already inside: an
ownerless lock is never observed, and two runs cannot both publish. Identity is
pid plus process start time, so a recycled pid is not a live holder.

- A second run exits **75** at once and writes a `skipped` STATUS naming the
  holder. It never touches the holder's STATUS and never starts a second turn.
- A dead holder (kill -9, reboot) is taken over only after the same verdict on
  two looks 150 ms apart, never for a lock younger than 1 s, and only when the
  moved-aside record is the one judged. An unreadable lock is respected until
  it is 30 s old. Only the run whose token the lock carries can release it.

## STATUS

With `--single-flight`, the run writes `names/<name>.status.json` atomically at
start (`outcome: "running"`) and at the end. It is rewritten by every run;
there is no run history file.

| field | meaning |
|---|---|
| `name`, `session` | the bound name and session id |
| `started`, `finished` | RFC 3339; `finished` is `null` while running |
| `outcome` | `running`, then `done`, `paused`, `stopped`, `error`, `timeout` or `skipped` |
| `exit_code` | the process exit code |
| `steps`, `tokens`, `cache_hit_ratio` | this run's numbers |
| `answer` | the first lines of the final answer |
| `error` | why the run failed, skipped or timed out |
| `pid` | the process id |

Exit codes: `0` done or paused, `1` error/stopped, `2` usage, `75` skipped,
`124` timeout. A paused run (a step cap) is meant to be continued by the next
scheduled call.

## Date changes

The system prompt is frozen at session start and holds the date, never the
time. When a session is resumed on a later UTC day, a `source` event (kind
`clock`, "it is now <date>") is appended at the tail before the task. The same
day adds nothing and the cached prefix is untouched. `KELIR_TODAY=YYYY-MM-DD`
replaces the clock (used by tests).

## Recipes

Use absolute paths; a scheduler's environment is nearly empty, so put the
provider key in the unit/task environment or in `config.toml`'s `key_env`.
The name and paths below are examples.

**cron** (every night at 03:00; an overlapping run skips):

```text
0 3 * * * OPENROUTER_API_KEY=… /usr/local/bin/kelir run --name nightly --single-flight --timeout 30m --task-file /etc/kelir/nightly.txt
```

**systemd timer:**

```ini
[Service]
Type=oneshot
EnvironmentFile=%h/.config/kelir/env
ExecStart=/usr/local/bin/kelir run --name nightly --single-flight --timeout 30m "check the build"
# exit 75 means "skipped, another run holds the lock": not a failure
SuccessExitStatus=75

[Timer]
OnCalendar=*-*-* 03:00:00
Persistent=true
[Install]
WantedBy=timers.target
```

**Windows Task Scheduler:**

```text
schtasks /create /tn "kelir nightly" /sc daily /st 03:00 /tr "C:\tools\kelir.exe run --name nightly --single-flight --timeout 30m check the build"
```

Use the Task Scheduler's "Run whether user is logged on or not" option for a
window-free run; kelir creates no console windows of its own.

Read the result from anywhere with `<state>/names/nightly.status.json` or
`kelir ls sessions --json`.
