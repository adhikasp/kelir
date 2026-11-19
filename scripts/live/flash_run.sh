#!/usr/bin/env bash
# Run ONE flash scenario for ONE binary with winspy watching, then report.
#
# Usage: flash_run.sh <head|wip> <scenario> <canned|live> [pane|detached|hidden]
#   head / wip   use $FLASH/bin-head/kelir.exe or $FLASH/bin-wip/kelir.exe (copy the binaries there first)
#   scenario     calls | bg | cancel | kill | mix | tui   (see flash_scn.sh)
#   pane         run inside a Herdr pane (the pane has a console: the normal case)
#   detached     the scenario's root is started with DETACHED_PROCESS from a pane (no console at all)
#   hidden       the scenario's root is started with a hidden new console
# Needs: winspy.exe built (winspy-build.ps1), hc on PATH, python, and for `live` an env file with OPENROUTER_API_KEY.
# Output (under $FLASH): <name>.spy.log, <name>.pane.txt, <name>.report.txt; one JSON line appended to summary.jsonl.
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FLASH="${FLASH:-$HOME/kelir-scratch/flash}"
LIVE_ENV="${LIVE_ENV:-$HOME/kelir-scratch/live.env}"
GITBASH="C:/Program Files/Git/bin/bash.exe"
B="$1"; SCN="$2"; MODE="$3"; HOW="${4:-pane}"
BD="$FLASH/bin-$B"; WORK="$FLASH/work"; mkdir -p "$WORK"
NAME="flash-$SCN-$B-$MODE"; [ "$HOW" != pane ] && NAME="$NAME-${HOW:0:3}"
TAG="FLASHTAG_${NAME//-/_}"
SPY="$FLASH/$NAME.spy.log"; REPORT="$FLASH/$NAME.report.txt"
WBD="$(cygpath -m "$BD")"; WFLASH="$(cygpath -m "$FLASH")"; WHERE="$(cygpath -m "$HERE")"

hc rm "$NAME" > /dev/null 2>&1
# 1. spy up (the first line is written once the baseline is taken)
"$FLASH/winspy.exe" "$SPY" 170 > /dev/null 2>&1 &
for _ in $(seq 1 100); do grep -q " START " "$SPY" 2>/dev/null && break; sleep 0.1; done
sleep 0.3

SCRIPT="set -a; [ '$MODE' = live ] && . '$LIVE_ENV'; set +a; exec bash '$WHERE/flash_scn.sh' '$WBD' '$SCN' '$MODE' '$WFLASH/run'"
mkdir -p "$FLASH/run"
START=$(date +%s)
case "$SCN:$HOW" in
  tui:pane)
    TUISCRIPT="set -a; [ '$MODE' = live ] && . '$LIVE_ENV'; set +a; exec bash '$WHERE/flash_scn.sh' '$WBD' tui '$MODE' '$WFLASH/run'"
    hc run --bg --name "$NAME" --cwd "$WORK" "$GITBASH" -c "$TUISCRIPT" "$TAG" > /dev/null
    sleep 5
    hc send "$NAME" "Run this bash command: echo tui-bash-call; sleep 1. Then say done." > /dev/null
    for _ in $(seq 1 40); do hc read "$NAME" 2>/dev/null | grep -q "tui-bash-call" && break; sleep 0.5; done
    sleep 6
    hc keys "$NAME" ctrl+c ctrl+c > /dev/null 2>&1
    hc wait "$NAME" --timeout 20 > /dev/null 2>&1
    ;;
  *:pane)
    hc run --bg --name "$NAME" --cwd "$WORK" "$GITBASH" -c "$SCRIPT" "$TAG" > /dev/null
    hc wait "$NAME" --timeout 160 > /dev/null 2>&1
    ;;
  *:detached|*:hidden)
    OUTF="$WFLASH/$NAME.child.txt"
    LAUNCH="python -I '$WHERE/launch_detached.py' $HOW 150 '$OUTF' -- '$GITBASH' -c \"$SCRIPT\" $TAG"
    hc run --bg --name "$NAME" --cwd "$WORK" "$GITBASH" -c "$LAUNCH" "L_$TAG" > /dev/null
    hc wait "$NAME" --timeout 160 > /dev/null 2>&1
    ;;
esac
END=$(date +%s)
hc read "$NAME" > "$FLASH/$NAME.pane.txt" 2>&1
hc kill "$NAME" > /dev/null 2>&1; sleep 0.5; hc rm "$NAME" > /dev/null 2>&1
sleep 2   # let late windows/processes show up
touch "$SPY.stop"
for _ in $(seq 1 50); do grep -q VERDICT "$SPY" 2>/dev/null && break; sleep 0.2; done

# 2. leftovers: processes still alive that this scenario started (matched by tag / our bin dirs / the canned server / marker sleeps)
# Only real scenario processes: a name allow-list (never claude/node/sh/herdr, whose command line may merely QUOTE
# these patterns), and never an ancestor of this script. An earlier version matched on command line alone and
# killed the tester's own claude.exe.
LEFT="$(powershell.exe -NoProfile -Command "\$anc=@(); \$q=\$PID; while(\$q){ \$anc+=\$q; \$q=(Get-CimInstance Win32_Process -Filter \"ProcessId=\$q\").ParentProcessId; if(\$anc -contains \$q){break} }; Get-CimInstance Win32_Process | Where-Object { \$anc -notcontains \$_.ProcessId -and \$_.Name -match '^(kelir|bash|sleep|python|python3\.12|conhost|cmd)\.exe\$' -and \$_.CommandLine -match 'FLASHTAG_${NAME//-/_}|kelir-scratch.flash.bin-(head|wip)|canned_server.py [0-9]+|sleep(\.exe)?\"? (12[0-3]|6[123])\$' } | ForEach-Object { \"\$(\$_.ProcessId) \$(\$_.Name) \$(\$_.CommandLine.Substring(0,[Math]::Min(140,\$_.CommandLine.Length)))\" }" | tr -d '')"
{
  echo "=== $NAME  ($(( END - START )) s)  bin=$B scenario=$SCN mode=$MODE how=$HOW"
  python -I "$HERE/winspy_report.py" "$SPY" "$TAG" | grep -v '^{'
  echo "leftover processes after: ${LEFT:-none}"
  tail -3 "$FLASH/$NAME.pane.txt" | cut -c1-200
} | tee "$REPORT"
printf '%s' "${LEFT:-none}" > "$FLASH/$NAME.left.txt"
python -I "$HERE/winspy_report.py" "$SPY" "$TAG" | grep '^{' | python -I -c "
import json,sys
d=json.loads(sys.stdin.read()); d.update(name='$NAME', bin='$B', scenario='$SCN', mode='$MODE', how='$HOW', leftover=open('$WFLASH/$NAME.left.txt',encoding='utf-8').read().strip()[:200])
open('$WFLASH/summary.jsonl','a').write(json.dumps(d)+chr(10))"
# kill exact leftover PIDs (all of them were started by this scenario)
for p in $(echo "$LEFT" | awk '{print $1}'); do taskkill //F //T //PID "$p" > /dev/null 2>&1; done
