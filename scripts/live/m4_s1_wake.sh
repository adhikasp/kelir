#!/usr/bin/env bash
# Scenario 1: a real model starts `sleep 60; echo ok` with `kelir bg run`, carries on, is woken.
# Usage: m4_s1_wake.sh <scratch-root>     (OPENROUTER_API_KEY in the environment)
source "$(dirname "$0")/m4_common.sh" "$1" s1
TASK='Start a background job with exactly this one bash command: kelir bg run "sleep 60; echo ok"   Do not wait for it. Meanwhile run: echo working > progress.txt   then reply with the single word "waiting". Later you will be told when the job finished; then run kelir bg out <id> (the id printed when you started it) and reply with what it printed in one line.'
say "model=$MODEL"
"$BIN" run --keep --json --model "$MODEL" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt" &
PID=$!
find_log || { say "no log"; exit 3; }
say "session $(basename "$(dirname "$LOG")")"
wait_for '"type":"bg_started"' 1 90 && say "bg_started seen"
wait_for '"type":"turn_ended"' 1 90 && say "first turn ended (model said waiting)"
T0=$(date +%s)
wait_for '"type":"bg_settled"' 1 150 && say "bg_settled after $(( $(date +%s) - T0 ))s of idle waiting"
wait_for '"type":"turn_ended"' 2 120 && say "woken turn ended"
"$BIN" ctl latest end > /dev/null 2>&1
wait $PID; say "kelir exit=$?"
echo "--- events (compact) ---"
jq -c 'select(.type|test("bg_|user|tool_settled|turn_ended")) | {seq,type,from,bg_id,call_id,outcome,state,exit_code,text:((.text//.final_text//.result//"")|tostring|.[0:110])}' "$LOG"
echo "--- bg dir ---"; ls "$(dirname "$LOG")/bg"
echo "--- processes still matching sleep 60 ---"; procs 'sleep 60'
