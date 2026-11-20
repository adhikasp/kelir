# m6fix implementer notes

Job `m6fix`: the findings of `docs/reviews/m6-review.md`. Branch `job/m6` (merged with main first).
All 18 tests of `tests/review_m6.rs` pass with their `ignore` attributes removed.

| finding | fix | proving test |
|---|---|---|
| F1 blocker: TOML error echoed the `env` line | `safe_error` in `src/mcp/config.rs`: file, line:column, parser message; serde's quoted values are cut out | `review_m6::a_bad_mcp_toml_does_not_print_the_env_block`; unit `a_syntax_error_never_echoes_the_source` (10 shapes); `e2e_m6::a_canary_in_env_reaches_no_output_channel` (stdout, stderr, events, every file of the session dir; broken / wrong-typed / dying / healthy config; session and one-shot) |
| F2 non-UTF-8 line killed the connection | `read_loop` reads bytes (`read_until`) and decodes lossily | `review_m6::a_non_utf8_line_does_not_kill_the_connection` |
| F3 server stderr dropped on death | EOF error and handshake errors carry `(server stderr: …)`; the reader waits up to 500 ms for stderr to drain | `e2e_m6::a_server_that_dies_at_start_says_why` |
| F4 `anyOf`/`type: [..]` strings | `declared_types` collects `type`, lists and `anyOf`/`oneOf`; `anyOf`/`oneOf` are validated | `review_m6::a_nullable_string_keeps_its_text`; unit `nullable_and_union_strings_keep_their_text`, `any_of_is_validated` |
| F5 retry after the request was sent | `ipc::WithinError` (`NotConnected` / `AfterConnect`); a call falls back only on a failed connect; `DROPPED_REPLY` refusal is exit 4 too | `e2e_m6::a_lost_session_after_the_send_is_exit_4_and_not_run_again` (fake session answers `tools`, drops `call`; the server log stays empty) |
| F6a inherited environment | allow-list `INHERITED` in `rpc.rs` + the server's `env` | `e2e_m6::a_server_does_not_inherit_the_sessions_environment` (uses a new `envnames` tool of the test server) |
| F6b Linux orphan after `kill -9` | unchanged and still unverified with a real node server; kill-on-close covers Windows, a well-behaved server exits on stdin EOF. No `PDEATHSIG` (wrong contract on a tokio worker). Open. | existing `a_server_does_not_outlive_a_session_that_is_killed` |
| F7 huge integer rounded | `type_value` keeps an integer beyond 64 bits as text | `review_m6::a_huge_integer_is_not_rounded`; unit `a_huge_integer_stays_text` |
| F8 orphaned `mcp_started` after resume | `reopen` writes `mcp_settled outcome=unknown exit_code=4` for every unsettled `mcp_started` | `e2e_m6::resume_settles_an_orphaned_mcp_call_as_unknown` (real kill of a session mid-`slow`, real resume) |
| N1 config read once | documented (`docs/reference/mcp.md`, DEVIATIONS) | – |
| N2 `list_changed` ignored | reader sets a flag, `Broker::ensure` re-lists | `e2e_m6::list_changed_refreshes_the_tools` (test server adds `late`) |
| N3 tool named `ls` | `kelir mcp <server> call <tool>` | `e2e_m6::call_reaches_a_tool_named_like_a_keyword`; unit `call_reaches_a_tool_named_ls` |
| N4 lost continuation spaces | collapsed in `tables.rs` and `verbs.rs`; `kelir docs --generate` | drift test |
| N5 death race | `request` re-checks `alive` after registering the waiter | covered by the reader/`request` ordering comment; the dead-server tests (`a_crashed_server_is_exit_3…`) |
| S1 project servers run unprompted | documented, listed in DEVIATIONS as open; the trust gate comes with STEER-7 | – |

## Side effects

- `tests/support/mcp_server.py` gained `envnames`, `MCP_TEST_DIE`, `MCP_TEST_LISTCHANGED`; `mcp ls` now
  counts 9 tools. Two older tests passed `MCP_TEST_LOG`/`MCP_TEST_PIDFILE` through the CLI's
  environment, which only worked because servers inherited everything; they now use the server's `env`.

## STEER-7 merge notes

Everything the CLI sends is one verb, `mcp` (`op` = `servers` | `tools` | `call`), built in
`Backend::ask` (`src/mcp/cli.rs`) and sent by `ipc::request_value_within`; served by
`Session::mcp_verb`. All three ops must carry the session token (`call` runs a tool; `servers` and
`tools` start servers). `mcp-settle` / `mcp-notify` are in-process and keep their random key. Details
in `docs/reference/mcp.md` ("Verb authentication"). `request_value_within` now returns
`WithinError`; steer7's `request_value` token injection must be applied there too.
