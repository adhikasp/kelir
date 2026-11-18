#!/usr/bin/env bash
# Scenario 3: cancel a job whose command spawns a child of a child; no orphan afterwards.
# The model starts the job (real `kelir bg run`); this script cancels it and inspects the
# OS process list before and after.
# Usage: m4_s3_cancel_tree.sh <scratch-root>
source "$(dirname "$0")/m4_common.sh" "$1" s3
TASK='Run this exact bash command: kelir bg run "bash -c '"'"'sleep 301 & sleep 302 & wait'"'"' & sleep 303 & wait"   Then run: sleep 4   Then run kelir bg ls. Then reply "ready". Do not cancel anything.'
say "model=$MODEL"
"$BIN" run --keep --json --model "$MODEL" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt" &
PID=$!
find_log || { say "no log"; exit 3; }
wait_for '"type":"turn_ended"' 1 120 || { say "turn did not end"; exit 4; }
say "--- BEFORE cancel: processes ---"; procs 'sleep(.exe)?.? (301|302|303)'
BEFORE=$(procs 'sleep(.exe)?.? (301|302|303)' | grep -c 'sleep.exe'); say "count before=$BEFORE"
ID="$(grep '"type":"bg_started"' "$LOG" | head -1 | jq -r .bg_id)"; say "job $ID"
say "cancel #1:"; "$BIN" bg cancel "$ID"
say "cancel #2 (again):"; "$BIN" bg cancel "$ID"
sleep 2
say "--- AFTER cancel: processes ---"; procs 'sleep(.exe)?.? (301|302|303)'
AFTER=$(procs 'sleep(.exe)?.? (301|302|303)' | grep -c 'sleep.exe'); say "count after=$AFTER"
wait_for '"type":"bg_settled"' 1 10 && say "session recorded bg_settled" && grep '"type":"bg_settled"' "$LOG" | jq -c '{state,exit_code,bg_id}'
"$BIN" ctl latest end > /dev/null 2>&1; wait $PID
[ "$BEFORE" -ge 3 ] && [ "$AFTER" -eq 0 ] && say "VERDICT: PASS (>=3 processes before, 0 after)" || say "VERDICT: FAIL (before=$BEFORE after=$AFTER)"
