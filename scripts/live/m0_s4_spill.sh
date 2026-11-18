#!/usr/bin/env bash
# Live scenario 4: >10 KB output -> spill notice, file on disk, model reads part with sed -n.
source "$(dirname "$0")/m0_common.sh" s4
"$KELIR_BIN" run --json --provider openrouter --model "$MODEL" \
  "Run: for i in \$(seq 1 3000); do echo \"line \$i of the big output\"; done   with bash. It will be long. Then, using a second bash call, read ONLY lines 1500-1502 of the saved output file the notice names (sed -n) and tell me what they say." > out.ndjson
echo "exit=$?"
jq -c 'select(.type=="tool_settled")|{call_id,outcome,bytes,lines,spilled,out_path,result:(.result|.[0:260])}' out.ndjson
jq -r 'select(.type=="result")|.final_text' out.ndjson
