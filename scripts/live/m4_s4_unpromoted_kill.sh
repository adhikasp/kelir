#!/usr/bin/env bash
# Scenario 4: a bash call that is NOT promoted; the session is killed; its tree must die.
# Usage: m4_s4_unpromoted_kill.sh <scratch-root>
source "$(dirname "$0")/m4_common.sh" "$1" s4
TASK='Run exactly this one bash command: bash -c "sleep 311 & sleep 312 & wait"   Nothing else.'
say "model=$MODEL (default promote_after, never reached)"
"$BIN" run --keep --json --model "$MODEL" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt" &
PID=$!
find_log || { say "no log"; exit 3; }
for _ in $(seq 1 300); do [ "$(procs 'sleep(.exe)?.? (311|312)' | grep -c 'sleep.exe')" -ge 2 ] && break; sleep 0.2; done
say "--- BEFORE kill ---"; procs 'sleep(.exe)?.? (311|312)'
BEFORE=$(procs 'sleep(.exe)?.? (311|312)' | grep -c 'sleep.exe')
killwin "$(winpid $PID)"; wait $PID 2>/dev/null; T0=$(date +%s.%N)
for _ in $(seq 1 100); do [ "$(procs 'sleep(.exe)?.? (311|312)' | grep -c 'sleep.exe')" -eq 0 ] && break; sleep 0.1; done
say "tree gone $(awk -v n="$(date +%s.%N)" -v t="$T0" "BEGIN{print n-t}" 2>/dev/null)s after the kill"
say "--- AFTER kill ---"; procs 'sleep(.exe)?.? (311|312)'
AFTER=$(procs 'sleep(.exe)?.? (311|312)' | grep -c 'sleep.exe')
tail -n 3 "$LOG" | jq -c '{seq,type}'
[ "$BEFORE" -ge 2 ] && [ "$AFTER" -eq 0 ] && say "VERDICT: PASS" || say "VERDICT: FAIL (before=$BEFORE after=$AFTER)"
