#!/usr/bin/env bash
# Live F1 check: a real model runs `sleep 6 & echo started`; the call must return fast.
source "$(dirname "$0")/m0_common.sh" f1
"$KELIR_BIN" run --json --provider openrouter --model "$MODEL" \
  "Run exactly this one bash command and nothing else, then say what it printed: sleep 6 & echo started" > out.ndjson
echo "exit=$?"
jq -c 'select(.type=="tool_started" or .type=="tool_settled")|{seq,ts,type,input,outcome,result}' out.ndjson
python - <<'PY'
import json
ev=[json.loads(l) for l in open("out.ndjson") if l.strip().startswith("{")]
t={e["type"]:e["ts"] for e in ev if e["type"] in("tool_started","tool_settled")}
from datetime import datetime
f=lambda s: datetime.fromisoformat(s.replace("Z","+00:00")[:26]+"+00:00")
print("tool call duration: %.2fs" % (f(t["tool_settled"])-f(t["tool_started"])).total_seconds())
PY
