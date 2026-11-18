#!/usr/bin/env bash
# Live scenario 5: which shell did bash resolve to, and does a CRLF file keep CRLF through edit.
source "$(dirname "$0")/m0_common.sh" s5
printf 'alpha\r\nbeta\r\ngamma\r\n' > crlf.txt
"$KELIR_BIN" run --json --provider openrouter --model "$MODEL" \
  "Use the edit tool to replace the line 'beta' with 'BETA' in crlf.txt (do not rewrite the file any other way). Then use bash to run: echo \$BASH_VERSION; uname -a; command -v bash   and tell me the output." > out.ndjson
echo "exit=$?"
jq -c 'select(.type=="tool_settled")|{call_id,outcome,result}' out.ndjson
echo "--- bytes of crlf.txt:"; od -c crlf.txt | head
jq -r 'select(.type=="result")|.final_text' out.ndjson
