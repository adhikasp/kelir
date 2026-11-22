# Dependencies

The spec names `tokio`, `serde`, one HTTP client (`reqwest` + rustls), `clap`, `ratatui`/`crossterm`,
`rmcp` and `interprocess` (`rmcp` is **not** used: M6 hand-rolls the JSON-RPC subset, see below). Everything else, one line each explaining why it is here.

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
- **`ratatui`** (with its `crossterm` backend) — the spec names `ratatui`/`crossterm`. M2 uses ratatui
  0.30's inline viewport and `insert_before`, and takes `crossterm` through ratatui's re-export so the
  two can never disagree about the version. Default features are trimmed to the crossterm backend,
  the layout cache and 256-colour support; the widget calendar, serde and macros are off.
- **`unicode-width`** — `wrap` needs a column count, and CJK/emoji are two columns wide. ratatui
  already depends on it (0.2), so this adds no build.

**MSRV:** `rust-version` is now 1.88, because ratatui 0.30 requires it (and the let-chains clippy
suggests for edition 2024 are stable from 1.88). Both build hosts run 1.99/1.100.

## MCP (M6)

- **No new crate.** The spec names `rmcp`, but the client side of `kelir mcp` needs six things:
  `initialize`, `tools/list`, `tools/call`, `notifications/cancelled`, a `ping` answer and inbound
  notifications, over a child's stdin/stdout as one JSON object per line. That is about 400 lines on
  `tokio` + `serde_json`, both already linked. `rmcp` is a full SDK (server side, macros, schema
  generation, transports we do not use) with its own dependency tree, which the 20 MB / 30 MB
  footprint rules and the build-time budget in `docs/BUILD.md` do not leave room for. The trade-off
  is recorded in `docs/DEVIATIONS.md` (M6); HTTP/SSE transport, the optional stretch, is not built.
  The base64 decoder for image and blob content is ~25 lines in `src/mcp/render.rs` rather than a
  crate.
- `toml`, `serde_json`, `ulid`, `tokio` (process, io-util, sync), `libc`/`windows-sys` job objects
  are the existing ones; `config.toml` reuses `toml`.

## Dev only (not linked into the binary)

- **Python 3** (a test tool, not a crate) — `tests/support/mcp_server.py` is the stdio MCP server the M6
  e2e tests drive (`python` on Windows, `python3` elsewhere; standard library only).
- **`tempfile`** — isolated state/session and working dirs for the unit and e2e tests.
- **`portable-pty`** — the headless PTY test that drives the real binary's TUI (type a message, read
  the answer off the pty, `/quit`). It is a test dependency only; nothing in the binary uses it.
