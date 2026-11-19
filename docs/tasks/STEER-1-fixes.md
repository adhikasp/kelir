# STEER-1 fixes (from docs/reviews/STEER-1-review.md)

Read the review in full first. Make any red reviewer test green without weakening it; every other
suite stays green on Windows and Linux (`bash scripts/remote-check.sh <machine>`); follow the
no-leaked-processes rule.

- **S1** key the metadata cache by the endpoint (hash of the normalised base URL + provider name), not
  the provider name alone; an unreachable base URL must fall back to `default` even on a warm box
  that has another endpoint's cache. **S5/N5**: never interpolate a config string into a file name
  (use a slug/hash).
- **S2** take the metadata fetch off the critical path: start it concurrently at session start, do
  not await it before the first request unless the cache is cold *and* the model's window is needed
  for a decision (compaction check is at `before_step`, so resolve lazily there), raise the cap to a
  value that fits the real ~1 MB payload (or stream-parse / fetch only what is needed), and never
  silently land on the 128 k default: record why in the log (see N4).
- **S3** write the cache atomically (temp file in the same dir + rename; fsync not required).
- **N1** `/models` URL derived correctly when `base_url` already ends in `/chat/completions` (or any
  full path). **N2** a failure timestamp in the future must not lock out refresh (clamp to now).
  **N3** lookup fallback by last path segment must not cross vendors (or must be documented and
  tested as intentional). **N4** every fetch failure is recorded (an `error`-kind or `state` field
  with a short reason: 401, offline, malformed, timeout) and shown in `kelir observe`/TUI banner.
- Update docs/DEVIATIONS.md and the `docs/PROGRESS.md` STEER-1 row; commit `STEER-1: fixes` (explicit paths).

## Note (2026-11-20, orchestrator): main has changed a lot since this brief was written
STEER-2 (providers, credentials), STEER-5 (retry/backoff), STEER-6 (capability gates that READ the cached model list: `parallel_tool_calls`,
`reasoning_effort`, vision) and the TUI v2 work all landed or are landing. Re-read `src/models.rs` and every caller before changing it:
the cache key, the fetch timing and the atomic write now also affect those capability gates, and S2 (take the fetch off the critical path)
must not make the first request lose its capability data (use a conservative built-in allow-list until the list arrives, and refresh the
cache without waiting for a `model` verb). Use `bash scripts/gate.sh --linux=<machine from your prompt>` for gates; write the work-dir name
`kelir-verify-steer1fix` conventions as in AGENTS.md. Your review file for this brief is docs/reviews/STEER-1-review.md.
