#!/usr/bin/env bash
# The milestone gate on this machine: fmt, clippy, the whole test suite, and
# (with --linux) the same commands on the Linux box. One summary line per
# stage, full output in logs, exit 1 if anything failed.
#
# usage: scripts/gate.sh [options] [-- <extra cargo args>]
#   --linux[=MACHINE]   also run scripts/remote-check.sh there; MACHINE comes from the flag or
#                       $KELIR_LINUX and has no default (a host name is not repo content)
#   --no-lock           do not take the machine-wide gate lock (you are already inside it)
#   --jobs N            CARGO_BUILD_JOBS and nextest test threads (default 4; BUILD-INFRA rule 1)
#   --out DIR           where the logs go (default: <scratch>/gate-<job>-<stamp>)
#   --quick             fmt + shape + clippy only (the loop you run before every commit)
#   --shape-only        run just the shape stage (what CI calls)
#   --load              also run the suite 3x in a row under artificial CPU load, no retries
#                       (STEER-9 stress mode; KELIR_GATE_LOAD sets the busy loops, default 4)
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
# No core dumps (BUILD-INFRA rule 6, docs/BUILD.md). The suite includes tests
# that abort a `kelir` on purpose (the `KELIR_TEST_CRASH_AT` crash injection), and on
# a box whose core pattern writes to a store each abort left a ~290 KB core:
# 2,660 files / 904 MB on 2026-11-20. The limit is set in the shell that starts
# the test binaries — before fmt, so nothing this gate spawns can dump — and is
# inherited by every child, including scripts/gate-load.sh's. Deliberately in
# the script, not on the host: no sudo, no system config, and the same file
# governs every box. `remote-check.sh` sets it again on the far side, because
# the far side is a different shell.
case "$(uname -s 2>/dev/null)" in
  Linux | Darwin)
    ulimit -c 0 2>/dev/null || echo "gate: cannot set ulimit -c 0 (core dumps possible)" >&2
    ;;
esac

orig_args=("$@")
linux=""
use_lock=1
jobs="${KELIR_GATE_JOBS:-4}"
out=""
quick=0
shape_only=0
load=0
extra=()
while [ $# -gt 0 ]; do
  case "$1" in
    --linux)
      # `--linux` alone reads $KELIR_LINUX, and there is no default: which
      # machine that is, is a property of the box, not of a public repo.
      linux="${KELIR_LINUX:-}"
      if [ -z "$linux" ]; then
        echo "gate: --linux needs a machine: use --linux=MACHINE or set KELIR_LINUX" >&2
        echo "gate:   (omit --linux to run the Windows half only)" >&2
        exit 2
      fi
      shift ;;
    --linux=*)
      linux="${1#--linux=}"
      if [ -z "$linux" ]; then
        echo "gate: --linux= needs a machine name: --linux=MACHINE (or set KELIR_LINUX)" >&2
        exit 2
      fi
      shift ;;
    --no-lock) use_lock=0; shift ;;
    --jobs) jobs="${2:?--jobs needs a value}"; shift 2 ;;
    --out) out="${2:?--out needs a value}"; shift 2 ;;
    --quick) quick=1; shift ;;
    --shape-only) shape_only=1; shift ;;
    --load) load=1; shift ;;
    --) shift; extra+=("$@"); break ;;
    -h | --help) sed -n '2,33p' "$0"; exit 0 ;;
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

# The tests behind nextest's status lines. nextest prints one line per attempt,
# `TRY 2 PASS [  1.2s] (3/4) kelir::suite test_name` (no `TRY` on the first
# attempt, `LKFAIL` when the leak check fails the test), and only the last line
# per test says how it ended. So the names are computed once, by final status:
#   nextest_names <log> failed   -> the tests that did not pass
#   nextest_names <log> retried  -> the tests that passed only on a retry (the
#                                   flakes; a test that failed every attempt is
#                                   failed, not a flake)
#   nextest_names <log> leaked   -> the tests whose leak check failed
# A test's name is the last two fields of its line, which holds for both the
# `kelir::suite test_name` and the `kelir::suite::test_name` spelling.
nextest_names() { # <logfile> failed|retried
  awk -v mode="$2" '
    {
      s = $0
      gsub(/^[ \t]+/, "", s); gsub(/[ \t\r]+$/, "", s)
      if (index(s, "[") == 0) next
      n = split(s, F, " ")
      if (n < 5) next
      i = 1
      if (F[1] == "TRY") { try = F[2] + 0; i = 3 } else try = 0
      st = F[i]
      if (st !~ /^(PASS|FAIL|FLAKY|SLOW|LEAK|LKFAIL|LEAK-FAIL|TIMEOUT|ABORT|SIGSEGV|SIGABRT|SKIP)$/) next
      name = F[n-1] " " F[n]
      last[name] = st
      if (try >= 2) attempts[name] = try
    }
    END {
      for (k in last) {
        bad = (last[k] ~ /^(FAIL|LEAK|LKFAIL|LEAK-FAIL|TIMEOUT|ABORT|SIGSEGV|SIGABRT)$/)
        if (mode == "failed" && bad) print k
        if (mode == "retried" && !bad && (k in attempts)) print k
        if (mode == "leaked" && last[k] ~ /^(LEAK|LKFAIL|LEAK-FAIL)$/) print k
      }
    }' "$1" | sort
}

# What a nextest log means for the gate. Prints the summary line, names every
# flake (a test that only passed on a retry: STEER-9's to fix) and every leaked
# process, and returns 1 when the gate must fail. A retry fails it only under
# KELIR_GATE_STRICT_RETRIES=1; a leak always does, because "no leaked processes"
# is a hard rule (AGENTS.md), not a style nit (S5).
nextest_report() { # <logfile>
  local file="$1" summary retried leaked n rc=0
  summary="$(grep -E '^[[:space:]]+Summary ' "$file" | tail -1 | sed -E 's/^[[:space:]]+//')"
  say "  result: ${summary:-no summary line}"
  retried="$(nextest_names "$file" retried)"
  if [ -n "$retried" ]; then
    n="$(echo "$retried" | grep -c .)"
    # Always named, never silent: the whole point of --retries is this line.
    say "  RETRIED (a flake to fix, STEER-9): $n test(s)"
    echo "$retried" | sed 's/^/    /' | tee -a "$log"
    if [ "${KELIR_GATE_STRICT_RETRIES:-0}" = 1 ]; then
      # Opt-in strictness for a job that has fixed its flakes: then a retry is
      # a regression, not a known cost.
      rc=1
      say "  nextest: FAILED (KELIR_GATE_STRICT_RETRIES=1 and a test needed a retry)"
    else
      say "  nextest: ok, but the $n test(s) above are STEER-9's to fix"
    fi
  fi
  leaked="$(nextest_names "$file" leaked)"
  if [ -n "$leaked" ]; then
    rc=1
    say "  LEAKED PROCESSES (hard rule: no leaked processes; AGENTS.md):"
    echo "$leaked" | sed 's/^/    /' | tee -a "$log"
  fi
  return "$rc"
}

# A failing run must name what failed, not only point at a log: the log is for
# the fix, the summary is for whoever reads the gate (S11).
nextest_names_report() { # <logfile>
  local names
  names="$(nextest_names "$1" failed)"
  if [ -n "$names" ]; then
    say "  FAILED TESTS:"
    echo "$names" | sed 's/^/    /' | tee -a "$log"
  else
    say "  FAILED TESTS: none named; the run itself errored ($1)"
  fi
}

gate_body() {
  if [ "$shape_only" = 1 ]; then
    run_stage shape "$out/shape.log" bash "$root/scripts/shape.sh"
    [ "$failed" = 0 ] && say "gate: shape ok" || say "gate: shape FAILED"
    return
  fi
  say "gate: $(git rev-parse --short HEAD) $(git log -1 --format=%s)"
  say "gate: $root  jobs=$jobs  log=$log"

  run_stage fmt "$out/fmt.log" cargo fmt --check
  run_stage shape "$out/shape.log" bash "$root/scripts/shape.sh"
  # Severity comes from `Cargo.toml [lints]`; passing `-- -D warnings` here
  # would be applied after the table and undo its allow-list (#23).
  # The workspace lint table is authoritative; do not re-enable the pedantic
  # allow-list with a blanket `-D warnings` (lint-groups-priority).
  run_stage clippy "$out/clippy.log" cargo clippy --all-targets -- -D warnings
  if [ "$quick" = 1 ]; then
    [ "$failed" = 0 ] && say "gate: quick ok" || say "gate: quick FAILED"
    return
  fi

  if ! command -v cargo-nextest >/dev/null 2>&1; then
    say "  nextest: MISSING — cargo nextest is a gate dependency; install it:"
    say "    cargo install cargo-nextest --locked   (both boxes; see docs/BUILD.md)"
    failed=1
  else
    if run_stage nextest "$out/nextest.log" \
      cargo nextest run --no-fail-fast --retries 2 -j "$jobs" "${extra[@]+"${extra[@]}"}"; then
      nextest_report "$out/nextest.log" || failed=1
    else
      # A failing run must name what failed, not only point at a log: the log is
      # for the fix, the summary is for whoever reads the gate (S11).
      nextest_names_report "$out/nextest.log"
      nextest_report "$out/nextest.log" || true
      failed=1
    fi
    run_stage doctest "$out/doctest.log" cargo test --doc
  fi

  if [ "$load" = 1 ]; then
    # STEER-9: the whole suite, three times in a row, with ${KELIR_GATE_LOAD:-8}
    # busy loops beside it and no retries. A test that fails here is a flake (or
    # a bug) and is named, per round, so the fix has a name to start from.
    local load_threads="${KELIR_GATE_LOAD:-4}"
    say "  load: 3 rounds under $load_threads busy loops, no retries"
    local n=0
    for round in 1 2 3; do
      if run_stage "load-$round" "$out/load-$round.log" bash "$root/scripts/gate-load.sh" "$load_threads"; then
        n=$((n + 1))
      else
        local names
        names="$(nextest_names "$out/load-$round.log" failed)"
        if [ -n "$names" ]; then
          say "    load-$round FAILED TESTS:"
          echo "$names" | sed 's/^/      /' | tee -a "$log"
        fi
      fi
    done
    [ "$n" = 3 ] || { failed=1; say "  load: FAILED ($n/3 green)"; }
    [ "$n" = 3 ] && say "  load: ok (3/3 green)"
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
  KELIR_GATE_INNER=1 KELIR_GATE_OUT="$out" \
    exec bash "$root/scripts/gate-lock.sh" -- bash "$0" --no-lock "${orig_args[@]}"
fi

gate_body
exit "$failed"
