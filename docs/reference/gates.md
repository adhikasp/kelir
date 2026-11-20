# Gates: allow, deny or rewrite at six events

**Summary:** a gate is an executable kelir runs at a named event; it reads the
event as JSON on stdin and may answer `allow`, `deny` or `rewrite` as JSON on
stdout. How to declare one, what each event sends and accepts, the fail-open
rule, the environment a gate gets, and the four example gates.
**Read this when:** you want a veto, a nudge or a side effect (pull, commit,
check a file) around a session, or a `gate` event in a log needs explaining
(`src/gates.rs`; the hooks are in `src/session.rs`: `gate`, `gate_prompt`,
`run_tools`, `drive_turn`).

Nothing is enabled by default. Opinions live in gates, not in the core.

## Declaring a gate (three equivalent ways)

1. **`[gates]` in `config.toml`** (machine, then project; the project table
   wins per key like every other key). Each key is an event name; the value is
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
   `<skill>.<event>`. A project skill binds a gate with **no trust prompt**, the
   same as a project `mcp.toml` (docs/DEVIATIONS.md).

3. **An inline command string**: the string form of (1).

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
"input": {…}}`. Empty output is `allow` (a side-effect gate prints nothing).
`reason` is kept to 2000 characters. Several gates at one event run in
declaration order; each sees the previous rewrite; the first `deny` (where deny
means something) ends the chain.

## Fail open

A gate that cannot start, exits non-zero, runs past **30 s**, prints something
that is not one decision object, prints more than 1 MiB, or rewrites with the
wrong shape is **allowed**. The `gate` event for the run says `decision: allow`
and `error` says why. A timed-out gate's whole process tree is killed (Windows:
its Job Object; Unix: its process group); no gate leaves a process behind,
and none opens a console window on Windows. While a gate runs the session waits
for it, so a `stop` verb is read when the gate ends (at most 30 s).

## Limits: a gate cannot multiply

At most **8** gate processes are alive in one kelir process at any moment; a run
past that is skipped and logged (`error: 8 gates are already running; this run was
skipped`), never queued or retried. A spawn failure is one `gate` event with `error`,
never a retry. A kelir started by a gate (it inherits `KELIR_GATE_EVENT`) runs **no
gates**: a gate never triggers gates. An empty command is a config error at load.

## What a gate's environment holds

An allow-list, not a copy: `PATH`, the home/temp/system variables, locale,
proxy and CA variables (the same list an MCP server gets) and `SSH_AUTH_SOCK`,
then `KELIR_SESSION_DIR`, `KELIR_GATE_EVENT`, `KELIR_GATE_NAME`, then the
gate's own `env` table. **No** `KELIR_TOKEN`, **no** `KELIR_SOCK`, no provider
key, no `key_env` variable. A gate that needs a secret is given it in its own
`env` table, explicitly.

## What is logged

One `gate` event per run (`gate`, `event`, `decision`, `reason`, `ms`,
`rewritten?`, `error?`), including failed-open runs. A gate's rewritten input
is in the `tool_started`/`user`/`turn_ended` event that used it; the model's
own `assistant` event keeps what it said. `kelir observe <session>` prints the
effective gates (`== gates ==`, commands and `env` names only) and the runs so
far (`== gates fired ==`).

## The example gates (`examples/gates/`)

Generic scripts to copy; none is enabled by default. Python 3, standard
library only.

| file | event | what it does |
|---|---|---|
| `verify_before_claim.py` | `turn_end` | denies a final answer that hedges ("probably", "I think", "from memory") when no tool ran that turn; asks for one check, once |
| `note_check.py` | `after_tool` on `edit` | runs the checker named in the gate's `env` table (`NOTE_CHECK_CMD`) over the edited file and appends its output as a note |
| `git_sync.py` | `session_start`, `session_end` | `git pull --ff-only` at start, commit and push at end; it only exists if you declare it, and `GIT_SYNC_PUSH = "0"` in its `env` table turns the push off; never forces |
| `numbered_answers.py` | `prompt_submit` | turns "do 1,4" into the text of items 1 and 4 of the last numbered list in the log |

## Testing a gate by hand

```sh
echo '{"event":"turn_end","final_text":"probably fine","tool_calls":0}' \
  | python examples/gates/verify_before_claim.py
```
