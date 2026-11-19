#!/usr/bin/env bash
# The milestone gate on this machine: fmt, clippy, the whole test suite, and
# (with --linux) the same commands on the Linux box. One summary line per
# stage, full output in logs, exit 1 if anything failed.
#
# usage: scripts/gate.sh [options] [-- <extra cargo args>]
#   --linux[=MACHINE]   also run scripts/remote-check.sh there (default: $KELIR_LINUX, else the maintainer)
#   --no-lock           do not take the machine-wide gate lock (you are already inside it)
#   --jobs N            CARGO_BUILD_JOBS and nextest test threads (default 4; BUILD-INFRA rule 1)
#   --out DIR           where the logs go (default: <scratch>/gate-<job>-<stamp>)
#   --quick             fmt + clippy only (the loop you run before every commit)
#   --load              also re-run the suite under artificial CPU load, 3x (STEER-9 stress mode)
#
# Everything a gate needs is here so nothing has to remember the flags:
#   * exactly one gate at a time across all worktrees (scripts/gate-lock.sh);
#   * 4 build jobs and 4 nextest threads: three agents share a 12-core desktop
#     and the suites are green serially but flake when oversubscribed
#     (BUILD-INFRA rule 1; docs/BUILD.md has the numbers);
#   * `cargo nextest run --no-fail-fast --retries 2`: one process per test, and
#     every test that needed a retry is named in the summary. A retry is a flake
#     to fix (STEER-9), never a clean pass;
#   * `cargo test --doc` still runs directly, because nextest does not run
#     doctests (the crate has none today; this keeps it that way).
set -uo pipefail

orig_args=("$@")
linux=""
use_lock=1
jobs="${KELIR_GATE_JOBS:-4}"
out=""
quick=0
load=0
extra=()
while [ $# -gt 0 ]; do
  case "$1" in
    --linux) linux="${KELIR_LINUX:-the maintainer}"; shift ;;
    --linux=*) linux="${1#--linux=}"; shift ;;
    --no-lock) use_lock=0; shift ;;
    --jobs) jobs="${2:?--jobs needs a value}"; shift 2 ;;
    --out) out="${2:?--out needs a value}"; shift 2 ;;
    --quick) quick=1; shift ;;
    --load) load=1; shift ;;
    --) shift; extra+=("$@"); break ;;
    -h | --help) sed -n '2,31p' "$0"; exit 0 ;;
    *) echo "gate: unknown argument: $1" >&2; exit 2 ;;
  esac
done

root="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)" || exit 2
cd "$root" || exit 2

if [ -z "$out" ]; then
  # The re-exec into the lock must reuse the outer run's directory, or one gate
  # leaves two: go through --out and KELIR_GATE_OUT for that.
  scratch="${KELIR_SCRATCH:-$HOME/kelir-scratch}"
  job="$(git rev-parse --abbrev-ref HEAD | tr '/' '-')"
  out="${KELIR_GATE_OUT:-$scratch/gate-$job-$(date -u +%Y%m%dT%H%M%SZ)}"
fi
mkdir -p "$out"
log="$out/gate.log"
: >"$log"

export CARGO_BUILD_JOBS="$jobs"
export CARGO_TERM_PROGRESS_WHEN="${CARGO_TERM_PROGRESS_WHEN:-never}"
export NEXTEST_TEST_THREADS="$jobs"

say() { echo "$*" | tee -a "$log"; }
failed=0
stage_ok() { say "  $1: ok"; }
stage_fail() { say "  $1: FAILED ($2)"; failed=1; }

run_stage() { # run_stage <name> <logfile> <command...>
  local name="$1" file="$2"
  shift 2
  local start
  start=$(date +%s%N)
  if "$@" >"$file" 2>&1; then
    say "  $name: ok ($(( ($(date +%s%N) - start) / 1000000 )) ms)"
    return 0
  fi
  say "  $name: FAILED ($(( ($(date +%s%N) - start) / 1000000 )) ms) — $file"
  failed=1
  return 1
}

gate_body() {
  say "gate: $(git rev-parse --short HEAD) $(git log -1 --format=%s)"
  say "gate: $root  jobs=$jobs  log=$log"

  run_stage fmt "$out/fmt.log" cargo fmt --check
  run_stage clippy "$out/clippy.log" cargo clippy --all-targets -- -D warnings
  if [ "$quick" = 1 ]; then
    [ "$failed" = 0 ] && say "gate: quick ok" || say "gate: quick FAILED"
    return
  fi

  if ! command -v cargo-nextest >/dev/null 2>&1; then
    say "  nextest: MISSING — install it (docs/BUILD.md)"
    failed=1
  else
    if run_stage nextest "$out/nextest.log" \
      cargo nextest run --no-fail-fast --retries 2 -j "$jobs" "${extra[@]+"${extra[@]}"}"; then
      # A retried test is a flake: name it even when the run is green.
      local retried
      retried="$(grep -E '^[[:space:]]+(TRY [2-9]|FLAKY)' "$out/nextest.log" | sed -E 's/^[[:space:]]+//' | sort -u)"
      local summary
      summary="$(grep -E '^[[:space:]]+Summary ' "$out/nextest.log" | tail -1 | sed -E 's/^[[:space:]]+//')"
      say "  result: ${summary:-no summary line}"
      if [ -n "$retried" ]; then
        say "  RETRIED (a flake to fix, STEER-9):"
        echo "$retried" | sed 's/^/    /' | tee -a "$log"
        if [ "${KELIR_GATE_ALLOW_RETRIES:-0}" != 1 ]; then
          # Green only because of a retry is not green: report the gate as a
          # failure so the flake gets fixed instead of ignored.
          failed=1
          say "  nextest: FAILED (a test needed a retry; set KELIR_GATE_ALLOW_RETRIES=1 to accept)"
        fi
      fi
    fi
    run_stage doctest "$out/doctest.log" cargo test --doc
  fi

  if [ "$load" = 1 ]; then
    say "  load: 3x under artificial load"
    local n=0
    for round in 1 2 3; do
      if run_stage "load-$round" "$out/load-$round.log" bash "$root/scripts/gate-load.sh" "$jobs"; then n=$((n + 1)); fi
    done
    [ "$n" = 3 ] || { failed=1; say "  load: FAILED ($n/3 green)"; }
  fi

  if [ -n "$linux" ]; then
    local machine="$linux"
    run_stage "linux ($machine)" "$out/linux.log" \
      bash "$root/scripts/remote-check.sh" "$machine" "kelir-verify-$(basename "$root")"
  fi

  if [ "$failed" = 0 ]; then
    say "gate: PASS"
  else
    say "gate: FAIL"
  fi
}
if [ "$use_lock" = 1 ] && [ -z "${KELIR_GATE_INNER:-}" ]; then
  # Re-run the same command line inside the lock (with --no-lock so it does not
  # try to take the lock twice).
  KELIR_GATE_INNER=1 KELIR_GATE_OUT="$out"     exec bash "$root/scripts/gate-lock.sh" -- bash "$0" --no-lock "${orig_args[@]}"
fi

gate_body
exit "$failed"
