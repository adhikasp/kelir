#!/usr/bin/env bash
# Run the whole suite once with a background CPU load, to catch the timing
# flakes that only appear when the box is busy (STEER-9 stress mode).
#
# usage: scripts/gate-load.sh [threads] [-- <extra nextest args>]
#
# The load is `threads` busy loops (default 4), started by this script, each in
# its own process group, and killed by exact PID when the run ends — never by
# image name, and never with a console signal (AGENTS.md hard rules). A trap
# makes that true on failure and on interrupt too.
set -uo pipefail
# No core dumps (BUILD-INFRA rule 6, docs/BUILD.md). The suite this runs includes
# the deliberate aborts of the crash-injection tests, and a box with core dumps
# enabled wrote a ~290 KB core for each one. Set here, before any test binary
# starts, so the load loops and nextest's children inherit it; the gate itself
# does the same, this is for a direct run of this script.
case "$(uname -s 2>/dev/null)" in
  Linux | Darwin) ulimit -c 0 2>/dev/null || true ;;
esac
threads="${1:-4}"
shift || true
[ "${1:-}" = "--" ] && shift
root="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)" || exit 2
cd "$root" || exit 1

load_pids=()
stop_load() {
  for pid in "${load_pids[@]+"${load_pids[@]}"}"; do
    kill -9 "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
  done
  load_pids=()
}
trap 'stop_load' EXIT INT TERM

busy='
n=0; while :; do n=$((n + 1)); done
'
i=0
while [ "$i" -lt "$threads" ]; do
  bash -c "$busy" >/dev/null 2>&1 &
  load_pids+=("$!")
  i=$((i + 1))
done
echo "gate-load: $threads busy loops: pids ${load_pids[*]}" >&2

# Process-count watchdog (incident 2026-11-20: about 1000 runaway sh.exe from a
# test). Counts `sh`/`bash` before the run and TERMs the nextest run (by its exact
# pid, which stops its tests) if the count grows by more than 40.
# Descendants of one pid, counted by walking the parent-pid graph. The
# incident was about 1000 runaway sh.exe from one suite; a global count also
# trips when a sibling worker starts its own agents, which is not our leak.
# Windows: PowerShell/CIM. Unix: `ps -eo pid=,ppid=`.
descendant_count() {
  local root=$1
  case "$(uname -s 2>/dev/null)" in
    Linux | Darwin)
      ps -eo pid=,ppid= 2>/dev/null | awk -v root="$root" '
        { pid[$1]=$2 } END {
          for (p in pid) { n=0; x=p; while (x!="" && x!=root && ++n<1000) x=pid[x]; if (x==root) c++ }
          print c+0
        }' ;;
    *) WD_ROOT="$root" powershell -NoProfile -Command '
        $r = [int]$env:WD_ROOT
        $all = @(Get-CimInstance Win32_Process)
        $seen = New-Object "System.Collections.Generic.HashSet[int]"
        [void]$seen.Add($r)
        for ($i = 0; $i -lt 12; $i++) {
          $next = @($all | Where-Object { $seen.Contains([int]$_.ParentProcessId) } |
                    ForEach-Object { [int]$_.ProcessId })
          if ($next.Count -eq 0) { break }
          foreach ($n in $next) { [void]$seen.Add($n) }
        }
        Write-Output ([string]($seen.Count - 1))
      ' 2>/dev/null | tr -d '\r' ;;
  esac
}
cargo nextest run --no-fail-fast --retries 0 -j 4 "$@" &
suite=$!
(
  while kill -0 "$suite" 2>/dev/null; do
    sleep 10
    now="$(descendant_count "$suite")"
    if [ "${now:-0}" -gt 200 ]; then
      echo "gate-load: WATCHDOG $now descendants of the suite (pid $suite): stopping the run" >&2
      kill -TERM "$suite" 2>/dev/null
      exit
    fi
  done
) &
watchdog=$!
wait "$suite"
rc=$?
kill "$watchdog" 2>/dev/null
stop_load
trap - EXIT INT TERM
echo "gate-load: load stopped, suite rc=$rc" >&2
exit "$rc"
