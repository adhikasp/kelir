# STEER-7 review (session-socket hardening) — verdict: FAIL (1 blocker)

Reviewer: fresh QA. Branch `job/steer7`, diff `e13662a..36c3063`. Windows 11 (a Linux box) + Linux (the maintainer).

## Verdict
**FAIL.** The token and the socket/pipe permissions work: every stranger attack was refused. But the
brief's headline goal — *a test child inside the session can no longer impersonate the bg shim* — is
not met. The child holds the token (by design), and the "this session started the job" check is
satisfied by evidence the child can create itself.

## Counts
- `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean.
- `cargo test --no-fail-fast`: 731 passed; the only failures are my 2 new red tests
  (`tests/review_steer7.rs`). All pre-existing suites are green, including `e2e_steer7` 10/10, M4, STEER-5.
- Linux (`scripts/remote-check.sh the maintainer kelir-verify-steer7-rev`, HEAD 36c3063): fmt and clippy ok,
  733 passed, 5 skipped, rc 0.
- No leaked processes: every kelir.exe / shim started from my binary was killed by exact PID; a final
  scan for processes under `kelir-wt\steer7\target` found none.

## Findings

| # | Sev | Where | Finding |
|---|-----|-------|---------|
| B1 | **blocker** | `src/trust.rs:365` (`owns`), `src/session.rs:2178` (`note_bg` falls back to the report when there is no status file) | **Shim impersonation still works for any process holding the token (every bash child).** `owns(id)` is true when `commands/<id>/` is a directory or `bg/<id>.status` is a file; the child can `mkdir commands/bg_fake` or write `bg/bg_fake.status` itself, then send a `bg` report. Live run (real binary, real model, session A): the log gained `bg_started`, `bg_settled state:lost` and `user from:job "[bg job bg_fake_by_child was lost: its shim is gone ...]"` for a job that never existed; the status-file variant recorded `bg_settled exited` with a made-up exit code. This is exactly F8 (43 injected "lost" events). Repro: `scripts/live/steer7_setup.sh` + `scripts/live/steer7_attack.py child` (attacks a1/a2/a4), and the failing tests `a_child_cannot_invent_a_job_by_making_a_commands_dir` / `a_child_cannot_invent_a_job_by_dropping_a_status_file` in `tests/review_steer7.rs`. Fix direction: ownership must come from the in-memory registry (ids the session itself started: its calls' ids and bg-spawn/watcher-registered ids; rebuilt on resume from the log's `tool_started`/`bg_started`), not from files in a directory the child can write; and `note_bg` must not record a job whose status file the shim did not write (no file means no event, rather than "lost"). |
| S1 | should-fix | docs; `src/bg.rs:925` | A sibling process of the same user (another session's tool child) reads `<state>/sessions/<id>/token` (0600 / owner ACL does not stop the same user) and can `say`/`cancel` the other session. Live: from session A's child, `cancel` and `say` on session B succeeded with B's token file (attack b4). This is by design (SPEC "reachability, not identity") but contradicts the brief's test line "a child process of one session cannot cancel another session's job" for anything that looks in the state dir; the e2e test only proves it with the child's own token. `KELIR_SESSION_DIR` also points children at the state tree. Say so plainly in DEVIATIONS, or keep the token somewhere a sibling does not find by listing the shared sessions dir, if cross-session isolation is wanted. |
| S2 | should-fix | `src/trust.rs:365-375` | Call ids (`13-2-1`, `6-1-1`) are deterministic and identical across sessions, so a "foreign" id that collides with a local `commands/<id>` is accepted: `cancel 13-2-1` naming session B's call was accepted by session A (attacks b2/b3). Folds into B1's fix (registry by unique id). |
| N1 | nit | `src/trust.rs:227` | `valid_job_id` accepts Windows reserved names and trailing dots (`nul`, `con`, `bg_esc.`): `bg-spawn` created `nul.status`, `con.status`, `bg_esc..status`. Harmless for a token holder, but a path-alias foot-gun (`id.` aliases `id` for `commands/`). Reject reserved device names and trailing `.`/space on Windows. |
| N2 | nit | `Trust::create` | On Linux with umask 000 the session's `bg/` is mode 777 (the session dir above it is 700, so unreachable by others; defence in depth only). |
| N3 | nit | `src/ipc.rs` | An unauthenticated peer can hold a connection and send a long unterminated line; 1 MB was answered instantly with `bad request line`. Larger lines and idle connections were not exhaustively tested; no read timeout or line cap seen. |

## Threat model results (each attack run against the real release binary)
Attack scripts: `scripts/live/steer7_attack.py` (modes stranger / reader / child / foreign) and
`scripts/live/steer7_setup.sh` (two real sessions in Herdr panes). The stranger ran from a
**console-less process** (`launch_detached.py detached`) over the named pipe.

| Threat | Result |
|--------|--------|
| (a) impersonate shim, **no token** (stranger) | refused for bg, bg-spawn, cancel, say, stop, end, model, compact and 22 variants (empty / space / null / int / list / bool / wrong tokens, `args.token`, `Token`, `auth`, `from=tui`, a valid line pipelined after a refused one); 0 log lines written |
| (a) impersonate shim **with token** (tool child) | **SUCCEEDS** (B1) |
| (b) cancel/report another session's *real* `bg_` job | refused: wrong token at B's socket, and "did not start job" at A (f1-f3, real id). Succeeds only through B's token file (S1) or an id collision (S2) |
| (c) learn the token | absent from events.jsonl, blobs, `commands/*/out`, bg status/out, every state file, all process command lines (CIM, hash-matched with `grep -f`), pane scrollback, TUI screen, `ctl` output, and error text (refusals never echo it). Not in the env of an unrelated shell. (My first run showed hits because my test woke the agent, which `cat`-ed the token file itself; the clean re-run is empty.) |
| (d) steer without token | refused (see (a)); with the token file it works (documented trusted) |
| (e) escape `bg/` | all refused (live, Windows): `..`, `../..`, session dir, other session's `bg`, directory junction (+ subdir), temp dir, drive root, `\\localhost\c$`, `\\?\C:\`, `//host/share`, `D:\`, `..\..`, sibling-prefix `bg_evil`; `out` outside / dotdot / junction / UNC refused; ids `../x`, `a/b`, `a\b`, `a:b`, 300 chars refused. No marker file written by an escaped spawn. The case variant `BG` resolves to the same dir and is rightly accepted. |
| (f) stale token after restart | `--resume` rotated the token (new file content differs from the old). The listener compares only with the in-memory token, so the old one is refused (my direct stale-token probe failed on a path error in the script, so this rests on the wrong-token live cases plus code reading). |

## Other acceptance checks
- Token: OS RNG, 256 bits, in memory; reaches children only via `Command::env` (bash children, the shim); argv scan empty; each session calls `Trust::create`, so a child session's token is distinct.
- `kelir ctl` works via the token file on Windows (`say`, `end`; `cancel` of an unknown id refused with a clear error) and on Linux (`end`).
- Windows ACL: `icacls token` shows only `a Windows box\the maintainer:(F)`, no inheritance. Linux (the maintainer, session started under umask 000): session dir 700, `sock` 600, `token` 600, `sessions/` 700; a second shell with umask 000 ran the stranger attacks, all refused.
- Not tested: a genuine second Linux user (the maintainer has one account, `sudo` needs a password); modes plus a umask-000 second shell stand in. Go provider is not live-verified (no key).
- Regressions: M4 shim reporting / promotion / wake / cancel / orphans, KELIR_SOCK in bash children, STEER-5 verbs and TUI verbs all green in the suites. TUI live pane: `kelir bg run` and `bg ls` worked, `ctl say` queued and showed. Test scrub of inherited `KELIR_*` still holds.

## Live-drive log (OpenRouter `deepseek/deepseek-v4.1-flash-1017`, ~6 short runs)
1. Two sessions; A's model ran `bash child_wrap.sh` which ran `steer7_attack.py child`. B1/S1/S2 reproduced; escapes refused.
2. Console-less `stranger`, `reader`, `foreign` attackers against A and B. Stranger: 0 exploits.
3. Clean pair (no waking `say`): hash search for both tokens over files, argv and pane: 0 hits.
4. `--resume` rotation. 5. TUI pane. 6. Linux attack on the maintainer (bogus key, idle session).
Setup note: the first launch used the wrong default provider (opencode-go); fixed with `--provider openrouter`.

## Added
`tests/review_steer7.rs` (2 failing tests prove B1), `scripts/live/steer7_attack.py`, `scripts/live/steer7_setup.sh`.
Re-review needs: B1 fix with those 2 tests green, S1 documented or fixed.
