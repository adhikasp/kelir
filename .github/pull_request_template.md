## What changed

<!-- One or two lines. Link the milestone/task if there is one (e.g. M7, docs/QUEUE.md item). -->

## Why

<!-- What problem does this fix, or what does it enable? -->

## Proof

**Every PR must show before vs after.** Pick the rows that apply and delete the rest.

- [ ] **Behaviour / logic change** (parser, gates, CLI, verbs, IPC, process handling):
      paste the before and after output — command run + exact stdout/stderr, event-log
      excerpts, or test names. A diff alone is not proof.
- [ ] **TUI or any user-facing change** (rows, keys, layout, prompts, status text, docs
      rendered to the user): **attach a screenshot or short recording of the real binary
      before and after**. Use `scripts/live/` or a Herdr pane; paste the images inline or
      link to them. Describe what the reviewer should look for. A test that only asserts
      on internal row state does not replace a visual.
- [ ] **Performance / footprint change** (binary size, RSS, timing): give the measured
      before and after numbers with the command and commit used (`docs/BUILD.md` has the
      measurement recipe).
- [ ] **Docs-only / config-only change** (this repo's own workflows/templates): link the
      before and after rendered state or the raw before/after values (e.g. `gh api` JSON).

Before:
<!-- output / screenshot / numbers / link -->

After:
<!-- output / screenshot / numbers / link -->

## Verification

- [ ] `bash scripts/gate.sh` (and `--linux=<machine>` where applicable) is green; result
      recorded in `docs/PROGRESS.md`.
- [ ] `kelir docs --generate` + `cargo test --lib docs` pass if a config key, CLI flag,
      env var, verb, event or TUI key changed (`CONTRIBUTING.md`).
- [ ] Live drive verdict from the reviewer recorded.

## Reviewer notes

<!-- Anything the reviewer should check by hand, known gaps, or follow-ups. -->
