#!/usr/bin/env bash
# Live scenario 3: two `sleep 3` in one message -> wall < 6 s, results in call order.
# Runs through logging_proxy.py (started by the caller on :18081) to capture request bodies.
source "$(dirname "$0")/m0_common.sh" s3
export KELIR_BASE_URL="http://127.0.0.1:18081/api/v1"
start=$(date +%s)
"$KELIR_BIN" run --json --provider openrouter --model "$MODEL" \
  "In ONE assistant message, make two parallel bash tool calls: first 'sleep 3; echo first', second 'sleep 3; echo second'. Do not call them one after the other. Then report both outputs." > out.ndjson
echo "exit=$? wall=$(( $(date +%s) - start ))s"
jq -c 'select(.type|test("assistant|tool_"))|{seq,ts,type,call_id,outcome,result,n:(.tool_calls|length?)}' out.ndjson
