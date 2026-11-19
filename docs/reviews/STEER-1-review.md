# STEER-1 review — model defaults and provider model metadata

Reviewer: opencode session (pane t9p), 2026-11-19. Scope: the maintainer's three commits as they
stand at HEAD `1dc2021` — `c8f6b11` (default model + window), `0f1bbba` (pin `-1017`), `ad8ec8f`
(metadata from the provider) — against `docs/tasks/STEER-1.md`, `docs/SPEC.md` §"Providers" /
§"Compaction", `docs/DEVIATIONS.md`. Later commits (`05d1d16`, `6988181`, … `1dc2021`) touched none
of `src/models.rs`, `src/session.rs`, `src/config.rs`'s window code, `src/compact.rs`,
`src/provider.rs`, so the code below is the code those three commits left. HEAD moved while the
review was running: `52942cf` changed only `DEFAULT_PROMPT` in `src/config.rs` (not the window
code), `4468b8b` and the two review commits are docs/tests — none of STEER-1's behaviour changed.

## Verdict: PASS WITH FIXES

0 blockers, 3 should-fix, 5 nits.

The change does what its commit message says: the per-family table is gone, OpenRouter's
`context_length`/`supported_parameters` and Go's `limit.context`/`capabilities` are both read, the
list is cached for a day with an hourly backoff, `state` and the TUI banner carry
`window`/`window_source`, and a session never fails because of the metadata fetch (401, 403, 500,
malformed, huge, slow and hanging endpoints all fell back to `default` with the session intact;
evidence below). The default model resolves to 1 048 576 tokens from the live provider list.

The two things worth fixing before this is called done are both about *where* that fetch happens
and *what* the cache belongs to: it blocks the first request for ~0.6 s (and hit its own 1.5 s cap
once in three cold starts on this network, silently landing on the 128 k default — the very
"compact at a tenth of the window" bug STEER-1 exists to kill), and `models-<provider>.json` is
keyed by the provider *name*, so a second endpoint (`KELIR_BASE_URL`, a proxy, a changed
`base_url`) inherits the first one's numbers.

## Gates

HEAD `1dc2021`, this machine (Windows 11), before my additions:

| gate | result |
|---|---|
| `cargo fmt --check` | clean (exit 0) |
| `cargo clippy --all-targets -- -D warnings` | clean (exit 0) |
| `cargo test` | **389 passed, 0 failed**: 262 lib, 0 main, 16 e2e, 11 e2e_m3, 10 e2e_m4, 15 e2e_tui, 23 e2e_verbs, 8 review_m0, 5 review_m1, 2 review_m2, 11 review_m3, 19 review_m4, 7 review_m4b |

Linux (`bash scripts/remote-check.sh the maintainer`, HEAD `1dc2021`): `FMT-OK`, clippy clean, **394
passed**: 267 lib, 16 e2e, 13 e2e_m3, 8 e2e_m4, 15 e2e_tui, 23 e2e_verbs, 8 review_m0, 5
review_m1, 2 review_m2, 11 review_m3, 19 review_m4, 7 review_m4b. (The lib/e2e_m3/e2e_m4 counts
differ by platform because five unit tests and two e2e tests are `cfg`-gated; the totals agree.)

After my additions (`tests/review_steer1.rs`, one assertion in `tests/e2e_tui.rs`): `cargo fmt
--check` clean, clippy clean, **395 passed, 0 failed, 2 ignored** on Windows — on both the review
commit `d02c87c` and `fe77ed4` (HEAD at report time) — and on Linux
(`scripts/remote-check.sh the maintainer` on `d02c87c`) `FMT-OK`, clippy clean, **400 passed, 0 failed, 2
ignored**. The two ignored tests are the findings below and fail with `-- --ignored`.

## Findings

| id | severity | where | finding |
|---|---|---|---|
| S1 | should-fix | `src/models.rs:166-172`, `src/session.rs:564,1418` | the cache file is `models-<provider>.json`: keyed by the provider *name*, not the base URL, so a second endpoint reuses the first one's list — and an unreachable base URL does **not** fall back to `default` on a warm box |
| S2 | should-fix | `src/session.rs:570`, `src/provider.rs:169-191` | the fetch is awaited on the critical path in `prepare` with a 1.5 s cap over a ~930 KB body: +0.64-0.69 s per cold start, and the cap was hit once in three cold starts, silently leaving the 128 k default for an hour |
| S3 | should-fix | `src/models.rs:187-194` | the cache is written with `std::fs::write`, not temp-file + rename: a reader between truncate and write sees no cache, a crash mid-write leaves a broken file |
| N1 | nit | `src/provider.rs:170` | `{base}/models` breaks when `base_url` already ends in `/chat/completions` (a shape `provider.rs` supports and tests): it asks for `/v1/chat/completions/models` and falls back to `default` forever |
| N2 | nit | `src/models.rs:60-68` | `saturating_sub` + a future `failed_at_ms` = "in backoff" until the clock catches up, so a clock that was ahead once stops the refresh for as long as the skew |
| N3 | nit | `src/models.rs:75-93` | the last-segment fallback in `lookup` can borrow another vendor's entry with the same last segment; the doc comment's parenthetical reads as if it cannot |
| N4 | nit | `src/session.rs:576-582` | every fetch failure is dropped (`let _ =`), so `state` says `default` with no reason (401 vs offline vs malformed) and nothing is logged |
| N5 | nit | `src/models.rs:166` | the provider name is interpolated into a file name unvalidated; a config name containing `/` or `..` writes outside the state dir (self-inflicted, but cheap to key on a slug) |

### S1 (should-fix) — the cache belongs to a provider name, not an endpoint

`cache_path_in` builds `<state>/models-{provider}.json` (`src/models.rs:166-172`) and `load_models`
passes `provider_name` (`src/session.rs:564`, the `--provider`/config name, not `base_url`).
`Config::provider` deliberately lets `KELIR_BASE_URL` override `base_url` for "a stand-in for
either provider (the mock server in tests, a proxy)" (`src/config.rs:266-272`), and a config can
change `base_url` between runs. All of those share one cache.

Failing scenario (measured, `review_steer1::a_cache_from_another_endpoint_is_not_reused`, ignored
and failing):

* endpoint A lists `test/model` at 262 144 → cache written, `window=262144 (api)`;
* the same state dir is used against endpoint B (a gateway that serves the same model at 8 192):
  the second start reports `window=262144 (cache)` and never asks B — `left: (262144, "cache"),
  right: (8192, "api")`.

Same thing live, with the brief's own criterion (b): after one real run in `states/s1a`, pointing
`KELIR_BASE_URL` at an unreachable `http://127.0.0.1:9/v1` still gave
`window=262144 window_source=cache` (not `default`). On a 1M cache against an 8 k endpoint,
auto-compaction would never fire before the provider's 8 k limit — a session failure, not just a
wrong number.

### S2 (should-fix) — the fetch is on the critical path and its budget is under the real payload

`prepare` awaits `load_models` before `session_started` (`src/session.rs:669`, `:601`), so the first
provider request waits for it; the wrapper timeout is 1.5 s (`:570`) and `provider.models()` reads
the whole body (`src/provider.rs:186-189`). Today's OpenRouter `/models` is **929 936 bytes** (455
models; the cache keeps 29 374 bytes of it).

Measured on this box (release binary, live key, `scripts/live/models_probe.py` for the local
control), spawn → first `session_started` line:

| start | ms |
|---|---|
| `kelir --version` (process baseline) | 16 / 31 / 16 |
| cold, fresh state dir 1 | **1515** (fetch hit the 1.5 s cap; cache 84 B = failure; window `default` 128 000) |
| cold 2 | 703 |
| cold 3 | 656 |
| warm (cache) 0/1/2 | 16 / 47 / 16 |

So the added cold-start latency is ~0.64-0.69 s, ten times the brief's `<100 ms` target, and the
tail is worse than the cap: one of three cold starts produced the 84-byte failure cache
(`{"provider":"openrouter","fetched_at_ms":0,"failed_at_ms":…,"models":[]}`) and therefore a 128 k
window for a 1 048 576-token model, kept for an hour by the backoff. On a slow link a 930 KB body
will always lose that race; the fallback is safe but it silently disables the feature it is there
to provide. The fetch also runs when `window`/`[model_windows]` is configured and cannot be
overridden by the result (`src/models.rs:213-215` answers `config` first), so those users pay the
latency for nothing. It is `await`ed rather than overlapped: the window is only needed at the first
`maybe_compact`, after the first step, so it could be fetched in parallel with the first request
and applied when it lands (or the previous/default window used for this start, with the list
written for the next).

### S3 (should-fix) — the cache write is not atomic

`store_in` is `create_dir_all` + `std::fs::write` (`src/models.rs:187-194`): no temp file, no
rename, no fsync. `load_in` tolerates it (`serde_json::…ok()?`, `:178-181`), so the failure is
silent: a concurrent start that reads between truncate and write gets `None` and falls back to
`default`; a process killed mid-write leaves a broken file until the next start refetches. My
race probe (8 simultaneous starts rewriting a 115 KB cache, one reader doing 7 205 reads at ~1 ms
intervals) saw `reads=7205 ok=7205 empty=0 broken=0`, so this is hard to hit on NTFS in practice —
the code is still not the atomic write the brief asks for, and the fix is three lines.

### N1 (nit) — full-path `base_url` builds the wrong `/models` URL

`chat_completions_url` accepts `base_url = "http://host/v1/chat/completions"` (test
`base_url_with_full_path_is_left_alone`), but `models()` blindly appends (`src/provider.rs:170`).
Measured against the local probe: `GET /v1/chat/completions/models` → 401 → failure cache →
`window=128000 default`. A strict provider would 404, which is equally silent. Either strip a
trailing `/chat/completions` or derive `/models` from the same helper.

### N2 (nit) — a future timestamp locks the fetch out

`is_fresh`/`in_backoff` compare with `now.saturating_sub(...)` (`src/models.rs:60-68`), so a cache
written while the clock was ahead (VM restore, a dual-boot clock, an NTP correction) reads as
"just failed" until real time passes the file's stamp: `in_backoff` stays true, so the list is
never refreshed and the window stays `default` for the whole skew. `review_steer1::a_cache_dated_in_the_future_does_not_lock_the_fetch_out`
(ignored) records it; a `now >= at` guard is enough.

### N3 (nit) — last-segment fallback can cross vendors

`lookup`'s third step matches on the last `/`-segment (`src/models.rs:87-92`). In one provider's
list that can pick `fireworks/deepseek-v4.1-flash-1017`'s context for a `deepseek/deepseek-v4.1-flash-1017`
query when the exact id is absent (the ids differ only in vendor). The doc comment above it
(`:75-77`) reads as if this cannot happen, which is what makes it worth a line. Harmless today (the
two entries agree) and invisible when it is not.

### N4 (nit) — a fallback has no reason

The fetch error is thrown away (`let _ = crate::models::store(...)`, `_ =>` at
`src/session.rs:576-582`), so nothing recorded or printed says why a start used `default` —
`state` shows only `window_source: "default"`. On the upside this is why nothing leaks (below).

### N5 (nit) — provider name → file name, unvalidated

`models-{provider}.json` (`src/models.rs:166`) with a config provider key like `..\x` writes
outside the state dir. It is the user's own config file, so it is self-inflicted; a small charset
check would close it.

## The review questions, answered

**Failure behaviour.** All variants tested against a controllable base URL
(`scripts/live/models_probe.py`) with a fresh state dir; every one: session ran, `window=128000
window_source=default`, one `/models` request, failure recorded (`failed_at_ms`) so the next hour
is quiet:

| `/models` answer | session | state | cache |
|---|---|---|---|
| 401 (`p8792`) | done, `probe: done` | 128000 default | `models:[]`, `failed_at_ms` set |
| 403 (`p8798`) | done | 128000 default | same |
| 500 (`p8793`) | done | 128000 default | same |
| malformed JSON (`p8794`) | done | 128000 default | same |
| slow:3000 ms (`p8795`) | done; session_started at 1560 ms | 128000 default | same |
| hang forever (`p8797`) | done; models→completion gap 1510 ms | 128000 default | same |
| 200 000 models → 2 000 (`p8796`) | done | 128000 default (test/model not listed) | 115 846 B, 2 000 models |
| unreachable `127.0.0.1:9` | session started, step failed on its own (no provider) | 128000 default | `fetched_at_ms:0`, `models:[]` |

No hang beyond the 1.5 s bound; no session failed *because of* the metadata. A 1 MB real list is
read whole (`scripts/live/models_probe.py` 2 000-model case and live); the parser keeps only
`id/context/tools` (`src/models.rs:134-163`).

*Secrets/headers.* A canary key (`sk-steer1-canary-fake`) was used for every local run; a grep of
all state dirs, session logs, stdout/stderr and probe logs for the key, `Bearer` and the value
found nothing outside the probe's by-design header *names*. The cache holds `provider`,
`fetched_at_ms`, `failed_at_ms` and `id/context/tools` only (`/tmp` example:
`{"provider":"openrouter","fetched_at_ms":…,"models":[{"id":…,"context":1048576,"tools":true},…]}`).
A completion 401 (`p8799`) reaches `events.jsonl`/stderr as `http 401: bad key` — the provider's
body, no request headers.

*Atomicity / corrupt cache.* Not atomic (S3). A hand-written corrupt/empty cache is tolerated:
`load_in` returns `None` and the start refetches (covered by the 401/500 cases, which write a
truncated-looking `models: []` and are then read normally).

**Cache keying.** Per provider *name*, not base URL (S1). 24 h and 1 h rules are unit-tested with
an injected `now_ms` (`src/models.rs:281-296`: `is_fresh` at TTL−1/TTL, `in_backoff` at
RETRY−1/RETRY) — good. Two processes starting at once: 8 concurrent starts against a stale cache
and the race reader (S3) — all exited 0, final cache valid with 2 000 models. Clock skew: S2/N2.

**Latency.** Measured above (S2): warm 16-47 ms total, cold 656-703 ms and one 1515 ms cap hit.
Not under 100 ms; not overlapped with the first request, though it could be (the window is first
read in `maybe_compact`, after step 1).

**Prompt-cache/fold determinism.** The window is never serialised into a request:
`build_request` (`src/provider.rs:75-125`) has no window parameter, and `self.window` is read only
in `maybe_compact` (`src/session.rs:1566`). Test
`review_steer1::the_window_does_not_change_the_request_bytes` runs the same task twice with a
shared `--cwd`, once on the provider's 262 144 and once with `window = 4096`, and asserts the two
recorded request bodies are byte-identical after removing `session_id`. Passes.

**Compaction arithmetic at 80 %.** `should_auto` is `tokens >= ceil(window * at)`
(`src/compact.rs:44-51`). At the live 1M window the first triggering count is 838 861 (838 860 is
below); with `window = 8192` it is 6 554; with the 128 k default 102 400. Unit test in
`review_steer1::the_auto_trigger_scales_with_the_window`; the e2e that really compacts at a small
window is `e2e_m3::the_auto_trigger_compacts_at_eighty_percent_of_the_window`.

**Public-repo hygiene.** At HEAD the name is gone from the worktree: `docs/SPEC.md:296` and
`docs/DEVIATIONS.md:9` say "the maintainer", scrubbed in `f2e5b4e`. It is still in **history**
(`ad8ec8f`'s diff added "the maintainera's call", `c8f6b11`/`0f1bbba` messages say the same), which is the
scrub task's business, not a code fix. Separately, `docs/QUEUE.md:48` still carries the private
path `C:/Users/the maintainer/kelir-scratch/live.env` (no key, but a personal path in a public repo; it
came in with `1e23abd`, outside these three commits).

**Windows + Linux + live.** Gates on both (above). Live: below. Go path: unit/e2e only
(`KELIR_GO_API_KEY` is not available here) — **not live-verified**.

## Live drive (OpenRouter, `deepseek/deepseek-v4.1-flash-1017`)

Release binary built with `cargo build --release` (6 263 808 bytes). All runs in Herdr panes, key
sourced inside the pane from the env file named in my brief and never printed; scratch
`C:/Users/the maintainer/kelir-scratch/STEER1`. Total spend: twelve short turns (~8 800 prompt tokens, ~46
completion tokens, ≈$0.0009 by the `cost` field kelir recorded).

**(a) metadata fetch, then the cache.** Start 1, fresh `live-state`:

```
1232ms {"seq":1, … "type":"session_started", … "provider":"openrouter","model":"deepseek/deepseek-v4.1-flash-1017" …}
1863ms {"seq":4, … "type":"assistant", … "text":"steer1-ok" …}
```
```
$ kelir ctl latest stop
{"ok":true,"verb":"stop",…,"state":{…,"window":1048576,"window_source":"api"}}
$ ls -l live-state
-rw-r--r-- … 29374 2026-11-19 11:17:12.190569300 +0700 models-openrouter.json
```
455 models cached; `{"id":"deepseek/deepseek-v4.1-flash-1017","context":1048576,"tools":true}`.

Start 2, same `live-state`:

```
cache_before: … 29374 2026-11-19 11:17:12.190569300 +0700 …models-openrouter.json
140ms {"seq":1, … "session_started" …}      <- no 1.2 s fetch
```
```
{"ok":true,…,"state":{…,"window":1048576,"window_source":"cache"}}
cache_after:  … 29374 2026-11-19 11:17:12.190569300 +0700 …models-openrouter.json  (unchanged)
```
The local mock counts the requests: `models_requests` stays 1 across the two starts
(`review_steer1::the_provider_list_is_fetched_once_and_the_next_start_uses_the_cache`).

**(b) unreachable base URL.** `KELIR_BASE_URL=http://127.0.0.1:9/v1`, fresh state dir:
`session_started` after the 1.5 s cap, turn then fails on its own transport error (there is no
provider behind it), and `state` = `{"window":128000,"window_source":"default"}`, cache
`{"fetched_at_ms":0,"failed_at_ms":…,"models":[]}`. No hang, no startup failure. With a warm cache
from another endpoint it reports `cache`, not `default` — that is S1.

**(c) cold vs warm latency.** Table in S2: warm 16/47/16 ms, cold 656/703/1515 ms. The 1515 ms one
is the cap firing and the session running on 128 000 for the 1 M model.

**(d) 401 and 5xx, no leaks.** Local probe (`base_url` I control) returning 401 on `/models`
(p8792), 403 (p8798), 500 (p8793) and 401 on the completion (p8799): sessions ended `done` (or
`ask` exited 1 with `kelir: http 401: bad key` for the completion case); grep of the state dirs,
`events.jsonl`, stdout/stderr and the cache for `sk-steer1-canary-fake`, `Bearer` and
`authorization` found only the probe's header *names*; nothing else.

Concurrency/leak check: 8 simultaneous `kelir ask` against the probe with a stale cache — all
`probe: done`, cache final and valid (`2000 models`). Before my runs `tasklist` showed 3
`kelir.exe` (other panes); after killing my panes: no `target/release/kelir.exe`, no probe python,
no `hc` pane of mine left (the one remaining `kelir.exe` is another pane's installed binary,
`~/.cargo/bin/kelir.exe`, parent an opencode/pwsh pane). No visible console windows: every server
and drive ran in a Herdr pane or as a child of my pane.

## Tests added (reviewer)

- `tests/review_steer1.rs` — 6 passing probes (fetch once then cache; 401/500/malformed fall back
  to `default` without failing; a 1 500-model list read to the end; `window` wins over the
  provider; identical request bytes under different windows; the 80 % arithmetic at 1 M / 8 k /
  128 k) and 2 ignored probes that record S1 and N2 and fail on purpose
  (`cargo test --test review_steer1 -- --ignored`).
- `tests/support/mod.rs` — `ModelsAnswer`/`Mock::start_with_models` (the mock can answer `/models`
  with an error, a malformed body or a list of the test's own), `Mock::models_requests()`, and
  `LiveSession::start_reusing(state, …)` so a second start can share a state dir.
- `tests/e2e_tui.rs` — the PTY test now asserts the banner line
  `window 262144 tokens (api)`; the banner addition had no test.
- `scripts/live/models_probe.py` — the controllable base URL used for the failure matrix (modes
  `ok/go/unauth/forbidden/server/malformed/slow:N/hang/huge:N/completion401`; logs arrival times
  and header *names* only, never values).

## Not verified / open

- The Go provider live (`KELIR_GO_API_KEY` absent): shape parsing is unit-tested
  (`models::the_go_shape_is_read`) and the mock serves it; no real Go `/models` was fetched.
- A 24 h expiry and the hourly retry across real wall-clock time: unit-tested with an injected
  clock only (which the brief allows); no test waits an hour.
- Whether the 1.5 s cap is hit on other networks — one hit here in three is enough to matter, but
  it is a sample of three.
