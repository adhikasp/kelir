# M8 fix round: implementation notes

Branch `job/m8`, fixing `docs/reviews/m8-review.md` (7 should-fix, 5 nits). `main` had moved only by a queue
note, so the merge was trivial. M7 is still on `job/m7`, not merged.

## Finding, fix, proving test

| finding | fix | test that proves it |
|---|---|---|
| F1 `.cmd` gate under Git Bash | `gates::adapt_for_shell`: `cmd /C x` becomes `cmd //C x` under a POSIX shell on Windows (MSYS rewrote `/C`) | `review_m8::a_cmd_gate_runs_from_a_console_less_parent` (un-ignored); unit `a_cmd_line_is_adapted_to_a_posix_shell_on_windows_only` |
| F2 `setsid` daemon outlives the run | every gate gets `KELIR_GATE_RUN=<token>`; after the run `reap::kill_run` kills the group, the descendants and every process carrying the token (`/proc`) | `review_m8::a_gate_that_daemonizes_does_not_outlive_the_run` (un-ignored, Linux); unit `the_sweep_kills_a_setsid_process_that_carries_the_token` |
| F3 killed session orphans the tree | hidden `kelir gate-guard` beside each Unix gate polls the session pid and runs the same sweep when it is gone | `review_m8::killing_the_session_mid_gate_kills_the_gate_tree` (un-ignored on Unix) |
| F4 `git_sync.py` pushes secrets | `git add -u` by default, `GIT_SYNC_ADD_NEW`, secret-name refusal, `GIT_SYNC_DRY_RUN` | `e2e_m8::git_sync_by_default_commits_tracked_changes_only`, `..._refuses_secret_like_names_even_when_adding_new_files`, `..._dry_run_changes_nothing` |
| F5 builtin skills unreachable | the prompt lists each builtin with its description and `kelir docs --skill <name>`; new `--skill` flag | `m8fix::the_system_prompt_sent_to_the_model_advertises_the_builtin_skills`, `docs_skill_prints_each_builtin_skill_in_full`, `docs_skill_with_an_unknown_name_lists_the_builtin_ones`; unit `the_rendered_prompt_names_every_builtin_skill_and_how_to_read_it` |
| F6 project gates without trust | project `[gates]` and project-layer skill gates need `trusted_projects` (machine config) or `KELIR_TRUST_PROJECT`; skipped with an `error` event otherwise; `observe` marks them | `m8fix::a_project_config_gate_does_not_run_in_an_untrusted_project`, `kelir_trust_project_lets_the_project_gate_run`, `trusted_projects_in_the_machine_config_...`, `trusted_projects_names_a_directory_not_a_prefix_of_a_name`, `a_project_config_cannot_trust_itself`, `a_machine_config_gate_runs_in_an_untrusted_project`, `a_project_skill_gate_does_not_run_in_an_untrusted_project` |
| F7 `delegation` names missing commands | skill rewritten for `kelir run` / `ask` / `bg run` (what `main` has); M7 spellings dropped until M7 lands | `docs::tests::builtin_skills_name_only_subcommands_and_flags_kelir_help_lists` (every `kelir <sub>` and `--flag` in a builtin skill must exist in clap's visible commands), `the_command_scanner_catches_a_command_that_does_not_exist` |
| N1 `hc wait --children` | `--children all` with its meaning | `the_delegation_and_herdr_skills_are_valid_...` (herdr text) |
| N2 env-only nesting guard | documented in `gates.md` (`KELIR_GATE_RUN`, limits); not closed | n/a |
| N3 `ctl say` to own session | one paragraph in `gates.md` | n/a (review test already covers the bound) |
| N4 no `tool` field | still declared in DEVIATIONS | n/a |
| N5 ~70 ms per gate | unchanged; the guard adds one tiny spawn per Unix gate | `a_thousand_gated_tool_calls_in_one_stream` still passes |

The review's tests were not weakened. Two of them put a skill under the project's `.kelir/skills` and now set
`KELIR_TRUST_PROJECT=1`, because that is exactly what F6 changes; their assertions are the same.

## Decisions worth a second look

1. **Trust is declared, not prompted.** kelir has no prompt, and `src/trust.rs` is the socket boundary, not a
   project store. `trusted_projects` is machine-layer only (a project config naming itself is ignored with a
   warning). Side effect: machine and project `[gates]` are now both kept instead of merged per key.
2. **The token sweep, not `PR_SET_PDEATHSIG`.** PDEATHSIG reaches only the shell, not its children, and is
   cleared by fork. The environment token survives `setsid` and double forks; a gate that clears its own env
   escapes it, which `gates.md` states.
3. **Guard process per Unix gate.** The simple alternative (a thread) dies with the session, which is the case
   to cover. It is killed on the normal path and gives up after the gate timeout plus 15 s.
4. **`delegation` is truthful about `main`.** Whoever merges M7 must update `skill-delegation.md` in the same
   commit; the new test then guards the real spellings (see DEVIATIONS).

## Process hygiene

All suites ran through a count watchdog (`sh.exe` +20 or `bash.exe` +80 over the baseline aborts the run and
kills it by PID). It never fired in the final runs. The first run did fire, on `bash.exe`, with a threshold of
+20 that was too tight for a box with other panes; `sh.exe` stayed flat. Nothing was killed by image name.
The Unix half (F2, F3, the guard) cannot compile on this machine: it is verified only by the Linux gate.

Gate result: Windows + Linux PASS at f68d6de (1247 passed, 3 skipped). The first Linux run caught an `Ordering` import that was Windows-only and a prompt format clash with the Herdr-line tests; both fixed, and `e2e_steer11` now asserts "description listed, body pull" (DEVIATIONS).
