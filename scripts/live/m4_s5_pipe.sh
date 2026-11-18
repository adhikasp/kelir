#!/usr/bin/env bash
# Scenario 5: `kelir run --json | jq` must not hang on a `cmd &` (nor on a promoted job).
# Usage: m4_s5_pipe.sh <scratch-root>
source "$(dirname "$0")/m4_common.sh" "$1" s5
say "5a: command with a trailing & (sleep 25 &)"
T0=$(date +%s.%N)
"$BIN" run --json --model "$MODEL" 'Run exactly this one bash command: sleep 25 & echo started    Then reply "ok".' | jq -c 'select(.type=="tool_settled" or .type=="result")|{type,outcome,exit_code}'
A=$(awk -v n="$(date +%s.%N)" -v t="$T0" "BEGIN{print n-t}"); say "5a: pipeline finished after ${A}s"
say "5b: promoted job (promote_after=4, sleep 25)"
export KELIR_PROMOTE_AFTER=4
T0=$(date +%s.%N)
"$BIN" run --json --model "$MODEL" 'Run exactly this one bash command: sleep 25; echo done    Then reply "ok" without waiting for anything else.' | jq -c 'select(.type=="tool_settled" or .type=="result")|{type,outcome,exit_code}'
B=$(awk -v n="$(date +%s.%N)" -v t="$T0" "BEGIN{print n-t}"); say "5b: pipeline finished after ${B}s"
unset KELIR_PROMOTE_AFTER
procs 'sleep 25'
awk -v a="$A" -v b="$B" 'BEGIN{ if (a<15 && b<15) print "VERDICT: PASS"; else print "VERDICT: FAIL (hung on the background process)" }' | tee -a "$LOGF"
