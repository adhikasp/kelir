# m6fix2 implementer notes

- F1 (stderr echo of env): `redact` in `src/mcp/rpc.rs` applied in `hint_from`; proof
  `review_m6b::a_server_that_echoes_its_env_on_death_is_redacted` (un-ignored).
- F2 (Linux orphan): `UNIX_GUARD` watcher in the server's process group; proof
  `review_m6b::a_killed_session_leaves_no_server_and_no_grandchild` (un-ignored on Linux).
- N1 accepted (DEVIATIONS), N2 documented in `docs/reference/mcp.md`, N3 stays open (STEER-7).
