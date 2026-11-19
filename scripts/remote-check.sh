#!/usr/bin/env bash
# The Linux half of every gate: build and test this checkout's HEAD on another
# machine through Herdr, and keep what it built so the next run is incremental.
#
# usage: scripts/remote-check.sh [--linux-script <path>] <machine> [workdir-name] [-- <extra nextest args>]
#        scripts/remote-check.sh prune <machine> [--keep N] [--job NAME] [--force]
#
# The gate: a git bundle of this checkout is copied over and fetched into a
# persistent clone on the far side, which is then forced to the bundled commit
# (fetch + `checkout -f --detach`, no merge, nothing left over in src/ or
# tests/). There `cargo fmt --check`, `cargo clippy --all-targets -- -D
# warnings` and the full nextest suite run. One line per stage; exit 1 on any
# failure.
#
# The work dir is *reused*: `~/kelir-verify-<job>` holds the clone and its
# `target/`, so the first run pays the cold build and every later run pays the
# incremental one (BUILD-INFRA rule 3; the old script recloned and rebuilt from
# cold every time, 62 s build + 29 s clippy per run). Do not delete it per run.
#
# `prune` retires work dirs (the Linux box had 22 of them, 57 GB). It keeps the
# newest `--keep` (default 3) and reports the rest; `--job NAME` narrows it to
# that job's dirs (`kelir-verify-NAME`, `kelir-verify-NAME-*`). Nothing is
# deleted without `--force`, a dir written to in the last hour is skipped, and
# so is one whose `lock` file names a live pid: a gate that deletes a
# colleague's target dir costs them a cold build.
set -uo pipefail

usage() { sed -n '2,26p' "$0"; exit 2; }

if [ "${1:-}" = "prune" ]; then
  shift
  M="${1:-}"; shift 2>/dev/null || true
  [ -z "$M" ] && usage
  keep=3; force=0; job=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --keep) keep="${2:?--keep needs a value}"; shift 2 ;;
      --job) job="${2:?--job needs a value}"; shift 2 ;;
      --force) force=1; shift ;;
      *) echo "prune: unknown argument: $1" >&2; exit 2 ;;
    esac
  done
  # The far side lists, sizes and ages the dirs and this side only formats its
  # tab-separated lines, so both ends agree on what a work dir costs.
  far_prune='
keep=$KEEP; force=$FORCE; job=$JOB; now=$(date +%s); n=0
if [ -n "$job" ]; then
  set -- "$HOME/kelir-verify-$job" "$HOME"/kelir-verify-"$job"-*
else
  set -- "$HOME"/kelir-verify*
fi
for d in "$@"; do
  [ -d "$d" ] || continue
  printf "%s\t%s\t%s\n" "$(stat -c %Y "$d" 2>/dev/null || echo 0)" \
    "$(du -sm "$d" 2>/dev/null | cut -f1)" "$d"
done | sort -rn | {
  tab=$(printf "\t")
  while IFS="$tab" read -r mtime size d; do
    size=${size:-0}; n=$((n+1))
    busy=""
    if [ -f "$d/lock" ] && pid=$(sed -n "s/^pid //p" "$d/lock" 2>/dev/null) && kill -0 "$pid" 2>/dev/null; then
      busy="in use by pid $pid"
    elif [ -n "$mtime" ] && [ $(( now - mtime )) -lt 3600 ]; then
      busy="written to $(( now - mtime ))s ago"
    fi
    if [ $n -le $keep ]; then
      printf "keep    %6s MB  %s\n" "$size" "$d"
    elif [ -n "$busy" ]; then
      printf "skip    %6s MB  %s  (%s)\n" "$size" "$d" "$busy"
    elif [ "$force" = 1 ]; then
      printf "remove  %6s MB  %s\n" "$size" "$d"
      rm -rf "$d"
    else
      printf "stale   %6s MB  %s  (--force removes it)\n" "$size" "$d"
    fi
  done
  printf "prune: %s work dirs%s; kept the newest %s\n" "$n" "${job:+ for $job}" "$keep"
}'
  hc exec --on "$M" --timeout 300 \
    --env "KEEP=$keep" --env "FORCE=$force" --env "JOB=$job" \
    -- bash -lc "$far_prune" 2>&1 | tr -d '\r'
  exit "${PIPESTATUS[0]}"
fi

M="${1:-}"; W="${2:-kelir-verify}"
[ -z "$M" ] && usage
shift 2 2>/dev/null || true
[ "${1:-}" = "--" ] && shift
extra=("$@")

root="$(git rev-parse --show-toplevel)" || exit 2
cd "$root" || exit 2
HEAD_SHA="$(git rev-parse HEAD)"

B="$(mktemp -u)".bundle
far="$(mktemp -u)".sh
trap 'rm -f "$B" "$far"' EXIT INT TERM
git bundle create "$B" --all >/dev/null 2>&1 || { echo "remote-check: git bundle failed" >&2; exit 2; }

# The far side runs a file, not an inline string, so quoting cannot bite.
cat >"$far" <<'EOS'
set -uo pipefail
# A non-login shell may have no cargo on PATH; the profile puts it in ~/.cargo.
case ":$PATH:" in
  *":$HOME/.cargo/bin:"*) ;;
  *) PATH="$HOME/.cargo/bin:$PATH"; export PATH ;;
esac
export CARGO_TERM_COLOR=never CARGO_TERM_PROGRESS_WHEN=never
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
wd="$HOME/$W"
mkdir -p "$wd" && cd "$wd" || exit 1
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
if cargo fmt --check > /tmp/fmt.log 2>&1; then
  echo FMT-OK
else
  echo FMT-FAIL; head -20 /tmp/fmt.log; exit 1
fi
if cargo clippy --all-targets -- -D warnings > /tmp/clippy.log 2>&1; then
  echo CLIPPY-OK
else
  echo CLIPPY-FAIL; tail -20 /tmp/clippy.log; exit 1
fi
if command -v cargo-nextest >/dev/null 2>&1; then
  cargo nextest run --no-fail-fast --retries 2 -j 4 $EXTRA > /tmp/nx.log 2>&1
  rc=$?
  grep -E '^[[:space:]]*(Summary|TRY [2-9]|FLAKY)' /tmp/nx.log | sed 's/^[[:space:]]*//' | tail -30
  if [ $rc != 0 ]; then
    grep -E 'FAIL|panicked' /tmp/nx.log | head -20
    echo NEXTEST-FAIL; exit $rc
  fi
  echo NEXTEST-OK
else
  cargo test --no-fail-fast > /tmp/ct.log 2>&1
  rc=$?
  grep -E '^test result|FAILED|panicked' /tmp/ct.log | head -40
  if [ $rc != 0 ]; then echo TEST-FAIL; exit $rc; fi
  echo TEST-OK
fi
if cargo test --doc > /tmp/doc.log 2>&1; then
  echo DOCTEST-OK
else
  echo DOCTEST-FAIL; tail -10 /tmp/doc.log; exit 1
fi
EOS

base64 -w0 "$B" | hc file write --on "$M" "$W/k.b64" >/dev/null \
  || { echo "remote-check: cannot write the bundle to $M:~/$W" >&2; exit 2; }
hc file write --on "$M" "$W/run.sh" <"$far" >/dev/null \
  || { echo "remote-check: cannot write the runner to $M:~/$W" >&2; exit 2; }

hc exec --on "$M" --timeout 600 -- \
  env W="$W" HEAD_SHA="$HEAD_SHA" EXTRA="${extra[*]:-}" bash "$W/run.sh"
rc=$?
echo "remote-check: $M rc=$rc (HEAD ${HEAD_SHA:0:7}, work dir ~/$W/src-tree kept)"
exit $rc
