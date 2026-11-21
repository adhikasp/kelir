#!/usr/bin/env bash
# Per-test failure rates under artificial load (STEER-9, docs/reviews/FLAKES.md).
#
# usage: scripts/flake-rate.sh [rounds] [busy-loops] [outfile]
#
# Runs the whole suite `rounds` times (default 3) with `busy-loops` CPU burners
# (default 8), each round through scripts/gate-load.sh with --retries 0 so a flake
# is a failure and not a quiet pass, and writes one line per test that ever
# failed: `<failures>/<rounds> <binary> <test>`. A process-count watchdog aborts
# the run when `sh`/`bash` processes grow by more than 20 over the start (the
# 2026-11-20 incident). Take the gate lock yourself: this script does not.
set -uo pipefail
rounds="${1:-3}"
threads="${2:-8}"
outfile="${3:-flake-rate.txt}"
root="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)" || exit 2
cd "$root" || exit 1
scratch="${KELIR_SCRATCH:-$HOME/kelir-scratch}/flake-rate-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$scratch"

# gate-load.sh watches its own tree. This outer watchdog is a second, coarse
# net for a suite that leaks shells outside that tree; it ignores sibling
# workers by only acting on a global growth of more than 120 since the start.
shcount() {
  case "$(uname -s)" in
    Linux | Darwin) pgrep -c -x -e sh,bash 2>/dev/null || echo 0 ;;
    *) tasklist 2>/dev/null | grep -ciE '^(sh|bash)\.exe' || true ;;
  esac
}
base="$(shcount)"
(
  while :; do
    sleep 10
    now="$(shcount)"
    if [ "$now" -gt $((base + 120)) ]; then
      echo "flake-rate: WATCHDOG sh/bash count $now (base $base)" >>"$scratch/watchdog.txt"
      kill -TERM "$$" 2>/dev/null
      exit
    fi
  done
) &
wd=$!
trap 'kill "$wd" 2>/dev/null' EXIT

: >"$scratch/all-failures.txt"
for r in $(seq 1 "$rounds"); do
  bash scripts/gate-load.sh "$threads" >"$scratch/round-$r.log" 2>&1
  rc=$?
  tr -d '\r' <"$scratch/round-$r.log" \
    | awk '{ s=$0; gsub(/^[ \t]+/,"",s); n=split(s,F," "); if (n>=5 && F[1] ~ /^(FAIL|TIMEOUT|SIGABRT|SIGSEGV|LEAK|LKFAIL)$/ && index(s,"[")>0) print F[n-1]" "F[n] }' \
    | sort -u | sed "s/^/round$r /" >>"$scratch/all-failures.txt"
  echo "flake-rate: round $r rc=$rc ($(grep -c "^round$r " "$scratch/all-failures.txt") failing tests)" >&2
done
awk -v R="$rounds" '{ c[$2" "$3]++ } END { for (k in c) printf "%d/%d %s\n", c[k], R, k }' "$scratch/all-failures.txt" | sort -rn >"$outfile"
echo "flake-rate: $(wc -l <"$outfile") tests failed at least once in $rounds rounds; rates in $outfile, logs in $scratch" >&2
