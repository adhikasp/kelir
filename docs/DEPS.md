# Dependencies

The spec names `tokio`, `serde`, one HTTP client (`reqwest` + rustls), `clap`, `ratatui`/`crossterm`,
`rmcp` and `interprocess`. Everything else, one line each explaining why it is here.

## Runtime

- **`serde_json`** — the event log, the NDJSON output and the provider body are JSON; `serde` alone
  cannot parse or build it.
- **`toml`** — `config.toml` is TOML (named by the spec); a hand-rolled parser would be worse.
- **`time`** — RFC 3339 `ts` values and the frozen date in the system prompt; the standard library
  has no calendar type.
- **`ulid`** — `ses_<ulid>` session ids, the id shape the spec fixes.
- **`windows-sys`** (Windows only) — `CreateJobObjectW`, `SetInformationJobObject` and
  `AssignProcessToJobObject` for the per-call Job Object with kill-on-close; `std` exposes only
  `creation_flags`, not job objects. M1 also uses it for `CREATE_SUSPENDED`, `OpenThread`/`ResumeThread`
  and a `Toolhelp32` thread walk (N2).
- **`libc`** (Unix only) — `killpg(2)` is the only way to signal a whole process group; `std` has no
  process-group signalling. The spec's *Shell and platforms* table names `killpg` for `stop`.

## Dev only (not linked into the binary)

- **`tempfile`** — isolated state/session and working dirs for the unit and e2e tests.
