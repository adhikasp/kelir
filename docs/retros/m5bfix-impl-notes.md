# m5bfix implementation notes

Merged `main` first (conflicts in DEVIATIONS, PROGRESS, config.rs: both sides kept). Then, per finding:

| finding | fix | proving test |
|---|---|---|
| F1 key sent to `[hint] endpoint` | key kept only when `endpoint` is unset or has the provider's scheme/host/port (`hint::same_origin`); else keyless | `review_m5b::the_session_key_is_not_sent_to_a_different_hint_endpoint` (un-ignored), `hint::tests::same_origin_compares_scheme_host_and_port` |
| F2 hint usage not recorded | `Scorer::take_usage`; `usage` event `kind: "hint"` (counts in spend, not in observe's cache ratio) | `e2e_m5b::the_scoring_calls_usage_is_recorded_as_kind_hint` |
| F3 `--attach` | not built: recorded in DEVIATIONS; `view` is the surface; PDF is a documented refusal with a better message | existing `pdf_and_odd_media_names_are_handled_not_crashed` |
| F4 `max_tokens` 120 | `SCORER_MAX_TOKENS = 600` | `hint::tests::the_scorer_cap_leaves_room_to_think` |
| N1 error vs source note | kept as `error` (a `source` event would enter the context); DEVIATIONS | - |
| N2 deleted memory note | one tail `source` message of kind `memory`, said once | `e2e_m5b::a_deleted_memory_note_is_told_to_the_model_once` |
| N3 observe uses current config | stated in the module doc and reference | - |
| N4 ctl help | `observe` added to the verb list in `cli.rs` | docs drift test |
| N5 scorer delay | one sentence in `sources-and-skills.md` | - |

Auth: `observe` is a read of `events.jsonl`, not a socket verb, so it takes no token and writes no
`verb` event; documented read-only in DEVIATIONS. No new verbs were added, so the canary-secret
test over every verb is unchanged.
