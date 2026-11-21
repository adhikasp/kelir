# Jobs and named sessions

**Summary:** `kelir run --name`, `kelir sessions`, `kelir job run|status|history`,
`jobs.toml`, the single-flight lock, `STATUS.json`, exit codes, and recipes for cron,
a systemd timer and Windows Task Scheduler.
**Read this when:** you want kelir to do a task on a schedule and remember what it did
last time, you are debugging a job that skipped (exit 75) or timed out (exit 124), or
you are reading `STATUS.json` from a script (`src/names.rs`, `src/jobs.rs`,
`src/jobrun.rs`, `src/joblock.rs`, `src/notify.rs`).

kelir has no daemon and no scheduler. A **job** is a command (`kelir job run <name>`)
that something else calls on a schedule: cron, a systemd timer, Windows Task
Scheduler, a CI cron, a button. Each run resumes the same named session, so the model
keeps its context between runs; no run overlaps another; and every run leaves a
status a person and a script can read.

## Named sessions

```text
kelir run --name <name> "task"      # create the session, or resume it with this task
kelir sessions ls [--json] [--named] [--all]
kelir sessions rename <old> <new>
kelir sessions forget <name>        # drops the name; the session and its log stay
```

- A name is `[a-z0-9._-]{1,64}`, starts letter/digit (no hidden dot-files), no trailing
  dot, not a Windows device name (`con`, `nul`, `com1`, …).
- `names/<name>` in the state dir holds `{"session", "created", "updated"}`. It is
  published create-or-fail and replaced by rename, so two processes naming one session
  cannot both win and a reader never sees half a file. An unreadable name file is a
  warning and reads as free; the next run replaces it. The directory is owner-only.
- A name is bound before start; a crash leaves a `new` name (`sessions ls`) that the next
  `run --name` resumes.
- `run --name` on a name that exists **resumes** that session (the same resume as
  `kelir resume`: the model, tools, system prompt and cwd come from the log) and adds the
  task as a `user` event with `from: "job"`. The first task of a new named session is
  `from: "cli"`.
- A name whose session is running is refused, naming the holder's pid. The pid comes from
  the session directory's `owner` file (pid + start time, so a reused pid is not a holder).
- `sessions ls` shows the named sessions and the newest unnamed roots: name, id, state
  (`running`, `done`, `stopped`, `paused`, `error`, `crashed`, `new`), last activity and
  tokens and cache-hit ratio. `--json` is an array of objects with the same fields plus `model`, `steps`, `pid`.

## `jobs.toml`

Read from `<config dir>/jobs.toml`, then the nearest `.kelir/jobs.toml` walking up from
the cwd; a job declared in both is the project's. Unknown keys are an error (exit 2).

```toml
[jobs.nightly]
task = "Check the build and write what changed to notes/changes.md"   # or task_file
# task_file = "nightly.md"          # relative to this jobs.toml
provider = "openrouter"             # optional; applies when a session STARTS
model = "some/model"                # optional; a resumed session keeps its own
max_steps = 40                      # optional; pauses the run at N steps
timeout = "30m"                     # wall clock for the run; "90", "90s", "30m", "12h", "7d"
session = "nightly"                 # the named session every run resumes (default: the job name)
new_session_every = "7d"            # "never" (default) or a duration
notify = "my-notify-command"        # one command, or a list; runs when the run ends
schedule = "every night at 03:00"   # for humans: kelir does not schedule
cwd = "."                           # relative to this jobs.toml (default: the caller's cwd)
```

- **`max_steps`** caps a job run's steps (flag wins). Unlike an interactive turn (answer
  on the cap-th step is `done`, STEER-4), a job reaching its cap is `paused`: the next run
  resumes it. Unset or `0` = no cap.
- **`new_session_every`** starts a fresh session (the name is moved to it; the old session
  stays) when the old one is older than the duration **or** its last request filled 90 % of
  the window (compaction normally keeps it below that). The new session's first context
  carries the old session's last compaction summary, or its last answer, as one `carry`
  source. `never` keeps one session for ever; compaction bounds it.
- **`timeout`** is the wall clock of the run (from the start of the turn, not the lock).
  At the deadline the turn is stopped (running tool calls die with their whole tree), the
  session ends, STATUS says `timeout` and the exit code is 124. If the loop has not ended
  20 s later the process is forced out after STATUS is written.
- **`notify`** commands run through the configured `shell` when the run ends (`done`,
  `error`, `stopped`, `timeout`, `paused`), with the lock still held. Like a gate: a
  scrubbed environment (the platform allow-list; never a provider key, `KELIR_TOKEN` or
  `KELIR_SOCK`), 30 s, the hook's whole tree killed when it ends, and **never a change to
  the exit code**; a failing hook is a line in `STATUS.json` (`notify_errors`). It gets
  `KELIR_JOB`, `KELIR_JOB_OUTCOME`, `KELIR_JOB_RUN`, `KELIR_JOB_SESSION` and
  `KELIR_JOB_STATUS` (the path of `STATUS.json`). Hooks declared in a **project's**
  `jobs.toml` run only when the project is trusted (`trusted_projects` in the machine
  config, or `KELIR_TRUST_PROJECT=1`), as project gates do; the machine file's always run.

## `kelir job run <name>`

```text
kelir job run <name> [--wait SECONDS] [--model M] [--provider P] [--cwd DIR] [--config FILE] [--max-steps N]
```

The flags win over the job. The final answer is printed on stdout.

| exit | meaning |
|---|---|
| 0 | `done` (or `paused`: a step cap or an unavailable provider; STATUS says so) |
| 1 | `error`, or `stopped` by a `stop`/`end` verb or a signal |
| 2 | usage: unknown job, bad `jobs.toml`, bad name, no key |
| 75 | skipped: another run of this job holds the lock |
| 124 | `timeout` |

### Single flight

One lock per job: `jobs/<name>/lock`, a file holding `{token, pid, start, cmd, started}`.

- Taken atomically with its record already inside (a temp file linked into place): an
  ownerless lock is never observed, and two runs cannot both publish.
- Identity is **pid plus process start time**, so a recycled pid is not a live holder.
- A second `job run` exits **75** at once (`--wait N` queues for up to N seconds) and
  appends a `skipped` record to `runs.jsonl`; it never touches the holder's `STATUS.json`
  and never starts a second turn.
- A holder that is provably gone (`kill -9`, a reboot) frees the lock: the next run takes
  it over, but only after the same verdict held on two looks 150 ms apart, never from a
  lock younger than 1 s, and the moved-aside record must be the one that was judged
  (otherwise it is handed back untouched). A lock is released only by the run whose token
  it carries. An unreadable lock file is respected until it is 30 s old.
- The next run records the dead one in `runs.jsonl` (`outcome: "error"`, "disappeared").

## STATUS

`jobs/<name>/` in the state dir:

```text
lock             # the single-flight lock (only while a run is in flight)
STATUS.json      # machine: written at run start (outcome "running") and at the end, atomically
STATUS.md        # human, short
runs.jsonl       # append-only: one line per run, skipped runs included
notes/           # the model's own: it may read and write here with bash
notify/          # the scripts of notify hooks
```

`STATUS.json`:

| field | meaning |
|---|---|
| `job`, `run` | the job and this run's id (`run_…`) |
| `session`, `session_name` | the session id and the name it is bound to |
| `started`, `finished` | RFC 3339; `finished` is `null` while running |
| `outcome` | `running`, then `done`, `error`, `stopped`, `timeout`, `skipped` or `paused` |
| `exit_code` | what `job run` exited with |
| `steps`, `tokens`, `cache_hit_ratio` | **this run's** numbers |
| `answer` | the first lines of the final answer |
| `error` | the last error, or the timeout, or why it paused |
| `schedule` | the job's `schedule` string |
| `pid`, `notify_errors` | the process, and any hook that failed |

`kelir job status [name] [--json]` prints it (all jobs without a name); `--json` adds
`live` (the lock's holder is running), `stale` (it says `running` but the process is gone)
and `paths`. A `STATUS.json` that cannot be read is reported (exit 1), never a crash, and
the next run rewrites it. `kelir job history <name> [-n N] [--json]` lists the last runs;
torn or corrupt lines are skipped.

Under `kelir job run` the model is told the paths by **one** `source` line (kind `job`)
at the tail of its context, not by a longer base prompt.

A rotating run adds a **second** tail source: a `carry` line (old session's summary), then
  the `job` line. Both go after the frozen prefix (cache untouched); they stay separate
  because they say different things (leftovers, this run's paths).

## Resuming the next day

The system prompt is frozen at session start; it holds the date, never the time. When a
session is resumed on a later (UTC) day, a `source` event (kind `clock`, "it is now
<date>") is appended at the tail before the task. Same day: nothing is added. The cached
prefix is untouched, so a daily job keeps its provider cache. `KELIR_TODAY=YYYY-MM-DD`
replaces the clock (tests).

## Recipes

Use absolute paths; a scheduler's environment is nearly empty, so put the provider key in
the unit/task environment or in `config.toml`'s `key_env`. The job name and paths below are
examples.

**cron** (`crontab -e`): every night at 03:00; overlapping runs skip themselves.

```text
0 3 * * * OPENROUTER_API_KEY=… /usr/local/bin/kelir job run nightly >> $HOME/nightly.log 2>&1
```

**systemd timer** (`~/.config/systemd/user/kelir-nightly.service` and `.timer`):

```ini
[Service]
Type=oneshot
EnvironmentFile=%h/.config/kelir/env
ExecStart=/usr/local/bin/kelir job run nightly
# exit 75 means "skipped, another run is in flight": not a failure
SuccessExitStatus=75

[Timer]
OnCalendar=*-*-* 03:00:00
Persistent=true
[Install]
WantedBy=timers.target
```

**Windows Task Scheduler:**

```text
schtasks /create /tn "kelir nightly" /sc daily /st 03:00 /tr "C:\tools\kelir.exe job run nightly"
```

For a window-free run use the Task Scheduler's "Run whether user is logged on or not"
option; kelir creates no console windows of its own.

To see a job from anywhere: `kelir job status nightly --json`, or read
`<state>/jobs/nightly/STATUS.json`.
