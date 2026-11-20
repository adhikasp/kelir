# steer8fix implementation notes

| Finding | Fix | Test |
|---|---|---|
| F1 wrong price for the default model | row `-1017` removed (time-of-day priced) | `spend::tests::time_priced_default_model_has_no_table_row` |
| F2 window 128000 before first request | `context.window` null + `window_source` while source is `default` | `spend::tests::status_window_is_null_until_known` |
| F3 TUI status not shown | DEVIATIONS entry | - |
| F4 warning wording/digits | "cost" (+ "partly estimated"), up to 6 digits | `spend::tests::small_cost_warning_shows_distinct_figures` |
| F5, F6, F7 | documented in DEVIATIONS and the status verb row | drift test |

`lookup_is_exact_and_the_config_wins` used the removed row for its table assertions; it now uses
`deepseek-v4-flash` (same intent, config override part unchanged).
