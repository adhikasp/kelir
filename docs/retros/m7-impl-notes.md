# M7 implementer notes: children as CLI

Job `m7`: brief `docs/tasks/M7.md`. Worktree `kelir-wt/m7`, branch `job/m7`.

## What landed

| piece | where |
|---|---|
| registry, caps (depth 3 / 10 per agent / 16 live), liveness, hand-off, snapshot cut and copy | `src/child.rs` |
| `kelir run --child [--background]`, `kelir brief`, `kelir ls [--json] [--all]`, `kelir kill <s>\|--tree` | `src/childcli.rs`, `src/cli.rs`, `src/main.rs` |
| `kelir child-host` (hidden): a child session's process | `src/session.rs` (`child_host`), `src/child.rs` |
| the `child` verb, the root's registry operations, spawn-wait-settle, kill, wake message, reaping | `src/session/children.rs` |
| `child_started` / `child_settled` | `src/event.rs`, `src/docs/tables.rs` |
| `brief` verb (`kelir ctl <s> brief`), `Verb::Child` | `src/verbs.rs`, `src/ctl.rs` |
| caps in `config.toml`; `KELIR_ROOT/PARENT/DEPTH` for tool calls | `src/config.rs`, `src/envs.rs`, `src/bg.rs` (`lineage_env`) |
| a long verb notices its client leaving | `src/ipc.rs` (`peer_gone`) |
| `ctl status`: `children`, `child_list` | `src/session.rs`, `src/spend.rs` |
| reference topic `children`, files/troubleshooting/source-map/README | `docs/reference/` |
| tests | `tests/e2e_m7.rs` (15), `src/child.rs` (18 unit), `src/childcli.rs` (4 unit); the mock learned `Mock::start_routed` (`tests/support/mod.rs`) |

## How a child travels

`kelir run --child "t"` in a bash call sends `child`/`run` to `KELIR_SOCK` (the session it runs
in, under that session's token). The session's loop hands the work to a task:

1. **admit**: the root is asked (locally for the root, over its socket with token + link for a
   descendant). The root reaps dead children, checks the three caps in one call, mints
   `chd_<hex>` and a link, writes `child_started`.
2. **spawn**: `kelir child-host` in its own process group / Job Object, hand-off on stdin
   (task, place in the tree, root socket, root token, link). A brief first gets its seeded
   session directory.
3. **up**: the pid and start time go to the registry (kill and reaping use them).
4. **wait**: stdout is the answer. Background: the verb already answered with a handle.
   Foreground: the reply waits; a dropped client kills the child.
5. **settle**: the registry's word wins (`killed`), `child_settled` is written, and for a
   background child of this session a `Verb::Child` is queued, so the wake message lands at a
   boundary, never inside a tool batch.

## Things that bit

- `json!({"op": "up-local"}).merge(body)` where `body` also had `"op"`: the second object won
  and the local call turned into the remote one (refused for lack of a link). Bodies carry no
  `op` now; the caller adds it.
- A child's `println!("→ bash …")` progress lines ended up in the answer. They go to stderr when
  the session is a child.
- Killing a subtree from above leaves the grandchild's spawner dead, so nobody reports its
  settlement: the root's once-a-second reap settles it (`killed` if a kill was aimed at it).
- A brief's copied log starts with the parent's task, so a test (or a reader) that finds a root
  by its task text must also look at `parent`.
- Windows hardening works: a hardened child host's command line cannot be read even by its
  user (`PROCESS_VM_READ` denied); the e2e test checks argv only where the OS lets it (Linux).

## Not done / open

- The M8 delegation skill and any base-prompt line about `kelir run --child` (the spec puts
  both at M8).
- `kelir ls` has no TUI panel; the wake is an info row.
- Linux half of the gate is run from here with `--linux=the maintainer`; the Unix-only parts
  (`killpg`, the parent poll, `/proc` leftovers) are exercised only there.
- Reviewer's live drive (real model, depth 3, `cached_tokens` of a brief on a long session).

## Gate

See the PROGRESS row.
