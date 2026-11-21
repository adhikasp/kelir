# Publishing kelir — preparation and hand-off

**Status: PREPARED, NOTHING PUSHED OR PUBLISHED.** This document records the
M12 publish-preparation work and the exact commands for the maintainer to run
*after* giving the explicit go-ahead. The push, repository creation, crates.io
publish and license choice are open decisions (see the end).

All work was done in the `job/m12prep` worktree on top of `main` `058d3ca`.
No remote was added, no push, publish or `gh repo create` was performed.

## 1. History scrub survey

### What was scanned

Every commit reachable from `--all` (571 objects in the survey: 569 on the
`main` lineage plus two on other branches), every commit message, every
author/committer identity field, and every unique blob (1,873 blobs under the
survey's size cap) — not just HEAD. The scanner reports only pattern *indices*
and counts, never matched text, so no secret or personal value was copied into
a file or into tool output during the survey. It reads the pattern file from
outside the repository: `C:/Users/<user>/kelir-scratch/scrub-patterns.txt`
(9 literal, case-insensitive patterns: personal names, machine names, a
Windows host name, a personal host name and a numeric address).

### Findings (categories only, no values)

- Every one of the 571 identities (author and committer) matched a personal
  name pattern; three different identity spellings were in use. **All authors
  and committers must be rewritten.**
- Commit messages: a handful of commits (single digits per pattern) carried a
  personal name in the message text (e.g. attribution notes).
- Tracked blobs across history: the name patterns appeared broadly in docs
  (`AGENTS.md`, `Cargo.toml`, `docs/PROGRESS.md`, `docs/QUEUE.md`, retros,
  reviews, tasks) and in test/source files, including the embedded banned-name
  list in `src/docs/tests.rs` and a Windows-looking path literal in
  `tests/review_steer6b.rs`. Machine/host/address patterns appeared in review
  and task docs and in one review fixture line.
- **HEAD itself was not publishable**: `scripts/scrub-check.sh` reported 7
  pattern matches in the current tree before this job's doc changes.

### Pattern-driven scrub plan

1. Keep the pattern file outside the repository (never commit it — a committed
   list of what to avoid would defeat the check).
2. Rewrite all identity fields to one neutral public identity.
3. Replace every pattern occurrence, case-insensitively, in commit messages
   and blob contents with neutral wording from a second outside-repo map:
   `C:/Users/<user>/kelir-scratch/m12-replacements.txt`. The mapping is one
   neutral phrase per pattern in the same order; values are never printed.
4. Create the result as a *new* repository in a new directory; never rewrite
   this worktree's history.
5. Verify with an all-history scanner over the export (identities, messages,
   *and* every blob of every commit), then with `scripts/scrub-check.sh` over
   the exported HEAD tree.

A future `.mailmap` is **not** sufficient on its own: it only changes how
names are displayed, not what is stored in the objects. The export rewrites
the objects themselves.

### Concrete command sequence (equivalent to `git filter-repo`)

`git filter-repo` is not installed on this machine and no network install was
performed. The equivalent used here is `git fast-export` → Python stream
filter → `git fast-import`, implemented in
`C:/Users/<user>/kelir-scratch/m12-export.py` (outside the repo). The filter
rewrites `author`/`committer` lines, scrubs every `data` block (blob contents
and commit messages) with the pattern/replacement files, drops fast-export's
blank separator lines (fast-import rejects them as empty commands), and adds
the terminating `done`.

```sh
PAT=/c/Users/<user>/kelir-scratch/scrub-patterns.txt
REP=/c/Users/<user>/kelir-scratch/m12-replacements.txt
SRC=C:/Users/<user>/kelir-wt/m12prep
OUT=/c/Users/<user>/kelir-scratch/publish/kelir

rm -rf "$OUT"
git init -q -b main "$OUT"
cd "$OUT"
git config user.name "kelir contributors"
git config user.email "noreply@users.noreply.github.com"
git config commit.gpgsign false

python C:/Users/<user>/kelir-scratch/m12-export.py "$SRC" "$PAT" "$REP" "$OUT"
git reset --hard main          # fast-import leaves the index/worktree empty
# fast-export emits refs in order; make sure main points at the final commit:
git checkout -B main job/m12prep   # or the final publication branch
git status --short             # must be empty
```

If `git filter-repo` is available instead, the equivalent one-liner shape is
(this was **not** run here — no filter-repo binary):

```sh
git filter-repo --force \
  --name-callback  'return b"kelir contributors"' \
  --email-callback 'return b"noreply@users.noreply.github.com"' \
  --commit-callback 'c.message = scrub(c.message)' \
  --blob-callback  'blob.data = scrub(blob.data)'
```

where `scrub()` applies the same outside-repo pattern/replacement map.

### The export that was produced

- Path: `C:/Users/<user>/kelir-scratch/publish/kelir` (new directory, outside
  the repo; this worktree's history was not touched).
- 572 commits, 447 tracked files; working tree clean after checking out
  the M12 commit as `main` (the export's `main` initially pointed at the
  pre-M12 merge commit; `git checkout -B main job/m12prep` fixes it).
- Identity fields: one neutral identity only.
- All-history verifier: **`RESULT=CLEAN`** (identities, messages and all
  blobs); report at
  `C:/Users/<user>/kelir-scratch/m12-export-verify.txt`.
- HEAD tree `bash scripts/scrub-check.sh "$PAT"`: **clean (440 tracked
  files)**.
- `cargo check --offline` on the export: **passes**.

Verifier command (also outside the repo):

```sh
python C:/Users/<user>/kelir-scratch/scrub-history-verify.py \
  C:/Users/<user>/kelir-scratch/publish/kelir \
  C:/Users/<user>/kelir-scratch/scrub-patterns.txt \
  C:/Users/<user>/kelir-scratch/m12-export-verify.txt
```

**Note:** the pattern/replacement/verifier/export files live in scratch. If
the export is moved to another machine, carry the pattern file and re-run the
verifier there; never copy the pattern file into the repository.

## 2. CI review (dry review only; no CI run has happened)

`.github/workflows/ci.yml` was read and parsed as YAML (no `actionlint` or
`act` binary is installed here). Findings:

- The workflow is structurally valid: one `test` job, `fail-fast: false`,
  three-OS matrix (`ubuntu-latest`, `macos-latest`, `windows-latest`),
  `actions/checkout@v4`, `dtolnay/rust-toolchain@stable` with rustfmt+clippy,
  `Swatinem/rust-cache@v2`, then fmt, clippy `-D warnings` and `cargo test`.
- **Cargo caching is present** via `Swatinem/rust-cache@v2`; no change needed.
- **Remote-machine tests are correctly gated by design**: the tests that
  mention the Linux gate call the local `far-env` / `far-runner` *print*
  subcommands of `scripts/remote-check.sh`; they never open an SSH connection
  and never read a machine from the repo. The gate itself takes the machine
  from `--linux=<machine>` or `KELIR_LINUX`, with no default. No remote env var
  is required for CI.
- **Cross-OS paths**: the only unconditional personal-looking path in tests is
  a Windows-style *string* in `tests/review_steer6b.rs` used as shell input;
  on macOS/Linux it is data passed to `bash` and does not affect the result.
  It is a scrub concern (handled by the export), not a CI failure. Other
  Windows paths in tests are behind `#[cfg(windows)]` or use `SystemRoot`.
  The `bash()` helper falls back to `bash` on PATH on non-Windows runners.
- **No CI fix was needed.** The real first run still has to happen on GitHub
  after the push; a dry review cannot prove runner availability, cache keys
  or the actual three-OS result.

Open CI item: run the workflow on GitHub after push; watch for any OS-specific
failure that cannot be reproduced locally.

## 3. Repo hygiene (added in this job)

- `README.md` — what/why, install from crates.io or source, config file
  locations and a minimal example, both providers with key sources, usage, and
  an explicit fully-autonomous safety stance (no approvals, no human in the
  loop, gates as the policy layer). No personal data.
- `SECURITY.md` — supported versions, private reporting, what to include,
  out-of-scope items, and the security model in brief.
- `CONTRIBUTING.md` — worktree/one-writer workflow, commit style, the test
  pyramid, `scripts/gate.sh` (including the Linux half), docs drift test and
  scrub check.
- `CHANGELOG.md` — release notes derived from `docs/PROGRESS.md` (M0–M11 and
  STEER-9), plus a `0.1.0 — pending release` heading.
- `LICENSE-MIT` and `LICENSE-APACHE` — both full texts prepared using the
  neutral copyright holder "kelir contributors". **No choice was made.**
- `Cargo.toml` — added neutral `authors`, `readme`, `repository`, `homepage`,
  `documentation`, `keywords` and `categories`. The repository/homepage URL
  uses a `adhikasp` placeholder deliberately, so the drafted files contain no
  personal account name; the maintainer must substitute the real public owner
  before publishing. `license = "MIT"` was already present and was **left as
  found** because choosing a license is the maintainer's decision (see open
  decisions).

No personal data appears in any drafted file (verified against the pattern
list). `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`
both pass on this branch.

## 4. Release prep

- Version: **0.1.0** (`Cargo.toml`), unchanged.
- Changelog: `CHANGELOG.md`, derived from the milestone rows in
  `docs/PROGRESS.md` (110 → ~1300+ tests across M0–M11, live-drive verdicts
  and known gaps are in that file).
- `cargo publish --dry-run`: **cannot run offline** — it tries to fetch the
  registry index and fails with `attempting to make an HTTP request, but
  --offline was specified`. No credentials were used and nothing was
  published.
- Offline substitute that *does* work:
  `cargo package --offline --allow-dirty --no-verify` → packaged 449 files,
  5.4 MiB (1.6 MiB compressed), exit 0. **From the scrubbed export** the same
  command packages 449 files with **zero pattern hits** in the `.crate`;
  `cargo check --offline` and `cargo test --offline --lib docs` (24 passed)
  also pass there.
- **Important:** packaging from this preparation worktree produces a `.crate`
  whose files still match 7 scrub patterns (the un-scrubbed historical docs
  are tracked files). Package/publish **only from the scrubbed export**, never
  from this worktree.
- crates.io name availability for `kelir` was **not** verified (no network);
  check `https://crates.io/crates/kelir` before publishing. If taken, choose
  an alternative name and update `Cargo.toml` and the README install command.

## 5. Publication checklist (run only after the maintainer says yes)

1. Get the explicit maintainer decision on **push yes/no** and on **license**.
2. Substitute the real public owner in `Cargo.toml` (`repository`, `homepage`)
   and `README.md` (clone URL); set the chosen license in `Cargo.toml`
   (`license = "MIT"`, `"Apache-2.0"`, or the dual `MIT OR Apache-2.0` if both
   files are kept) and keep only the chosen `LICENSE*` file.
3. Check the crates.io name `kelir` is free; adjust if needed.
4. Re-run the export from the final committed state (commands in §1), then
   the all-history verifier and `scripts/scrub-check.sh` on the export.
5. Run the gates on the export's equivalent commit: `fmt`, `clippy
   -D warnings`, the full suite, and `scripts/gate.sh --linux=<machine>` from
   a machine name supplied at run time.
6. Get a fresh reviewer verdict on the scrubbed export (secrets, names,
   emails, hosts, paths, tokens) and on the docs; record it in
   `docs/reviews/` and add the publication row to `docs/PROGRESS.md`.
7. Run `cargo publish --dry-run` with network access from the scrubbed export,
   then `cargo package`/`cargo publish` when ready.
8. Only then, and only with the explicit go-ahead: create the GitHub remote
   and push `main`:
   ```sh
   cd C:/Users/<user>/kelir-scratch/publish/kelir
   git remote add origin <public-repository-url>
   git push -u origin main
   ```
   Never force-push. The first CI run happens on that push.
9. Confirm the three-OS CI run is green; watch for the OS-specific issues a
   dry review cannot catch.

## Open decisions (blocking — nothing proceeds without them)

- **Push / publish: YES or NO** — the maintainer must explicitly confirm
  before the first push, repository creation, crate publish or any other
  outward-facing action. This job did none of them.
- **License: MIT vs Apache-2.0** (or dual). Both texts are prepared; the
  existing `license = "MIT"` field is a pre-existing value, not a choice made
  here. The maintainer must choose and the docs/Cargo.toml must be updated in
  the same commit.
- **Repository owner/URL** — the drafted files used an `<owner>` placeholder;
  the real public owner must be supplied.
- **crates.io name** — verify `kelir` is available (needs network).
- **Real CI run** — only GitHub can prove the matrix; do not treat this dry
  review as CI sign-off.
