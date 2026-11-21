# canary-flake — root cause of the four RED-gate canaries

Branch `job/canary-fix`, base `main 058d3ca`, fix commit `0eb9609`. Not a regression:
the same four failed 3/3 in `gate-job-bashdescfix-20261121T011646Z` (commit `d984b86`,
1369 tests / 4 failed / 3 skipped, `gate: FAIL`, Linux half ok) while
`gate-job-m10fixrev-20261121T014431Z` was green at `96594d5` (1387 passed / 3 skipped)
and `gate-job-m11fixrev-20261121T060708Z` was green at current-main lineage
(1432 passed / 3 skipped). Nothing in M10/M11/steer9 touched these tests' assertions
or the hardening path; the flips are test-side, not product-side.

## Root cause per test

### 1. `review_steer7c a_same_user_child_cannot_read_the_shims_key_from_its_memory`
The memscan (`scripts/live/steer7c_memscan.py`, `ReadProcessMemory`, DER PKCS#8 EC
prefix + 134 hex = 166 hex chars) asserts the shim's signing key is NOT readable.
SPEC, *Who may drive a session* / *What this does not cover* (`docs/SPEC.md:118-139`),
explicitly says a same-user process that reads another process's memory is INSIDE
the boundary; the DACL is best effort and explicitly does not defeat the user's own
tools. `docs/reviews/steer7d-review.md` S2 records the same finding and SPEC already
accepts it. The assertion is therefore a canary for an out-of-boundary property.
It flips because whether the DACL (`deny_same_user_memory_access`, best effort,
stderr warning only) was applied before the scan, and whether `OpenProcess` succeeded,
depend on process layout and load: full-gate failures log `FOUND key of 166 hex
chars in pid … (31-39 regions read) -> forged settlement in log: true`; an isolated
nextest run passes with `NOT FOUND (OpenProcess failed: 5)` and no key file.

### 2. `review_steer7d a_same_user_child_cannot_read_job_keys_from_the_sessions_memory`
Same cause, aimed at the session (`Trust.jobs` holds every key for the session's
life). SPEC says the session gets the same best-effort DACL and that deliberate
same-user memory reads remain inside the boundary. Failures log `scan of the session
process: FOUND key of 166 hex chars in pid … (37-41 regions read)`; green runs scan
fewer regions or fail to open the process. The property can never be guaranteed
without breaking the documented harness kill/query rights (`taskkill` needs
`PROCESS_QUERY_INFORMATION`), so option 2 is not available as a product change.

### 3. `review_steer7d a_spawn_with_another_kind_still_gets_a_shim_closed_to_memory_reads`
Same cause, aimed at a `bg-spawn` that supplies `kind: "x"`. The real fix already
shipped in steer7fix4: `SpawnRequest::pinned` (`src/bg.rs:748-760`) overwrites the
caller's kind with `job`, pins sock/token/job, and the session honours only cmd,
shell, cwd and a filtered env (`docs/SPEC.md:82-92`). Full-gate logs show
`"kind":"job"` in the spawned status and still `FOUND key ... in pid …`, i.e. the
memory read is the *only* failing part and it is out of boundary. The
`kind=x` hardening probe additionally depends on the DACL timing described above.

### 4. `e2e_m7 registry_operations_need_the_childs_link_and_no_secret_travels`
Two load-sensitive assertions, both test-side:
- The env-dump proof read `env | sort` from the `tool_settled` **result** preview,
  which spills to `[output spilled: …] --- first 1024 bytes --- … --- last 1024
  bytes ---` (`src/spill.rs`, default `spill_limit = 10240`). Sorted output puts the
  `KELIR_*` lines in the omitted middle. Under nextest each test process inherits a
  different `KELIR_*`/`CARGO_*` set, so the dump crosses 10 KB on some runs; the old
  gate panic at `tests/e2e_m7.rs:1123` shows exactly the 10,629-byte spill preview
  with no `KELIR_ROOT=`. `cargo test` (one binary at a time, smaller env) stays under
  the limit. The probe's *full* output is the child's `commands/<id>/out` file.
- Earlier gates (`gate-job-m7-20261120T131321Z`) failed at old line 1086 with
  `a child token in …\sessions\ses_…\commands\6-1-1\out`: the token-scope check used
  `any(component == owner)`, while that path's own components are `commands` and
  `6-1-1`, so a token appearing in a sibling's tool output could be mis-scoped
  depending on the env inherited by the child. Now the check extracts the `ses_…`
  component and compares it to the token's owner, tightening (not weakening) the
  cross-session guarantee.

## Option chosen and why — 1 (reframe), with an event-driven fix for e2e_m7

- For the three memory canaries, option 1 is the only honest one: option 2 would
  require removing the harness's kill/query rights that SPEC explicitly reserves,
  and cannot close an inside-boundary path anyway. The probes are kept verbatim as
  `#[ignore = "out of SPEC boundary: same-user ReadProcessMemory is trusted by design;
  probe only"]` so `cargo nextest run --run-ignored only` still measures and reports
  FOUND/NOT FOUND. Their finding remains documented in `docs/reviews/steer7c-review.md`
  and `docs/reviews/steer7d-review.md` S2; the script `scripts/live/steer7c_memscan.py`
  is unchanged.
- Each ignored probe gained a *live* regression test that asserts the real,
  in-boundary guarantee: `the_shims_signing_key_stays_out_of_every_exposed_channel`
  (7c) and `a_spawned_job_pins_its_kind_and_never_exposes_the_key` (7d) check that
  the PKCS#8 prefix never reaches a verb answer, `events.jsonl`, an argv/`bg ls`
  output, and that `kind` is pinned to `job`. No security guarantee was removed.
- For `e2e_m7`, option 3 (STEER-9 method): the assertion now reads the authoritative
  full `commands/<id>/out` file, bounded by the same 90 s `CEILING`, while still
  checking the model's preview is bounded and secret-free and cross-session tokens
  never cross paths. It proves strictly more than before (full env + preview bounds).

## 3x nextest evidence (branch, `--retries 0 -j 4`)

Target filter (three runnable replacements; the three out-of-boundary probes are
the ignored originals):

| run | result |
|---|---|
| 1 | `3 tests run: 3 passed, 1436 skipped` — `canary-3x/run1.log` |
| 2 | `3 tests run: 3 passed, 1436 skipped` — `canary-3x/run2.log` |
| 3 | `3 tests run: 3 passed, 1436 skipped` — `canary-3x/run3.log` |

`3/3 green, zero retries`. The preserved probes were separately run with
`--run-ignored only -E 'test(a_same_user_child_…) + …'`:
`3 tests run: 3 passed, 1436 skipped` (`canary-probe/out.log`) — this is the probe
reporting its measurement, not a gate assertion.

## Full gate (`bash scripts/gate.sh --linux=the maintainer`, commit `0eb9609`)

```
gate: 0eb9609 canary-fix: frame memscan probes as out-of-boundary; make e2e_m7 env proof spill-proof
  fmt: ok (2379 ms)
  clippy: ok (716 ms)
  nextest: ok (222594 ms)
  result: Summary [ 220.461s] 1433 tests run: 1433 passed, 6 skipped
  doctest: ok (2375 ms)
  linux (the maintainer): ok (268232 ms)   # 1435 tests run: 1435 passed, 3 skipped
gate: PASS
```

Zero retried/flaky tests (`grep -i 'RETRIED\|TRY 2\|FLAKY'` on `nextest.log` matches
only test *names* containing "retried"). No leaked `kelir.exe` from this run; the
only remaining one predates this session (PID 72024, started 11:52). No processes
were signalled by group, none by image name; gate lock serialised all runs.
