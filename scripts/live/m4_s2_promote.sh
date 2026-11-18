#!/usr/bin/env bash
# Scenario 2: promotion at 5 s of a 20 s command, taskkill the session, the job survives,
# `kelir bg out` reads it, `kelir resume` shows it settled.
# Usage: m4_s2_promote.sh <scratch-root>
source "$(dirname "$0")/m4_common.sh" "$1" s2
export KELIR_PROMOTE_AFTER=5
TASK='Run exactly this one bash command and nothing else, then say "started": for i in 1 2 3 4; do echo tick $i; sleep 5; done; echo finished-the-job'
say "model=$MODEL promote_after=$KELIR_PROMOTE_AFTER"
"$BIN" run --keep --json --model "$MODEL" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt" &
PID=$!
find_log || { say "no log"; exit 3; }
SID="$(basename "$(dirname "$LOG")")"; say "session $SID"
wait_for '"outcome":"promoted"' 1 60 || { say "NOT PROMOTED"; tail -5 "$LOG"; exit 4; }
say "promoted:"; grep '"outcome":"promoted"' "$LOG" | jq -c '{seq,call_id,outcome,out_path,result:(.result|.[0:200])}'
wait_for '"type":"bg_started"' 1 10 && say "bg_started logged"
BGDIR="$(dirname "$LOG")/bg"
ID="$(grep '"outcome":"promoted"' "$LOG" | jq -r '.out_path' | xargs dirname | xargs basename)"
JOBPID="$(jq -r .pid "$BGDIR/$ID.status")"; SHIM="$(jq -r .shim "$BGDIR/$ID.status")"
say "job id=$ID command pid=$JOBPID shim pid=$SHIM"
wait_for '"type":"turn_ended"' 1 60; say "turn ended; killing the session (taskkill /F /PID, no /T)"
killwin "$(winpid $PID)"; wait $PID 2>/dev/null
sleep 1
alive "$JOBPID" && say "job command pid $JOBPID ALIVE after session kill" || say "job command pid $JOBPID DEAD after session kill"
alive "$SHIM" && say "shim pid $SHIM ALIVE" || say "shim pid $SHIM DEAD"
say "--- kelir bg out $ID (no session) ---"; "$BIN" bg out "$ID"
say "--- kelir bg wait $ID --timeout 40 ---"; "$BIN" bg wait "$ID" --timeout 40; say "wait exit=$?"
say "--- kelir bg out $ID after finish ---"; "$BIN" bg out "$ID"
jq -c . "$BGDIR/$ID.status"
say "--- kelir resume ---"
"$BIN" resume --json --model "$MODEL" "$SID" > "$S/resume.ndjson" 2> "$S/resume.err"; say "resume exit=$?"
jq -c 'select(.type|test("bg_|resumed|user|session_ended")) | {seq,type,from,bg_id,state,exit_code,text:((.text//"")|tostring|.[0:120])}' "$LOG"
