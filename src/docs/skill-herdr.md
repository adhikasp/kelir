---
name: herdr
description: inside a Herdr pane, list the terminals around you (hc ls --json) and count those sharing my space; never kill, remove or type into another pane
metadata:
  builtin: true
  body: kelir docs
---

# Herdr: the terminals around you

Herdr runs programs and coding agents in terminals that stay alive; `hc` is its CLI.
Check first whether you are in a pane:

```sh
echo "$HC_TERMINAL_ID"      # empty: you are NOT in Herdr
```

**If it is empty, say so** ("I am not running inside a Herdr pane") and do not try
`hc` guesses: there is nothing to count or address. If `hc` is not on PATH, say that.
Everything below assumes `HC_TERMINAL_ID` is set (also `HC_TERMINAL_SOCKET`, `HC_SCOPE`).

## See the fleet

```sh
hc ls --json            # live terminals; add --all for finished ones
hc get "$HC_TERMINAL_ID" --json    # me: address, labels, where, cwd
hc machine ls           # other machines this one reaches
```

`hc ls --json` is `{"machines":[{name,state}], "terminals":[…]}`. Each terminal has
`address` (`t1x`, or `<machine>/t33` on another machine), `agent`, `cwd`, `state`
(`live`…), `status` (`working`, `done`, `blocked`, absent for a plain shell), `turn`, and
`where`: a label `"<space> / <tab title>"`. Labels `herdr.space`, `herdr.tab` and `where`
come from `hc get`; `--label K=V` filters.

## "How many agents share my space?"

Ask live, never from memory (the frozen prompt line is a snapshot):

1. `hc get "$HC_TERMINAL_ID" --json` gives my `where`; my space is the text before ` / `.
2. `hc ls --json`, keep terminals whose `where` starts with `<my space> / `, and whose
   `agent` is an agent (not a bare shell: no `status`, `agent` is a shell path).
3. Count them, say whether that includes me, and list `address agent status` per line.

Group by that space prefix, across machines (the `machines` list says which are
connected; a disconnected machine's terminals may be missing, say so).

## Talk to another terminal

```sh
hc read <t>             # its screen and history; start here before touching anything
hc send <t> "text"      # paste text and press enter (to an agent: a new message)
hc wait <t>             # block until it stops working (--output X: until it prints X)
hc wait <t> --children all   # until none of <t>'s children is working (any: the next news)
```

Use `hc wait`, not `sleep`. Read before you send; one message, not a stream.

## The rule

**Never kill, remove, interrupt, relabel or type keys into another agent's pane.** No
`hc kill`, `hc rm`, `hc interrupt`, `hc keys`, `hc label` on a terminal you did not start.
You may read it and `hc send` it a message when that is the task. Panes you started
yourself (`hc run --bg …`) are yours to end. When a pane looks stuck, report it to the
person with its address; do not "clean up".

Depth: `hc --help`, `kelir docs sources-and-skills` (the Herdr line in the system prompt),
`kelir docs tui` (the status states kelir itself reports).
