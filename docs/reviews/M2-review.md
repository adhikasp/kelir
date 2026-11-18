# M2 review

Reviewed `b3e3074..36a5df6` (implementer commits `a0c6f45..385a909`; the rest are docs) against
`docs/SPEC.md` and `docs/tasks/M2.md`. Host: Windows 11, Git for Windows bash, `hc 0.1.21`, Herdr
pane = ConPTY, 200x120. Live model: `deepseek/deepseek-v4.1-flash-1017` on OpenRouter. Linux gate:
`scripts/remote-check.sh the maintainer`.

## Verdict: FAIL (1 blocker, 2 should-fix, 5 nits)

The TUI itself is good: inline viewport, scrollback, streaming, Ctrl-C, double Ctrl-C, `/quit`,
`kelir ctl` steering, Ctrl-O tail, odd input and resize all behave, and every action goes through
the verb channel. But the Herdr status, the milestone's headline "a kelir pane in the Herdr app
shows working / idle / done", **does not work on this machine's Herdr**: the pane's `status` stays
`null` for the whole of a real turn (B1). A second, intermittent bug corrupts the screen because the
status writer and the painter share stdout unsynchronised (F2). `cargo test` is red on purpose
until F2 is fixed (`tests/review_m2.rs`: 1 of 2 fails).

## Gates (run independently)

| gate | result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo test --lib` | 192 passed |
| `cargo test --test e2e` / `e2e_verbs` / `e2e_tui` | 16 / 21 / 14 passed |
| `review_m0` / `review_m1` | 8 / 5 passed |
| `review_m2` (reviewer-added) | R2 passes, **R1 fails** (F2) |
| Linux (`remote-check.sh the maintainer`, HEAD `36a5df6`) | fmt, clippy clean; 194 unit, 16/14/21/8/5 passed (PTY tests too). Run before `review_m2` existed. |
| release binary | 5.57 MB (limit 20 MB) |
| idle RSS, TUI waiting for input | 12.5-12.9 MB working set, 2.5 MB private (limit 30 MB) |

## Findings

| # | sev | where | finding | failing scenario |
|---|---|---|---|---|
| B1 | **blocker** | `src/herdr.rs:51-67`, `report` 131 | Herdr (`hc 0.1.21`) only accepts a record that is exactly `state=<word>`. kelir's records carry `;app=kelir` (and `;id=...`, `;msg=...`), so **no status is ever set**: `hc get`'s `status` is `null` while the screen says `kelir working`. DEVIATIONS claims the format is "what claude-code emits" and "what the Herdr core parses"; on this Herdr it is not. The unit and PTY tests only check the prefix `\x1b]7501;state=working`, so they cannot see it. | Real run (pane `live-m2-r1`, 3 turns, polled `hc get` every 4 s through a 90 s tool call): `status: None` throughout. Probe `scripts/live/m2_osc_probe.py` in plain panes: `state=working` -> `{'state':'working'}`; `state=working;app=kelir`, `...;id=ses_1`, `...;msg=aGk=`, `...;title=x`, `...;progress=10`, `...;kind=agent`, `...;agent=kelir`, `state=working;` (trailing `;`), `,`/`&`/space separators, `working;app=kelir` -> `null`; ST and BEL terminators both fine; same on a Linux core (the maintainer). A process renamed `claude.exe` emitting `app=claude-code` is also `null`, so it is not an agent-name allowlist. A bare `state=working` then `state=done` flips `status` as expected. |
| F2 | should-fix | `src/herdr.rs:116,135`, `src/tui.rs:892,899`, `src/session.rs` (`report`) | The session task writes OSC records straight to `std::io::stdout()` while the TUI task paints through ratatui's `Stdout`. crossterm emits one CSI as several small `write`s, so an OSC lands in the middle of one and the tail is printed as text. Seen live (real model): a stray `17;200H` line in the live area. | `tests/review_m2.rs::r1_...`: 40 short turns in a pty, scan the bytes for `ESC [ params ESC`. Fails every run (3-6 cut sequences, e.g. `\x1b[\x1b]7501;state=done...`, `\x1b[31;\x1b]7501;...`). Fix: send status through the TUI task (one writer), or share one lock around all stdout writes. |
| F3 | should-fix | `src/tui.rs:905-912` (`finish`) | After `/quit` or a double Ctrl-C the shell's next output overwrote **row 1 of the pane** (the `session ses_...` banner line) instead of landing below the viewport: `kelir exit: 0` + the rest of the old text on that row. 2 of 2 exits in a Herdr/ConPTY pane. Cause not isolated (the raw tail is ConPTY's re-render; I could not see the cursor the shell started from). | Pane runs `m2_pane.sh`, then `echo kelir exit: $?`; `hc read` shows `kelir exit: 01m7qwj0...` where `session ses_01m7qwj0...` was. |
| N1 | nit | `src/session.rs` idle `stop`, `src/tui.rs` `Reply` | Ctrl-C in an idle TUI prints `» stop: nothing is running` and then `» stop: now`: the reply says the verb was applied "now" for a stop that was dropped. | Ctrl-C in an idle pane. |
| N2 | nit | `src/cli.rs`, `src/main.rs` | Bare `kelir --model x` is a usage error; only `kelir tui --model x` takes flags. Task item 1 says "`kelir` with no subcommand (and `kelir tui ...`)". Flatten `TuiArgs` into `Cli` or document it. | `kelir --model m` -> `unexpected argument '--model'`. |
| N3 | nit | `tests/e2e_tui.rs:471-480` | The PTY test proves the OSC bytes are written, not that Herdr takes them (B1), and a CSI/OSC interleave (F2) passes unseen. Add an assertion on the exact root record and keep R1. | n/a |
| N4 | nit | `src/tui.rs:884-895`, `scrollback.rs` | `wrap`/`Live::due` re-wrap the whole streamed message on every frame: O(n^2) for one very long answer. Not measured. | a 1 MB single assistant message (not run) |
| N5 | nit | `src/tui.rs` `finish` | Terminal restore on a panic is by ratatui's hook and could not be exercised: there is no injection point and the editor survived every input I threw at it (CJK, ZWJ emoji, 260-char word, paste, odd commands). | n/a |

Checked and **correct**: every `/` command parses to its verb and appears in the log with
`from: "tui"` (`tools off bash` -> `next_step`, `later` -> `turn_end`, `compact` -> `next_step`
plus the "M3" error event); the viewport is 8 rows; wide characters are counted at two columns; the
log keeps the full text of an odd message (CJK, emoji, 260-char word); a message sent with
`ctl say` is echoed in the pane (`› hello from ctl`) and logged `from: ctl`; tool lines are
`$ cmd (exit 0 · 236 B · 6 lines)`, `(stopped · 6 B)`, `(disabled · 118 B)`; cache ratio and cost
show in the status line; `kelir` / `kelir tui` without a TTY exit 2 in 66 ms with a clear message
(also when piped); no secrets or personal paths in the new files; new deps (`ratatui`,
`unicode-width`, dev `portable-pty`) are spec-named or justified in `docs/DEPS.md`.

## Test pyramid

- **Unit (192, +51):** editor state machine, command parsing, status line, OSC builder, wrap/commit.
  Good, pure, and the "every action has a verb" enumeration is exhaustive by construction.
- **E2E `e2e_tui` (14):** `TestBackend` snapshots (idle, streaming, running call, error, tail), the
  action->verb enumeration, non-TTY, and a real-binary PTY drive. Gaps: nothing checks that Herdr
  accepts the record (B1), nothing checks the byte stream for torn sequences (F2), no resize, no
  Ctrl-C-with-a-running-call, no `ctl` steering of a TUI session, no exit-cursor check.
- **Added `tests/review_m2.rs` (2):** R1 torn-CSI scan over 40 turns (fails: F2), R2 resize
  narrow / wide / 12x30 mid-session then a turn and `/quit` exit 0 (passes).

## Live drive log

Panes: `hc run --bg --name live-m2-... --cwd <scratch> "C:/Program Files/Git/bin/bash.exe" -c '... scripts/live/m2_pane.sh <scratch>'`
(`m2_pane.sh` runs `kelir tui --model deepseek/deepseek-v4.1-flash-1017`). Real-model runs sourced
`live.env` inside the pane command; the key was never printed. Mock-backed runs
(`canned_server.py`, dummy key) covered the scenarios that need a deterministic long tool call.
The orchestrator had to recreate `live.env` (it was missing at first); the mock runs were done
meanwhile.

- **L1 real model, streaming + tool lines + status (pane r1): PASS for the TUI, FAIL for status.**
  Prompt "list the files with bash, summarise, then `sleep 90`". Viewport repainted; tool lines
  `$ ls -la (exit 0 · 236 B · 6 lines)`; status line `kelir working · ... · step 3 · 2.7k tok · 31% cache
  · $0.0003 · 1 running`; the final answer was committed to scrollback. `hc get`'s `status` was
  `None` the whole time (B1). One stray `17;200H` appeared (F2).
- **L2 `ctl say --now` from a second shell (r1): PASS.** `kelir ctl latest say --now "change of plan..."`
  -> `{"ok":true,"applies":"now"}`; the pane echoed `› change of plan: ...`, the `sleep 90` settled
  `stopped` (`sleep.exe` 1 -> 0), the model ran `echo steered`.
- **L3 `/quit` during `sleep 95` (r1): PASS.** Log: `verb end`, `tool_settled stopped`, `turn_ended
  stopped`, `session_ended end`; pane exit 0; no `sleep.exe` left. (My first try typed `/quit`
  through Git Bash path conversion -> `C:/Program Files/Git/quit`, which kelir correctly took as a
  message.)
- **L4 Ctrl-C during `sleep 120` (mock pane c3): PASS.** `hc keys ctrl+c` -> `» stop (Ctrl-C again...)`,
  `(stopped · 6 B)`, status `kelir idle`, `sleep.exe` 1 -> 0. Two Ctrl-C in one `hc keys` -> `» quit
  (second Ctrl-C)`, `» end: now`, exit 0. Ctrl-O showed the call's output tail and closed again.
- **L5 `ctl latest say` into an idle TUI (c3): PASS** (queued, turn ran, answer in scrollback).
- **L6 odd input (mock pane c4): PASS.** `/foo`, `/model`, `/tools off`, `/tools`, `/later x` in idle
  (starts a turn), `/compact tighten` (verb logged, "not implemented until M3"), `/stop` in idle,
  `/help`, then a line with CJK, ZWJ emoji and a 260-char word: wrapped, logged intact, session alive.
- **L7 resize: PASS in the pty test (R2); not driven in a pane** (`hc` has no resize command).
- **L8 scrollback keeps finished turns: PASS** (`hc read` shows every turn above the viewport).
- **L9 terminal restore on exit: FAIL** (F3). Panic restore not exercised (N5).
- **Herdr status flip (`hc get` working -> done): FAIL** (B1).

**Not live-verified:** Go provider (`KELIR_GO_API_KEY` unavailable), macOS, Linux pane, a real
resize in a pane, panic restore, `TERM_PROGRAM=herdr-telegram`. Caveat on B1: the acceptance rule
was measured on `hc 0.1.21` only, and I did not capture what Claude Code itself writes; a newer
Herdr may take the extra keys.

## Spec items not yet covered

- Herdr `working`/`done`/`idle`/`error` actually reaching the pane (B1); `msg` for `error` (no
  accepted carrier found; every record with `msg` was ignored).
- The "resume label" record (`id=...`) has no effect on this Herdr (no `records` appear).
- `--resume` is M3.

## Fix list for the implementer (in order)

1. B1: make the root record exactly `ESC ] 7501 ; state=<word> ST` and re-check with
   `scripts/live/m2_osc_probe.py` + `hc get` in a pane. If the id/msg records matter, find what this
   Herdr takes for them (capture what Claude Code writes in a pane) before adding any key back.
   Add a test on the exact bytes.
2. F2: one writer for the terminal (send status through the TUI task, or one shared lock); `review_m2`
   R1 goes green.
3. F3: leave the cursor below the viewport on exit; add an exit-cursor check.
4. N1-N3 as time allows.
