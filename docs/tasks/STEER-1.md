# STEER-1 — model defaults and provider model metadata (maintainer-steered change)

Not a spec milestone: three commits made on demand by the maintainer outside the orchestrated
loop (c8f6b11, 0f1bbba, ad8ec8f). It still goes through the same gates and a fresh review.

## What changed
1. Default model is `deepseek/deepseek-v4.1-flash-1017` on OpenRouter (Go: `deepseek-v4.1-flash`).
2. `src/models.rs`: reads the provider's published model metadata (`GET {base}/models`, both the
   OpenRouter shape — `context_length`, `supported_parameters` — and the Go shape — `limit.context`,
   `capabilities`), caches it in the state dir for 24 h with an hourly backoff after a failure.
3. The compaction window comes from that metadata; `[model_windows]` / `window` config still win;
   the old per-family table is gone. `state` gains `window` and `window_source`
   (`config|api|cache|default`); the TUI banner shows them. The test mock serves `/models`.

## Review questions
- Failure behaviour: offline, 401/403, 5xx, malformed JSON, huge response (hundreds of models),
  slow endpoint: session start must not hang or fail; fall back to `default` window; never log or
  cache secrets/headers; cache file written atomically; corrupt cache tolerated.
- Cache keying: per provider/base URL; two kelir processes starting at once; clock skew; 24 h/hourly
  rules actually tested with an injectable clock.
- Does fetching metadata at session start add latency to the first request (cold start target
  <100 ms for `ask`)? Is it async/non-blocking where it can be?
- Prompt-cache and fold determinism untouched: the window value must not alter request bytes.
- Compaction trigger arithmetic at 80 % with a 1M window and with a small configured window.
- Public-repo hygiene: SPEC/DEVIATIONS mention a person by name — flag (the scrub task handles it).
- Windows + Linux (`scripts/remote-check.sh`), real provider (live): fetch against OpenRouter,
  second start uses cache; unreachable base URL falls back.
