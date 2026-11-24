# Children: `kelir run --child`, `kelir brief`, `kelir ls`, `kelir kill`

**Summary:** how an agent starts, watches, asks and stops other kelir sessions from
`bash`: foreground and background children, the caps, the registry, `brief`, and
what the tree looks like in the logs.
**Read this when:** you are delegating work, reading `call_started{kind:"child"}` / `call_settled{kind:"child"}`,
wondering why a start was refused (exit 3), or checking what a killed parent leaves
behind (`src/child.rs`, `src/childcli.rs`, `src/session/children.rs`).

There is no spawn tool: the native tools stay `bash`, `edit` and `compact`. A child
is a kelir process the **session** starts on behalf of a `bash` call, with its own
session directory, its own log and its own provider cache.

## The commands

```text
kelir run --child "<task>" [--background] [--model M] [--provider P] [--cwd DIR]
kelir brief "<question>"
kelir ls [--json] [--all]
kelir kill <session>          # a descendant, and everything below it
kelir kill --tree             # every descendant of this session
```

- **Foreground** (`kelir run --child "<task>"`): the call blocks and **stdout is the
  child's final text**. Exit 0 when it finished (`done`/`paused`), 1 when it ended
  `error`/`killed`/`lost`/`stopped`. If the `bash` call is stopped, its child is stopped
  with it (outcome `stopped`).
- **Background** (`--background`): prints a handle at once (the child id and its
  session). The child's completion is a `call_settled` with `kind: "child"` event and a `user` message from
  `child` (`[child ses_… done]` and its answer, cut at 8 KB with a pointer to the child's
  log) at the next boundary, which wakes the parent like a settled `bg` job. A parent
  that ends first takes its running children with it: wait for what you need.
- **Refused** (a cap, below): exit **3**, one line saying which cap and the count.
- Outside any session (`KELIR_SOCK` unset) `kelir run --child` is an ordinary run;
  `--background` has nothing to wake and is refused (use `kelir bg run`).
- `--model` and `--provider` default to the parent's current ones. Credentials come the
  usual way (the environment or opencode's store, read by the child itself); nothing
  secret is ever in a child's argv.

## Caps

| key | default | meaning |
|---|---|---|
| `[children] max_depth` | 3 | the deepest a child may be (root = 0) |
| `[children] max_per_agent` | 10 | **live** children one agent may have |
| `[children] max_live_descendants` | 16 | live descendants one root may have, at any depth |

The **root session** holds the registry and decides, in one step, so two starts racing
for the last slot cannot both win. A settled child frees its slot at once; a child whose
process is gone (crashed, killed from outside) is settled at once by the process that started it (`error`, or `killed` when it was
signalled); only a child nobody reports on is reaped by liveness, after a 5 s grace and the
root's once-a-second tick. Either way it frees its slot. A `brief` takes a slot while it runs.
`kelir ls` shows the cache-hit ratio per session.

## The tree

Every session exports to its tool calls `KELIR_ROOT` (the tree's root session),
`KELIR_PARENT` (the session the call runs in) and `KELIR_DEPTH`, beside `KELIR_SOCK` and
`KELIR_TOKEN`. A child's `session_started` carries `parent` and `depth`.

`kelir ls` prints the tree under `KELIR_ROOT` (outside a session: the newest roots and
what is below them; `--all` for everything), one line per session:

```text
ses_…  running  d0 session m  cache 61%  fix the failing tests
  ses_…  done     d1 child   m  cache 40%  read the logs and say why it fails
  ses_…  running  d1 brief   m  cache 98%  does the config reload on SIGHUP?
```

`--json` prints the same as a nested array (`id`, `parent`, `depth`, `kind`, `state`,
`provider`, `model`, `cache_hit_ratio`, `task`, `children`).
`state` is the session's own while it runs and the registry's word (`done`, `killed`,
`lost`, …) afterwards. `kelir ctl <root> status` lists the live registry
(`child_list`: session, depth, kind, state, pid).

`kelir kill <session>` stops a **descendant** of the session the call runs in and
everything below it; a session never kills itself, a parent or a sibling (exit 1,
`refused`). From a terminal, `kelir kill <session>` asks the tree's root with its token
file, like `kelir ctl`. The pid that is signalled is the one the registry recorded when
the child started, checked against its start time; a caller never names a pid.

## `brief`

`kelir brief "<question>"` (or `kelir ctl <session> brief "<question>"`) asks a side
question on a **by-value snapshot** of the session: the log up to the last settled tool
batch, copied into a new session whose `session_started` names it a child. The request
it sends is the parent's last request plus one user message — same system prompt, same
tool list, same messages, and the **parent's session id as the provider cache key** — so
the prefix hits the parent's cache. `edit` and `compact` are refused by result
(`refused: this is a brief…`), and so are `kelir run --child` and `kelir brief` (a side question
starts no work; exit 3); `bash` works. In `kelir ls` it is kind `brief` with its own question
as the task, and its tokens count only what it added after the snapshot (the copied prefix is
the parent's, counted there). The parent's history is untouched: its log
gains only the `call_started`/`call_settled` pair with `kind: "child"`, and the answer is the command's
stdout. The tool call that runs `kelir brief` is itself not in the snapshot (its result
does not exist yet).

## Processes

A child runs in its own process group (Unix) or kill-on-close Job Object (Windows), held
by the parent session: when the parent session ends, so do its children, unless a
`kelir bg` job started them (those stay detached on purpose). If the parent is killed
without cleanup (`kill -9`, `taskkill /F`), a Windows child dies with the Job; a Unix
child notices within half a second that its parent is gone and ends itself, taking its
own tool calls with it. A child's progress lines go to `<child session>/host.err`; its
stdout is its answer. A child's Herdr status is never reported: only the root writes it.

## Trust

Reaching the root takes the root's session token. The parent hands it to the child over its stdin
pipe; it is held in memory, never in argv, the environment, a file, a log line or a tool call's
environment (a tool call gets its own session's token only). The root also checks that the caller's
own session id is a live descendant before an `admit`, `up`, `settle` or `kill`; a refusal writes
nothing. `kill` is limited to what is below the asker. The ids are random (`chd_` and 128 bits).
Not defended: a process of the same user that reads the root's token **file** can drive the root as
`kelir ctl` can.

## Events

`call_started` and `call_settled` with `kind: "child"` (see `events`). The root writes both for **every**
descendant it admits; a parent that is not the root writes the same pair for its own
direct children, because that is where it wakes and resumes from. A resumed root settles
the children its log shows as unsettled `lost`: nothing outlives its parent.
