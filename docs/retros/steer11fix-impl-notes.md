# steer11fix implementation notes

Merged main (tui3 landed; `tui.md` already carried the banner/folds/outcome lines, I added the
slash aliases). Fixed the review's findings:

- S1 prompt precedence: config `prompt` replaces `<config>/prompt.md` (read only when unset); only
  `<cwd>/.kelir/prompt.md` beats it. Fixed in the config row (tables.rs), config.md, sources-and-skills.md.
- S2 `kelir ls` removed from files.md. S3 `KELIR_HOME` documented as `$HOME`/`USERPROFILE` only
  (envs.rs row, env.md, files.md). S4 `bg ls` does not list a job with no status file.
- S5 env guard now rejects `env::var`, `var_os`, `vars`, `vars_os`, aliased imports and `env::{`.
- S6 recorded in DEVIATIONS.md (function pointers, not lines).
- Drift tests added: defaults compared with the running code (`the_documented_defaults_...`),
  event field names both ways, no serde rename/alias on config fields. Hidden clap flags are
  skipped in cli.md like hidden subcommands.
- Nits: N1 shell default, N3 aliases, N4 skill body. Added "what will the model see" and
  credential rotation to troubleshooting.md. N2 (`ctl --help` verb list) left: pre-existing text.
- Mutations checked: `var_os` in util.rs fails `envs`; changing `spill_limit` default fails `docs`.
