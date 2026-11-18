# M0 fixes (from docs/reviews/M0-review.md)

Fix F1-F6 and N1, N3, N4, N5, N6 (N2: do it in M1 if cheap, otherwise leave a TODO in DEVIATIONS.md). Keep
`tests/review_m0.rs` and make all of it green; do not weaken those tests.

- **F1** `bash` must return when the shell process exits, not at pipe EOF. After the shell exits,
  drain what is already buffered for a short grace period (e.g. 200 ms), then return; a leftover
  background child keeps running detached from the call (it must not be killed, it is not tracked yet).
- **F2** stream the output to `commands/<id>/out`; keep in memory only a head (first 1 KB), a tail
  ring (last 1 KB), byte and line counts, and the first `spill_limit` bytes if you need them for the
  inline result. No `Vec` of the whole output. Add a test with ~50 MB of output that asserts the
  process RSS stays small, or at minimum that memory is bounded by construction.
- **F3/F4** the output directory name must not come from the provider id: use
  `commands/<seq>-<step>-<n>` (monotonic, filesystem-safe); keep the provider call id only as a field
  in the events. `tool_settled.out_path` stays accurate for every call.
- **F5** edit: try the literal `old` first; only fall back to line-ending conversion if the literal
  does not match; keep per-region line endings when inserting.
- **F6** payload rule: any single string field over 10 KB in an event (`assistant.text`,
  `reasoning`, `tool_calls[].args`, `tool_started.input`, ...) is written to
  `blobs/<seq>-<field>` in the session dir and the event holds
  `{"spilled": true, "path": "...", "bytes": N, "preview": "<first 1 KB>"}` in its place.
  **`fold` must read the blob back so the request bytes are identical to the unspilled ones**: the
  cache prefix must not change. Add a unit test that folds a log with a spilled arg and asserts the
  request is byte-identical to one built from the inline version, and an e2e with a 50 KB `edit`.
- **N1** a panicked/cancelled tool task settles as `error`, never drops a call.
- **N3** normalise separators of `out_path` (forward slashes everywhere is fine).
- **N4** the spill hint names `sed -n` first, `rg` second.
- **N5** usage errors in `--json` mode still print a `result` event (outcome `error`, exit code 2).
- **N6** add `docs/DEPS.md`: each dependency outside the spec's list, one line why.

Gates: fmt, clippy -D warnings, full `cargo test` green (including `review_m0`). Commit
`M0: fixes`, update the `docs/PROGRESS.md` row, then
`hc send t1x "M0 fixes ready: <commit> <counts>"`.
