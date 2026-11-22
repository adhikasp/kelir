# Gates: allow, deny or rewrite at six events

**Summary:** a gate is an executable or built-in Rust gate kelir runs at a
named event; it reads the event as JSON on stdin (built-ins get the same input)
and may answer `allow`, `deny` or `rewrite` as JSON on stdout. How to declare
one, what each event sends and accepts, the fail-open rule, the environment a
gate gets, and the five example gates.
**Read this when:** you want a veto, a nudge or a side effect (pull, commit,
check a file) around a session, or a `gate` event in a log needs explaining
(`src/gates.rs`; the hooks are in `src/session.rs`: `gate`, `gate_prompt`,
`run_tools`, `drive_turn`).

Nothing is enabled by default. Opinions live in gates, not in the core.

**Trust.** A gate is code that runs as you. Gates from your own machine config
(and `--config`/`KELIR_CONFIG`) always run. Gates a *project* brings (its
`.kelir/config.toml` `[gates]`, and a skill under `.kelir/skills` or
`.claude/skills` that binds one with `kelir-gate`) run only in a **trusted
project**: list its directory in the machine config's `trusted_projects` (a
directory covers everything under it), or set `KELIR_TRUST_PROJECT=1` for one
run. Otherwise they are skipped, each with an `error` event (`where` `gate`,
"not started: this project is not trusted"), and `observe` marks them
`trusted: false`. A project config cannot trust itself (`trusted_projects` there
is ignored with a warning).

## Declaring a gate (three equivalent ways)

1. **`[gates]` in `config.toml`** (the machine config's gates and the project's
   are both kept, in that order; the project's need trust, see above). Each key is an event name; the value is
   a command string, a table, or a list of either:

   ```toml
   [gates]
   turn_end = "python /path/to/verify_before_claim.py"
   session_end = ["sh /path/commit.sh", "sh /path/push.sh"]
   before_tool = [
     { name = "no-rm", command = "python /path/no_rm.py", tools = ["bash"] },
   ]
   after_tool = { command = "python /path/note_check.py", tools = ["edit"], env = { CHECKER = "mdl" } }
   ```

   Table keys: `command` (required), `name` (letters, digits, `-_.`; default is
   the stem of the command's last path), `tools` (also `tool`; `before_tool` and
   `after_tool` only; names, `mcp:<server>` for every tool of a server, or a
   trailing `*` prefix), `env` (extra variables, strings). A repeated name gets
   `-2`, `-3`. The command runs through the configured `shell`, so `.cmd` and
   `.ps1` gates work with a `cmd.exe` or PowerShell shell.

2. **A skill's `scripts/` file**, bound by the skill's front matter:

   ```yaml
   ---
   name: verify
   description: block unverified claims
   metadata: {kelir-gate: turn_end}
   ---
   ```

   `kelir-gate: <event>` binds `scripts/<event>.*`, else `scripts/gate.*`, else
   the only file in `scripts/`. `kelir-gate: turn_end=scripts/check.py;
   after_tool=scripts/note.py` names the files (they must stay inside the skill
   directory). The interpreter follows the extension (`.py` python, `.sh` bash,
   `.js` node, `.ps1` PowerShell, `.cmd`/`.bat` cmd). The gate is named
   `<skill>.<event>`. A project skill's gate needs the project to be trusted.

3. **An inline command string**: the string form of (1).

4. **A built-in gate**: a named table in `[gates]` with `builtin = "<name>"` in
   place of a command. The built-in runs in-process through the same runner and
   produces the same `gate` event; only its options live in the same table. The
   known built-ins are `hint` (default event `prompt_submit`); an unknown name is
   a config error that lists the known ones.

   ```toml
   [gates.hint]
   builtin = "hint"
   # event = "prompt_submit"   # the only event hint supports; omit for the default
   model = "some/cheap-model"  # required
   # provider = "openrouter"  # default: the session's provider
   # endpoint = "http://127.0.0.1:8080/v1"
   # timeout_ms = 2000  # positive integer; `timeout` is an alias, not documented
   # max_names = 3      # positive integer
   ```

   Built-ins fail open like external gates: an error or timeout is logged and
   treated as `allow`. A built-in may use the session's provider client and
   read-only event log (a `GateCtx`) and reports its own token usage as a
   `usage` event whose `kind` is the gate's name.

## The six events

| event | stdin carries (besides the session fields) | `deny` means | `rewrite` replaces (`input`) |
|---|---|---|---|
| `session_start` | `provider`, `model`, `resumed` | nothing (side effects only) | nothing |
| `prompt_submit` | `text`, `from` (`tui`, `ctl`, `cli`, `channel`) | the prompt is dropped, with the reason shown to the person | `{"text": …}` the prompt |
| `before_tool` | `tool`, `call_id`, `input` | the call returns `denied by gate <name>: <reason>` (outcome `denied`) | the call's input object |
| `after_tool` | `tool`, `call_id`, `input`, `result`, `outcome`, `exit_code`, `out_path` | nothing | `{"result": …}` replaces what the model reads; `{"note": …}` appends `[gate <name>] …` |
| `turn_end` | `final_text`, `tool_calls`, `steps`, `blocked` | the turn continues once with the reason as a message from `gate`; **blocks at most once per turn** | `{"text": …}` the final text |
| `session_end` | `reason` | nothing | nothing |

`tool` is the native name (`bash`, `edit`, `compact`) or `mcp:<server>.<tool>`
for a call made through `kelir mcp`. `after_tool` runs only for a call that ran
and finished (`ok`/`error`), and a rewritten result is cut to `spill_limit`.
`prompt_submit` sees human prompts (the command-line task, the TUI, `ctl say`),
never a job wake-up or a gate's own message. A command-line task a gate drops
ends a one-shot run with exit 1 (session ended `denied`); a TUI waits for the
next message.

Every stdin document also carries `event`, `session`, `session_dir`, `log` (the
path of `events.jsonl`), `cwd` and `turn`.

## The answer

One JSON object on stdout: `{"decision": "allow|deny|rewrite", "reason": "…",
"input": {…}, "context": "…", "kind": "…"}`. Empty output is `allow` (a
side-effect gate prints nothing). `reason` is kept to 2000 characters. Several
gates at one event run in declaration order; each sees the previous rewrite;
the first `deny` (where deny means something) ends the chain.

`context` (optional, at most 32 KiB) is extra text the gate contributes: kelir
appends it as a `source` event of kind `kind` (default `gate`) **after** the
user message at `prompt_submit`, and at the tail otherwise, so the cached
prefix is unchanged. The built-in hint uses this path; an external gate gets
the same one. `usage` (optional object with `prompt_tokens`,
`completion_tokens`, `cached_tokens`, `cache_write_tokens`) is recorded as a
`usage` event of the gate's name, for a gate that made its own provider call.

## Fail open

A gate that cannot start, exits non-zero, runs past **30 s**, prints something
that is not one decision object, prints more than 1 MiB, or rewrites with the
wrong shape is **allowed**. The `gate` event for the run says `decision: allow`
and `error` says why. A timed-out gate's whole process tree is killed (Windows:
its Job Object; Unix: its process group, then every process that still carries the
run's `KELIR_GATE_RUN` token, so a gate that detaches with `setsid` or a double
fork is found too); no gate leaves a process behind, and none opens a console
window on Windows. If kelir itself is killed mid-gate, a small `kelir gate-guard`
process started beside the gate notices within about 0.2 s and kills the gate's
tree (Unix). **Limits:** the token sweep reads `/proc` on Linux and `ps` on
macOS (on other Unix systems only the process group is killed); a gate that
clears its own environment before detaching escapes the sweep on either system,
and one that crosses `ssh` or a container boundary is out of reach on any
system. While a gate runs the session waits
for it, so a `stop` verb is read when the gate ends (at most 30 s).

## Limits: a gate cannot multiply

At most **8** gate processes are alive in one kelir process at any moment; a run
past that is skipped and logged (`error: 8 gates are already running; this run was
skipped`), never queued or retried. A spawn failure is one `gate` event with `error`,
never a retry. A kelir started by a gate (it inherits `KELIR_GATE_EVENT`) runs **no
gates**: a gate never triggers gates. An empty command is a config error at load.

**Child sessions** (`kelir run --child`, `kelir brief`) run the same gates with their
own session directory (`KELIR_SESSION_DIR` is the child's), their own `gate` events in
their own log, and the same scrubbed environment: a child's gate never sees the token,
the socket or a provider key either. The cap is per process, so a child is limited to
**2** live gate processes (the root keeps 8); with the default 16 live descendants a
whole tree has at most 8 + 16 x 2 = **40** gate processes alive. A `brief` is a child
and counts the same. A prompt a parent hands to a child (`from` `parent` or `brief`) is
not a human prompt and goes through no `prompt_submit` gate.

## What a gate's environment holds

An allow-list, not a copy: `PATH`, the home/temp/system variables, locale,
proxy and CA variables (the same list an MCP server gets) and `SSH_AUTH_SOCK`,
then `KELIR_SESSION_DIR`, `KELIR_GATE_EVENT`, `KELIR_GATE_NAME`, `KELIR_GATE_RUN`
(a per-run token kelir uses to find leftovers; leave it alone), then the
gate's own `env` table. **No** `KELIR_TOKEN`, **no** `KELIR_SOCK`, no provider
key, no `key_env` variable. A gate that needs a secret is given it in its own
`env` table, explicitly.

## What is logged

One `gate` event per run (`gate`, `event`, `decision`, `reason`, `ms`,
`rewritten?`, `error?`), including failed-open runs. A gate's rewritten input
is in the `call_started`/`call_settled` (`kind: "tool"`), `user` or `turn_ended` event that used it; the model's
own `assistant` event keeps what it said. `kelir observe <session>` prints the
effective gates (`== gates ==`, commands and `env` names only) and the runs so
far (`== gates fired ==`). `kelir ctl <session> status` carries a `gates` object:
`declared`, `runs`, `denied`, `rewritten`, `failed_open` and the `last` run (printed as one `gates` line when any gate is declared)
(`gate`, `event`, `decision`).

## The example gates (`examples/gates/`)

Generic scripts to copy; none is enabled by default. Python 3, standard
library only.

| file | event | what it does |
|---|---|---|
| `verify_before_claim.py` | `turn_end` | denies a final answer that hedges ("probably", "I think", "from memory") when no tool ran that turn; asks for one check, once |
| `note_check.py` | `after_tool` on `edit` | runs the checker named in the gate's `env` table (`NOTE_CHECK_CMD`) over the edited file and appends its output as a note |
| `git_sync.py` | `session_start`, `session_end` | `git pull --ff-only` at start; at end commits **tracked** changes only (`git add -u`) and pushes. `GIT_SYNC_ADD_NEW = "1"` also stages new files; a secret-like name (`.env*`, `*.pem`, `*.key`, `id_rsa*`, `credentials*`, `*secret*`) refuses the whole commit; `GIT_SYNC_DRY_RUN = "1"` only prints what it would do; `GIT_SYNC_PUSH = "0"` turns the push off; never forces. All in the gate's `env` table |
| `numbered_answers.py` | `prompt_submit` | turns "do 1,4" into the text of items 1 and 4 of the last numbered list in the log |
| `load_project_agents.py` | `before_tool` on `edit`/`bash` | first call touching a repo whose `AGENTS.md` wasn't loaded is denied with a read instruction; `PROJECT_ROOT` (or `KELIR_AGENTS_REPO`) in the gate's `env` names it; once per session |

### Reading a repo's instructions when the session starts elsewhere

kelir walks up from the session cwd for `AGENTS.md`/`CLAUDE.md`; it does not
re-scan when a tool targets another repo. `load_project_agents.py` checks the
session log (`session_started` `sources[]` plus its own deny marker): the first
`edit`/`bash` touching `PROJECT_ROOT` is denied with a `cat` instruction, later
calls pass; read-only bash (`cat`, `rg`, `head`, ...) is never denied.

```toml
[gates]
before_tool = [
  { name = "load-project-agents", tools = ["edit", "bash"],
    command = "python3 /path/to/load_project_agents.py",
    env = { PROJECT_ROOT = "/path/to/the/code/repo" } },
]
```

`PROJECT_ROOT` must be in the gate's `env`: gates get a scrubbed environment.
Sessions started in the repo already load its instructions and are left alone.

## A gate that talks to its own session

A gate that runs `kelir ctl <this session> say ...` stalls: the session reads verbs
only after the gate ends, so the call waits out the gate's 30 s timeout, and the
message is then delivered (and gated) as a human prompt. Write to a file, or
return `rewrite`, instead.

## Testing a gate by hand

```sh
echo '{"event":"turn_end","final_text":"probably fine","tool_calls":0}' \
  | python examples/gates/verify_before_claim.py
```
