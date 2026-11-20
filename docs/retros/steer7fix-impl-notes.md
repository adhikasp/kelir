# STEER-7 fixes — implementation notes

Brief: `docs/tasks/STEER-7-fixes.md`. Findings: `docs/reviews/steer7-review.md` (FAIL, 1 blocker B1,
S1, S2, three nits). Both red tests in `tests/review_steer7.rs` pass unweakened (the file's only edit
is rustfmt's reflow, no assertion touched).

## 0. Merged `main` first

`git merge main` into `job/steer7` before anything else, as the brief says; no conflicts.

## 1. B1 (blocker) — a tool child could impersonate the bg shim

The old ownership test was "a `commands/<id>/` directory or a `bg/<id>.status` file exists". Every
bash child holds `KELIR_TOKEN` by design, so the token proves nothing about a report, and the child
can create either file. Two things close it, and they are complementary:

- **The session is the only issuer of job ids.** `Job {capability, call_id}` and a `HashMap` in
  `trust::Trust` are the whole of "is it mine". Ids are `bg_<32 hex>` / `call_<32 hex>` (128 random
  bits, STEER-7 S2: no deterministic names two sessions share). A bash call's own id is
  `<seq>-<step>-<n>` (kelir names the directory) and is *registered* with an **empty** capability,
  so it can be cancelled but no `bg` report is ever accepted for it. `kelir bg run` gets its id and
  capability from the new internal `bg-claim` verb (one round trip, starts nothing — `bg-spawn`
  would leave a second shim behind); the number of claims per session is bounded (`CLAIM_BUDGET`
  = 256) so an authenticated child cannot make the session mint for ever.
- **A per-job capability.** 32 random bytes, handed to that job's shim as `KELIR_JOB_CAP` (its
  environment, never an argv, never a file), required on the *request* (`Request.capability`, not
  in the status-shaped `args`, which a child can write). `Trust::proves` compares it in constant
  time and needs the id in the registry too.
- **Files are never evidence.** `note_bg` records only ids it `owns`; a status file nobody minted
  is left alone (still listed by `kelir bg ls`, which reads files, which is fine — it is a human's
  view, not an event). On resume the registry is rebuilt **from the log only**: `bg_started` for
  jobs, and a new optional `tool_started.dir` for calls. A resumed job has no capability, so it
  answers `cancel` but never a `bg` report — the secret died with the process that minted it, and a
  shim that outlives its session settles through its status file and the watcher.
- **Refusals are rate-limited.** `REFUSAL_BUDGET` = 32 `verb` events per session; past it a refusal
  is still answered and not logged, so a child naming foreign ids one by one cannot grow the log.
- **Paths are resolved, and the id is checked first.** `bg` checks ownership, then `out`/`cmd_path`
  (canonicalised inside `bg/` or `commands/`; `..`, a symlink/junction out, a UNC path or another
  drive is refused), then kind, then the capability. Path resolution never runs on the say-so of a
  stranger's report.

## 2. S1 — the same-user limit, documented, and the token file dropped on exit

SPEC (*Who may drive a session*) and DEVIATIONS now say precisely what the boundary is: it protects
against other users, a stray process, a script that found the pipe by name and the model's own tools
impersonating a shim — **not** against same-user code that can read `<session dir>/token`. Where it
is cheap it is tightened: `Trust::retire()` removes the token file when the session ends (a token
nobody serves is only something to find), and the resumed process mints a new one. The brief's test
line that a child "can report itself" with the token alone was corrected: the token is what a child
has, the capability is what a shim has.

## 3. Nits

- **N1** — `valid_job_id` refuses Windows device names (`CON`, `PRN`, `AUX`, `NUL`, `COM0..9`,
  `LPT1..9`, their `name.ext` and superscript-digit spellings, case-insensitively), a trailing `.`
  or space, and now leading/trailing whitespace at all (it used to be trimmed away, which silently
  turned `bg_esc ` into an id the session would then own). `com10` and `console` stay valid.
- **N2** — the session's `bg/` is created at start with `0700` on Unix (`restrict_dir_to_owner`;
  explicitly, not left to the umask, which under `umask 000` gives `777`). On Windows the DACL is
  inherited from the already-private state dir; there is no portable 0700 to set, and the code says
  so rather than pretending.
- **N3** — the listener reads at most `MAX_LINE` (64 KB) per request line, waits at most
  `IDLE_TIMEOUT` (10 s) for it, and bounds its own answer write by `IO_TIMEOUT`, so a peer that
  connects and says nothing, sends a megabyte with no newline, or never reads its answer cannot hold
  a connection task for as long as it likes. All three are unit-tested through `read_bounded` over a
  duplex stream.

## 4. Live attack drive (no key needed)

The reviewer's `scripts/live/steer7_setup.sh` needs `OPENROUTER_API_KEY`, which this worktree does
not have. Equivalent coverage without a key: `scripts/live/canned_server.py` as the provider, the
real debug binary, two real sessions (A attacking, B victim), and the reviewer's own
`scripts/live/steer7_attack.py` run in its `child`, `stranger` and `foreign` modes against A and in
`child` against A while naming B's job. Results:

- `child`: `SUMMARY exploits=[]`. Every `bg` report a child sends is now refused — `a1` (an existing
  `commands/<id>/`) for the missing capability, `a2`/`a4` (a `mkdir`ed directory, a dropped status
  file) for the id not being the session's, `a3` (control) likewise. Every `bg-spawn` escape (`..`,
  the session dir, a junction, temp, drive root, UNC, `D:\`, `BG`) is refused; no marker file was
  written; `nul`, `con`, `bg_esc.`, 300-char and traversal ids are refused.
- `stranger`: `SUMMARY exploits=[]`; the 1 MB line is refused at the cap, the session still answers
  an ordinary request immediately, and the junk left the log unchanged.
- `foreign` (authenticated to A, naming B's real job): `SUMMARY exploits=[]`.
- One expected finding remains and is the documented same-user boundary: `reader` (same user, reads
  B's token file) can drive B. That is what S1 says it can, and it cannot read a *capability*, so it
  still cannot impersonate a job's shim.

Two fixes to the attack script itself were needed to drive it: a refused `verb` event is not an
"exploit" (the design logs authenticated attempts, so `new_events` now labels them `(rejected)` and
only an accepted verb or a job event counts), and the 1 MB probe no longer blocks its own process on
a write the listener has stopped reading.

## 5. Tests

- New: `a_report_with_the_token_but_no_capability_is_refused` (end-to-end: a real `bg run` job, then
  the token alone, a wrong capability and a 64-zero capability are all refused, nothing reaches the
  log) and `a_request_line_is_bounded_in_length_and_in_time` (unit); `valid_job_id` gained the N1
  cases; `a_tool_child_gets_the_token_and_the_file_does_not_outlive_the_session` replaces the old
  `..._and_can_report` (which asserted the very thing B1 says must not be true).
- Still green after the change: `review_steer7`, `e2e_steer7` (11), `e2e_m4` (10),
  `review_m4` (19), `e2e_m3`, `e2e_verbs`, `review_m1`, `e2e`, the whole `--lib` suite (414).

## 6. Known gaps

- A `tool_started` written by an older kelir has no `dir`, so a resume does not adopt that call's id;
  a `cancel` for it is refused after the resume (safe side, and the directory is still on disk).
- The capability is not stored, by design; after a resume only the file-based settlement path remains
  for a job that outlived its session.
- A same-user process that can read `/proc/<pid>/environ` (Linux) is inside the boundary, as S1
  documents.
