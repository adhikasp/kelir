# STEER-2 fixes — implementation notes

Brief: `docs/tasks/STEER-2-fixes.md`; findings: `docs/reviews/steer2-review.md` (PASS WITH FIXES,
3 should-fix, 3 nits). All six are addressed, and the reviewer's 13 red/edge tests in
`tests/review_steer2.rs` pass unweakened (no edit to that file).

## 0. Merged `main` first

`git merge main` (STEER-10 fixes2, the STEER-2 brief itself), docs only, no conflicts. Committed as a
merge on `job/steer2` before anything else, as the brief says.

## 1. Finding 1 (should-fix) — an entry expired on the first read is re-read once

The expiry check moved out of `parse_auth_export` and into `store_credential`:

- `parse_auth_export(json)` no longer takes `now_ms` and no longer rejects a stale entry; it reports
  what the store says (`access`, `expires_at_ms`). A stale token is data, not a parse error.
- `store_credential` reads, and when the credential is expired **or inside `EXPIRY_MARGIN`** it sleeps
  `REREAD_DELAY` (250 ms — opencode writes the rotated token as a side effect of running, so an
  immediate second read can race that write) and reads once more. The fresh read wins if it is
  outside the margin and carries a different expiry. If it is still expired: the usage error naming
  the expiry and `REFRESH_FIX`, before any request. Exactly two reads, ever.
- Unit tests pin all three shapes: expired-then-fresh recovers, still-expired names the time and the
  fix (and reads exactly twice), and a second read with a *different but still past* expiry does not
  produce a third read. E2E `a_store_entry_expired_on_the_first_read_recovers_on_the_re_read` drives
  the fake `opencode` through the real binary and asserts the fresh token is the one on the wire and
  neither token reaches any output.
- `an_expired_store_credential_fails_with_the_fix` (STEER-2's own test) still passes unchanged: the
  fake returns the same expired export twice, so the error path is the same, just after two reads.

## 2. Finding 2 (should-fix) — `key_env` is a fallback, not a replacement

`resolve_with` builds the candidate list **default variable first, then `key_env` when it names a
*different* variable**, then `key_cmd`, then (Go only) the store. For OpenRouter that is
`OPENROUTER_API_KEY` → `key_env` → `key_cmd`, exactly SPEC.md:338. For Go it is `KELIR_GO_API_KEY`
→ `key_env` → `key_cmd` → store, which keeps the override semantics in the same list. The unit test
that pinned the old replacement (`MY_GO_KEY` beating a set `KELIR_GO_API_KEY`) was corrected to the
SPEC order, and a new e2e proves both halves for OpenRouter with a real config file
(`key_env` naming an unset variable does not hide `OPENROUTER_API_KEY`; with it unset, `key_env`
answers). §DEVIATIONS has the entry, SPEC.md already said this, so SPEC text is unchanged except
the Go bullet now spells the order out.

## 3. Finding 3 (should-fix) — the migration message names the table and the file

`config::load` now records the files it merged (`sources: Vec<PathBuf>`, explicit path or the machine
+ project pair) and an unknown `[providers.<name>]` table returns
`unknown_provider_table(name, &sources)`:

```
`[providers.kelir]` in <path>: unknown provider `kelir`: the table name *is* the provider and there
are exactly two, `openrouter` and `opencode-go` (`kind = "…"` is no longer used). Rename the table to
`[providers.openrouter]` or `[providers.opencode-go]`, then check `provider = "…"` in the same file;
`key_env` keeps working
```

The short `unknown_provider(name)` ("names the pair") stays for `provider = "…"` and `--provider`,
where there is no table. `kelir doctor` still exits 2 on the bad config — it cannot report a config
it cannot load — but the message now tells the maintainer the exact edit. E2E
`an_unknown_provider_table_says_where_it_is_and_what_to_rename` runs `run`, `doctor` and `ask`
against the maintainer's shape (`[providers.kelir]`, `kind = "openrouter"`, `key_env`) and asserts the
table, the path, the pair and the rename sentence in every one, with no request made.

## 4. Nit 4 — spaces eaten by line continuations

Both warnings are single strings again: "...use the environment (OPENROUTER_API_KEY / KELIR_GO_API_KEY)
or `key_cmd = ["cmd", …]`" and "...use `key_env`, `key_cmd`, or the environment". A backslash
continuation at the end of a Rust string literal eats the following indentation, so the space now
lives at the start of the next line.

## 5. Nit 5 — `doctor` fails only for a provider that matters

Exit 1 (and `"ok": false` in JSON) only when the **selected** provider has no usable key, or when any
provider's credential is **expired**. A non-selected provider with no key is reported on its own line
as information. That is the difference between "this machine runs Go and has no OpenRouter key"
(info) and "the Go credential died" (a real fault that would stop this session). To make the second
case visible, the error branch now sets `expired: true` when the message is the expiry one. E2E
`doctor_does_not_fail_for_the_provider_this_session_does_not_use` covers both: `--provider opencode-go`
on a Go session (exit 0, one provider) and both providers with Go selected and desktop-key absent
(exit 0, `FAIL openrouter` + `ok opencode-go` in one report). The reviewer's
`doctor_with_expired_store_fails_clearly_without_secret` still gets exit 1.

## 6. Nit 6 — the helper's stdout is bounded

`run_command` is now a thin wrapper over `run_command_bounded(argv, timeout, max_stdout)`, with
`MAX_HELPER_STDOUT = 64 MiB`. The reader thread reads `take(max + 1)` bytes and sets an `AtomicBool`
on overflow; the wait loop watches that flag, so the child is killed **as soon as it outruns the
bound** rather than at the 5 s timeout — otherwise the child would sit blocked writing into a pipe
nobody drains, which is exactly the leak AGENTS.md forbids. The kill is guarded by `try_wait` so a
reaped pid is never signalled. 64 MiB, not 1 MiB: the reviewer measured a real 30 MB padded export,
and the bound is a memory guard, not an expectation. Unit test drives it through the same path with a
4096-byte bound (fast, no 64 MB allocation) on both platforms.

## Gates

`bash scripts/gate.sh --linux=the maintainer`: fmt, clippy `-D warnings`, nextest 696 run / 696 passed
(9 skipped), doctest, and the Linux half. Note: the first gate's Linux half ran against the
*unfixed* HEAD because the remote check checks out `HEAD_SHA` from a git bundle — uncommitted work is
not exported. The commit was made and the gate re-run; see the commit message for the result.

## What I did not do

- Did not touch `tests/review_steer2.rs` (the reviewer's tests are the acceptance criteria).
- Did not add a `--all` flag to `doctor`: `--provider` narrowing plus the exit-code rule covers nit 5
  without new surface (documented in DEVIATIONS).
- Did not make `doctor` report a non-selected provider's *absence* as a warning line beyond its
  existing `FAIL` mark; a "not configured" mark would be a new vocabulary for the same fact.
