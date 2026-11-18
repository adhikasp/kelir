# M1 fixes (from docs/reviews/M1-review.md)

Read the review first. Make `tests/review_m1.rs` fully green without weakening it, and keep every
other suite green.

- **F1** `stop --call <id>` stops exactly that call. It must not arm a deadline over the batch or
  abort siblings; the batch ends when every call settles. (A bounded wait for the *stopped* call's
  own process tree to die is fine.)
- **F2** `live_calls` tracks settled calls: `stop --call` on an unknown or already-settled call is
  refused with a clear error and changes nothing.
- **N1** fsync the `verb` event before sending the ack.
- **N2** a `stopped` result tells the model about partial output: byte/line counts and the
  `out_path` (and `tool_settled.out_path` is filled in), plus a tail of up to 1 KB.
- **N3** `ctl latest` targets the newest session only (error if it is dead); document in README/usage.
- **N4** Unix: create the socket and state/session dirs with mode 0700/0600 (`cfg(unix)`).
- N5/N6 (other nits in the review): fix if cheap, otherwise one line each in docs/DEVIATIONS.md.

Gates: fmt, clippy -D warnings, full `cargo test`. Update the `docs/PROGRESS.md` M1 row (counts),
commit `M1: fixes` (stage explicit paths), then `hc send t1x "M1 fixes ready: <commit> <counts>"`.
