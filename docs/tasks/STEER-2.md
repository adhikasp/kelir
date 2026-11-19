# STEER-2 — two providers, OpenCode Go credential read from opencode itself

## Why
A maintainer session copied opencode's OAuth access token into `KELIR_GO_API_KEY` (env + a dotenv).
That copy goes stale (the token carries an `expires`), shadows the live store, and puts a secret
in more places. kelir should read the credential from opencode directly and the provider choice
should be unambiguous.

## Build
1. **Exactly two providers**, named `openrouter` and `opencode-go`. `provider = "..."` (config,
   `--provider`) accepts only those; anything else is an error that lists the two. No ad-hoc names
   (a maintainer config currently uses a provider called `kelir` of kind openrouter: that must now
   be `openrouter`). Optional `[providers.openrouter]` / `[providers.opencode-go]` tables only
   *override* settings of those two (`base_url`, extra headers, key source, `reasoning_field`).
2. **Key sources per provider**, first match wins, and `kelir doctor` (new, tiny) prints which
   source was used, never the secret:
   - OpenRouter: `OPENROUTER_API_KEY`, then `key_env = "NAME"`, then `key_cmd = ["cmd", ...]`
     (stdout, trimmed). A literal `key = "..."` in a config file is rejected with a warning
     recommending env/`key_cmd`.
   - OpenCode Go: **default = opencode's own credential**: run `opencode auth export opencode`
     (the supported CLI; JSON array, the entry with `integrationID == "opencode"` and
     `active`, token = `value.access`, `value.expires` in ms epoch). No SQLite and no reading
     of opencode.db directly. Then `KELIR_GO_API_KEY` as an explicit override placed *first* only
     if set; (decide order: explicit env/`key_env`/`key_cmd` override > opencode store).
     Find the `opencode` executable via PATH (on Windows also the `.cmd` shim; run it with a
     hidden console, no window, 5 s timeout, never printing its output).
3. **Freshness**: read at session start, hold in memory only (never written to disk, logs, events,
   `observe`, error text or the process command line). If `expires` is within 2 minutes or the
   provider answers 401/403, re-read once (opencode refreshes its own credential when it runs) and
   retry the step; if still expired, fail with a message naming the expiry time and the fix
   ("run any `opencode` command once so it refreshes, then retry"). Do **not** call the OAuth
   refresh endpoint yourself (rotating the refresh token would break opencode).
4. **Latency**: measure `opencode auth export` (it may talk to opencode's background service);
   do it concurrently with other startup work; document the number; `kelir ask` cold start must
   stay reasonable (record it).
5. Tests: unit (JSON shapes: missing entry, inactive, expired, malformed, extra integrations;
   key-source precedence; provider-name validation; redaction: a test that fails if any secret
   string appears in any event/log/error output), e2e with a fake `opencode` executable on PATH
   (script/binary in tests/support) returning crafted JSON incl. a refreshed second read after a
   401; Windows (.cmd shim, no console window) and Linux.
6. Docs: SPEC.md Providers section and DEVIATIONS.md; README/usage lists the two providers and
   key sources. Update `scripts/live` defaults to the new provider names.

## Out of scope
Removing the old env copy from the maintainer's machine (done by the orchestrator afterwards).

## Gates
fmt, clippy -D warnings, full `cargo test`, `bash scripts/remote-check.sh <machine>`, the
no-leaked-processes rule, then a fresh reviewer including a live drive with the real
opencode credential against the real Go endpoint (cheap model), and a check that the secret is
absent from every log/event.
