#!/usr/bin/env bash
# Live scenario 1: edit creates hello.txt, bash cats it.
source "$(dirname "$0")/m0_common.sh" s1
"$KELIR_BIN" run --json --provider openrouter --model "$MODEL" \
  "Create hello.txt containing the word hi using the edit tool, then cat it with bash and tell me what it says." > out.ndjson
echo "exit=$?"
jq -c 'select(.type|test("tool_|usage|turn_ended|result|error"))|{seq,type,call_id,name,outcome,cached_tokens,prompt_tokens,cost,steps,final_text}' out.ndjson
