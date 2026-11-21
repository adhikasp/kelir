#!/usr/bin/env bash
# The Linux half of every gate: build and test this checkout's HEAD on another
# machine through Herdr, and keep what it built so the next run is incremental.
#
# usage: scripts/remote-check.sh [--linux-script <path>] <machine> [workdir-name] [-- <extra nextest args>]
#        scripts/remote-check.sh prune <machine> [--keep N] [--job NAME] [--yes]
#        scripts/remote-check.sh far-env | far-runner | far-argv [W [SHA [EXTRA]]]
#                                         (the far side of the gate, printed)
#
# The gate: a git bundle of this checkout is copied over and fetched into a
# persistent clone on the far side, which is then forced to the bundled commit
# (fetch + `checkout -f --detach`, no merge, nothing left over in src/ or
# tests/). There `cargo fmt --check`, `cargo clippy --all-targets -- -D
# warnings` and the full nextest suite run. One line per stage; exit 1 on any
# failure. Only committed state travels: the bundle is HEAD, so commit first
# (a dirty tree is reported, not silently shipped).
#
# The work dir is *reused*: `~/kelir-verify-<job>` holds the clone and its
# `target/`, so the first run pays the cold build and every later run pays the
# incremental one (BUILD-INFRA rule 3; the old script recloned and rebuilt from
# cold every time, 62 s build + 29 s clippy per run). Name it after the job
# (`kelir-verify-<job>`, which is what `gate.sh --linux` passes); one job's dir
# is never another job's. Do not delete a work dir to make a gate pass.
#
# `prune` retires work dirs, and is deliberately conservative: it deletes only
# directories *this script created*, proven by the marker file the script writes
# when it creates a dir (`.kelir-workdir`: a random token, the time and the
# work-dir name). On top of that a candidate must
#   * have a name that matches `kelir-verify-<plain job name>` exactly,
#   * be a real directory, not a symlink, whose physical path is directly under
#     $HOME,
#   * contain only what a work dir contains (the marker, `src-tree`, `k.b64`,
#     `run.sh`, `lock`, `logs`),
#   * not be a live run (`lock` names a running pid) and not have been written
#     to in the last hour.
# Anything else is reported as `skip` with the reason and is never touched: a
# directory named like a work dir that this script did not create (a user's own
# project, a decoy, a bare `mkdir`) has no marker and survives. Keeping is per
# job: the newest `--keep` (default 3) of each job's dirs, so an idle job keeps
# its warm `target/` no matter how many other jobs ran after it. Nothing is
# removed without `--yes`; the default is a dry run.
#
# No core dumps (BUILD-INFRA rule 6, docs/BUILD.md). The crash-injection suites
# (M3 and its reviews) abort a `kelir` on purpose, and on a stock Linux box every
# abort left a ~290 KB core in the system store: 2,660 files / 904 MB on
# 2026-11-20, all `.../target/debug/kelir` SIGABRT. `kernel.core_pattern` and the
# dump settings decide that, not the repo, so the far-side runner sets
# `ulimit -c 0` before the first test binary starts and every child inherits it.
# In the script rather than on the host on purpose: no sudo, no system config
# touched, and the same file governs every box. The runner prints the live
# `ulimit -c`, so a gate log says in its own words that the limit is really 0.
# `far-env` prints that prologue, which is how a test executes it for real.
set -uo pipefail

# The usage text is the file's own header: every line after the shebang, up to
# the first line of code, so it cannot drift out of date.
usage() { awk 'NR > 1 && /^set -uo pipefail$/ { exit } NR > 1' "$0"; exit 2; }

sh_quote() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }

# The far side's environment, and its one rule that is not about cargo: no core
# dumps. Printed on its own (`far-env`) so a test can execute the very text the
# gate ships, and pasted verbatim at the top of the runner, so there is one copy.
far_env() {
  cat <<'EOS'
set -uo pipefail
# A non-login shell may have no cargo on PATH; the profile puts it in ~/.cargo.
case ":$PATH:" in
  *":$HOME/.cargo/bin:"*) ;;
  *) PATH="$HOME/.cargo/bin:$PATH"; export PATH ;;
esac
export CARGO_TERM_COLOR=never CARGO_TERM_PROGRESS_WHEN=never
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
# No core dumps (BUILD-INFRA rule 6): the crash-injection tests abort on
# purpose, and an unchecked box wrote 2,660 cores / 904 MB for them. Set before
# any test binary starts, in the shell that starts them, so every child inherits
# it; no -S/-H, so this lowers the soft *and* the hard limit and nothing the
# gate runs can raise it back.
ulimit -c 0 2>/dev/null || echo "CORE-LIMIT: cannot set ulimit -c 0" >&2
echo "CORE-LIMIT: ulimit -c = $(ulimit -c 2>/dev/null)"
EOS
}

# The far-side runner: that environment, then the work dir and the stages. It is
# written to a file and run from there, so quoting cannot bite.
far_runner() {
  far_env
  cat <<'EOS'
wd="$HOME/$W"
created=0
[ -d "$wd" ] || created=1
mkdir -p "$wd" && cd "$wd" || exit 1
# The marker is the proof that prune may ever remove this dir: it is written
# only when this script creates the dir, so a directory somebody else made (or
# one that merely looks like a work dir) is never a prune candidate.
if [ "$created" = 1 ] && [ ! -e .kelir-workdir ]; then
  printf 'token %s\ncreated %s\njob %s\ncreator remote-check.sh\n' \
    "$(date -u +%Y%m%dT%H%M%SZ).$$.$RANDOM$RANDOM" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$W" \
    >.kelir-workdir
fi
# Logs go in this work dir, in a directory of their own per run: two gates for
# different jobs on the same box must not read each other's results (S6).
logdir="$wd/logs/$(date -u +%Y%m%dT%H%M%SZ).$$"
mkdir -p "$logdir" || exit 1
echo "logdir: $logdir"
base64 -d k.b64 > k.bundle 2>/dev/null || { echo "MACHINE-GATE: cannot decode k.b64"; exit 1; }
tree=src-tree
if [ ! -d "$tree/.git" ]; then
  rm -rf "$tree"
  git clone -q k.bundle "$tree" || { echo "MACHINE-GATE: clone failed"; exit 1; }
fi
cd "$tree" || exit 1
# The lock file only means "a run is in here": prune skips it.
printf 'pid %s\n' "$$" > ../lock
trap 'rm -f ../lock' EXIT INT TERM
git fetch -q --force ../k.bundle "+refs/heads/*:refs/remotes/verify/*" >/dev/null 2>&1
git checkout -q -f --detach "$HEAD_SHA" 2>/dev/null || {
  echo "MACHINE-GATE: the bundle does not contain $HEAD_SHA"; exit 1; }
git clean -qfdx -e target >/dev/null 2>&1
echo "HEAD: $(git log --oneline -1)"
echo "MACHINE-GATE: $(uname -s) $(nproc) cores, $(date -u +%FT%TZ)"
if cargo fmt --check > "$logdir/fmt.log" 2>&1; then
  echo FMT-OK
else
  echo FMT-FAIL; head -20 "$logdir/fmt.log"; exit 1
fi
if cargo clippy --all-targets -- -D warnings > "$logdir/clippy.log" 2>&1; then
  echo CLIPPY-OK
else
  echo CLIPPY-FAIL; tail -20 "$logdir/clippy.log"; exit 1
fi
if command -v cargo-nextest >/dev/null 2>&1; then
  cargo nextest run --no-fail-fast --retries 2 -j 4 $EXTRA > "$logdir/nextest.log" 2>&1
  rc=$?
  grep -E '^[[:space:]]*(Summary|TRY [2-9]|FLAKY|LEAK)' "$logdir/nextest.log" | sed 's/^[[:space:]]*//' | tail -30
  if [ $rc != 0 ]; then
    grep -E 'FAIL|LEAK|panicked' "$logdir/nextest.log" | head -20
    echo "NEXTEST-FAIL (log: $logdir/nextest.log)"
    exit $rc
  fi
  echo NEXTEST-OK
else
  cargo test --no-fail-fast > "$logdir/test.log" 2>&1
  rc=$?
  grep -E '^test result|FAILED|panicked' "$logdir/test.log" | head -40
  if [ $rc != 0 ]; then echo "TEST-FAIL (log: $logdir/test.log)"; exit $rc; fi
  echo TEST-OK
fi
if cargo test --doc > "$logdir/doc.log" 2>&1; then
  echo DOCTEST-OK
else
  echo DOCTEST-FAIL; tail -10 "$logdir/doc.log"; exit 1
fi
EOS
}

# What the far side is given, one `NAME=value` per line: the work dir, the
# commit the bundle must contain, and the extra nextest arguments. Printed so a
# test reads the same values this side sends.
far_fields() { # <workdir> <head-sha> <extra>
  printf '%s\n' "W=$1" "HEAD_SHA=$2" "EXTRA=$3"
}

# The far-side argv, one argument per line: the caller reads it back into an
# array, so no word splitting or quoting on this side can change it (an `EXTRA`
# with a space in it arrives as one argument).
far_argv() { # <workdir> <head-sha> <extra>
  printf '%s\n' env
  far_fields "$@"
  printf '%s\n' bash "$1/run.sh"
}

case "${1:-}" in
  far-env) far_env; exit 0 ;;
  far-runner) far_runner; exit 0 ;;
  far-argv) shift; far_argv "${1:-kelir-verify-job}" "${2:-0ff1ce}" "${3:-}"; exit 0 ;;
esac

if [ "${1:-}" = "prune" ]; then
  shift
  M="${1:-}"; shift 2>/dev/null || true
  [ -z "$M" ] && usage
  keep=3; yes=0; job=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --keep) keep="${2:?--keep needs a value}"; shift 2 ;;
      --job) job="${2:?--job needs a value}"; shift 2 ;;
      --yes | --force) yes=1; shift ;;
      --dry-run) yes=0; shift ;;
      *) echo "prune: unknown argument: $1" >&2; exit 2 ;;
    esac
  done
  case "$keep" in
    '' | *[!0-9]*) echo "prune: --keep must be a whole number, not '$keep'" >&2; exit 2 ;;
  esac
  if [ -n "$job" ]; then
    case "$job" in
      *[!A-Za-z0-9._-]* | .* | *..*)
        echo "prune: --job must be a plain job name (letters, digits, . _ -), not '$job'" >&2
        exit 2
        ;;
    esac
  fi
  # The far side does the deciding: it is where the directories and their
  # markers are, and it must refuse an unsafe candidate by itself, not because
  # this side formatted it carefully. This side only passes the knobs and
  # prints what came back.
  far_prune='
keep=$KEEP; yes=$YES; job=$JOB
die() { echo "prune: $*" >&2; exit 2; }
case "$keep" in
  "" | *[!0-9]*) die "--keep must be a whole number, not $keep" ;;
esac
if [ -n "$job" ]; then
  case "$job" in
    *[!A-Za-z0-9._-]* | .* | *..*) die "--job must be a plain job name, not $job" ;;
  esac
fi
home=$(cd "$HOME" 2>/dev/null && pwd -P) || die "cannot resolve HOME"
now=$(date +%s)
tab=$(printf "\t")

# What a work dir this script created contains, and nothing else.
allowed() {
  case "$1" in
    .kelir-workdir | src-tree | k.b64 | run.sh | lock | logs) return 0 ;;
  esac
  return 1
}

# refuse_reason <dir>: print why a dir may not be touched, or nothing.
refuse_reason() {
  local d=$1 base real entry name marker
  base=${d##*/}
  case "$base" in
    kelir-verify-?*) ;;
    *) echo "not a kelir-verify work dir name"; return ;;
  esac
  name=${base#kelir-verify-}
  case "$name" in
    *[!A-Za-z0-9._-]* | .* | *..*) echo "name is not a plain job name"; return ;;
  esac
  if [ -L "$d" ]; then echo "symlink"; return; fi
  if [ ! -d "$d" ]; then echo "not a directory"; return; fi
  real=$(cd "$d" 2>/dev/null && pwd -P) || { echo "cannot resolve"; return; }
  case "$real" in
    "$home"/*) ;;
    *) echo "resolves outside HOME ($real)"; return ;;
  esac
  case "$real" in
    "$home"/*/*) echo "not directly under HOME ($real)"; return ;;
  esac
  marker="$d/.kelir-workdir"
  if [ ! -f "$marker" ] || [ -L "$marker" ]; then
    echo "no marker from this script: not created by it"
    return
  fi
  if ! grep -q "^token [A-Za-z0-9._-]" "$marker" 2>/dev/null; then
    echo "marker carries no token"
    return
  fi
  while IFS= read -r entry; do
    name=${entry##*/}
    if ! allowed "$name"; then
      echo "contains something a work dir does not ($name)"
      return
    fi
  done <<EOF2
$(find "$d" -mindepth 1 -maxdepth 1 2>/dev/null)
EOF2
}

say_skip() { printf "skip    %6s MB  %s  (%s)\n" "$2" "$1" "$3"; }

n=0; kept=0; skipped=0; stale=0; removed=0
# Group by job: the work-dir name after `kelir-verify-`, so keeping is per job
# and an idle job never loses its warm target to a busier one (S8).
# Records go into an array, not a newline-joined string: this body lives inside
# a single-quoted assignment, so an ANSI-C quoted newline here would close that
# quote early and silently turn the separator into a literal n (one merged line
# for every dir, and every dir then read as "not a directory").
recs=()
for d in "$HOME"/kelir-verify*; do
  [ -e "$d" ] || [ -L "$d" ] || continue
  base=${d##*/}
  if [ -n "$job" ]; then
    case "$base" in
      "kelir-verify-$job" | "kelir-verify-$job"-*) ;;
      *) continue ;;
    esac
  fi
  n=$((n + 1))
  # GNU stat uses -c %Y; BSD/macOS uses -f %m. Without this fallback every
  # dir reads mtime 0 on macOS and the prune treats fresh work dirs as stale.
  mtime=$(stat -c %Y "$d" 2>/dev/null || stat -f %m "$d" 2>/dev/null || echo 0)
  size=$(du -sm "$d" 2>/dev/null | cut -f1)
  recs+=("$mtime$tab${size:-0}$tab$d")
done

# Newest first, then keep the newest `keep` of each job.
printf "%s\n" ${recs[@]+"${recs[@]}"} | sort -rn | {
  while IFS="$tab" read -r mtime size d; do
    [ -n "$d" ] || continue
    group=${d##*/}
    group=${group#kelir-verify-}
    # A job owns `kelir-verify-<job>` and `kelir-verify-<job>-<n>` (a second
    # work dir for the same job): those are one family, so keeping is per job.
    case "$group" in
      *-[0-9] | *-[0-9][0-9] | *-[0-9][0-9][0-9]) group=${group%-*} ;;
    esac
    reason=$(refuse_reason "$d")
    if [ -n "$reason" ]; then
      say_skip "$d" "$size" "$reason"
      skipped=$((skipped + 1))
      continue
    fi
    # per-group counter: keeping is per job (S8)
    key=$(printf "%s" "$group" | tr -c "A-Za-z0-9._-" "_")
    eval "cnt=\${cnt_$key:-0}"
    cnt=$((cnt + 1))
    eval "cnt_$key=$cnt"

    pid=""
    if [ -f "$d/lock" ] && pid=$(sed -n "s/^pid //p" "$d/lock" 2>/dev/null) && kill -0 "$pid" 2>/dev/null; then
      say_skip "$d" "$size" "in use by pid $pid"
      skipped=$((skipped + 1))
    elif [ "$cnt" -le "$keep" ] && [ -n "$mtime" ] && [ "$mtime" -gt 0 ]; then
      printf "keep    %6s MB  %s  (job %s, #%s)\n" "$size" "$d" "$group" "$cnt"
      kept=$((kept + 1))
    elif [ -n "$mtime" ] && [ $((now - mtime)) -lt 3600 ]; then
      say_skip "$d" "$size" "written to $((now - mtime))s ago"
      skipped=$((skipped + 1))
    elif [ "$yes" = 1 ]; then
      printf "remove  %6s MB  %s  (job %s, #%s)\n" "$size" "$d" "$group" "$cnt"
      rm -rf "$d"
      removed=$((removed + 1))
    else
      printf "stale   %6s MB  %s  (job %s, #%s; --yes removes it)\n" "$size" "$d" "$group" "$cnt"
      stale=$((stale + 1))
    fi
  done
  printf "prune: %s work dirs%s: %s kept, %s skipped, %s stale, %s removed (newest %s per job, %s)\n" \
    "$n" "${job:+ for job $job}" "$kept" "$skipped" "$stale" "$removed" "$keep" \
    "$([ "$yes" = 1 ] && echo "--yes" || echo "dry run")"
}'
  hc exec --on "$M" --timeout 300 \
    --env "KEEP=$keep" --env "YES=$yes" --env "JOB=$job" \
    -- bash -lc "$far_prune" 2>&1 | tr -d '\r'
  exit "${PIPESTATUS[0]}"
fi

M="${1:-}"; W="${2:-kelir-verify}"
[ -z "$M" ] && usage
shift 2 2>/dev/null || true
[ "${1:-}" = "--" ] && shift
extra=("$@")

case "$W" in
  kelir-verify-?*) ;;
  kelir-verify) echo "remote-check: name the work dir after the job: kelir-verify-<job>" >&2; exit 2 ;;
  *) echo "remote-check: the work dir must be kelir-verify or kelir-verify-<job>, not $W" >&2; exit 2 ;;
esac

root="$(git rev-parse --show-toplevel)" || exit 2
cd "$root" || exit 2
HEAD_SHA="$(git rev-parse HEAD)"
if [ -n "$(git status --porcelain 2>/dev/null)" ]; then
  echo "remote-check: this checkout has uncommitted changes; the bundle carries HEAD ($(git rev-parse --short HEAD)) only" >&2
fi

B="$(mktemp -u)".bundle
far="$(mktemp -u)".sh
trap 'rm -f "$B" "$far"' EXIT INT TERM
git bundle create "$B" --all >/dev/null 2>&1 || { echo "remote-check: git bundle failed" >&2; exit 2; }

# The far side runs a file, not an inline string, so quoting cannot bite.
far_runner >"$far" || { echo "remote-check: cannot write the runner" >&2; exit 2; }

base64 -w0 "$B" | hc file write --on "$M" "$W/k.b64" >/dev/null \
  || { echo "remote-check: cannot write the bundle to $M:~/$W" >&2; exit 2; }
hc file write --on "$M" "$W/run.sh" <"$far" >/dev/null \
  || { echo "remote-check: cannot write the runner to $M:~/$W" >&2; exit 2; }

# The command travels as argv, read back into an array here: an `EXTRA` with
# spaces in it then reaches the far side as one argument, which the old
# `env ... EXTRA="${extra[*]}"` string form did not guarantee.
mapfile -t far_cmd < <(far_argv "$W" "$HEAD_SHA" "${extra[*]:-}")
hc exec --on "$M" --timeout 600 -- "${far_cmd[@]}"
rc=$?
echo "remote-check: $M rc=$rc (HEAD ${HEAD_SHA:0:7}, work dir ~/$W/src-tree kept)"
exit $rc
