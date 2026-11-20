#!/usr/bin/env bash
# M6 live drive (reviewer): a real model uses `kelir mcp` from bash against the shipped test server
# and one real public server (@modelcontextprotocol/server-everything, installed in $EVERYTHING_CMD).
# Usage: m6_live.sh <scratch-dir> <scenario: a|b|c>      (OPENROUTER_API_KEY must be in the environment)
#   a  list servers/tools, call echo, pipe a structured result into jq
#   b  a 20 KB result called bare (spill notice) and then piped to wc -c
#   c  the real server: list tools, echo, get-sum
set -u
S="$1/m6$2"
D="$(cd "$(dirname "$0")" && pwd)"
BIN="${KELIR_BIN:-$D/../../target/release/kelir.exe}"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
: "${OPENROUTER_API_KEY:?OPENROUTER_API_KEY must be set}"
rm -rf "$S"; mkdir -p "$S/proj/.kelir" "$S/home" "$S/cfg"
export KELIR_STATE_DIR="$S/state" KELIR_HOME="$S/home" KELIR_CONFIG_DIR="$S/cfg"
SERVER="$(cd "$D/../../tests/support" && (cygpath -m "$PWD" 2>/dev/null || pwd))/mcp_server.py"
# the model types bare `kelir`: put this build first on PATH
BINDIR="$(cd "$(dirname "$BIN")" && pwd)"; export PATH="$BINDIR:$PATH"
{
  echo "[servers.demo]"
  echo "command = 'python'"
  echo "args = ['$SERVER']"
  echo "description = 'kelir test server: echo, structured, big, fail, image'"
  if [ -n "${EVERYTHING_CMD:-}" ]; then
    echo "[servers.everything]"
    echo "command = '$EVERYTHING_CMD'"
    echo "description = 'MCP reference server (echo, get-sum, ...)'"
  fi
} > "$S/proj/.kelir/mcp.toml"
case "$2" in
  a) TASK='Use the kelir CLI through bash. 1) Run `kelir mcp ls`. 2) Run `kelir mcp demo ls`. 3) Call the demo tool echo with text=hello. 4) Run `kelir mcp demo structured | jq .answer` and report the number. Finish with the single word ready.' ;;
  b) TASK='Use bash. 1) Run `kelir mcp demo big` with no pipe and tell me how many bytes the output notice said. 2) Run `kelir mcp demo big | wc -c` and report the number. Finish with the single word ready.' ;;
  c) TASK='Use bash and the kelir CLI. 1) Run `kelir mcp everything ls --detail name`. 2) Run `kelir mcp everything get-sum a=40 b=2` and report the sum. Finish with the single word ready.' ;;
esac
cd "$S/proj" || exit 1
"$BIN" run --json --cwd "$S/proj" --model "$MODEL" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt"
echo "kelir exit $?"
cp "$S"/state/sessions/*/events.jsonl "$S/events.jsonl"
python - "$S" <<'PY'
import json, sys
S = sys.argv[1]
ev = [json.loads(l) for l in open(S + "/events.jsonl", encoding="utf-8") if l.strip()]
for e in ev:
    t = e["type"]
    if t in ("tool_started",):
        print("tool_started", json.dumps(e.get("input"))[:200])
    elif t in ("mcp_started",):
        print("  mcp_started", e["server"], e["tool"], json.dumps(e["args"])[:100], "parent=", e.get("parent_call_id"))
    elif t in ("mcp_settled",):
        print("  mcp_settled", e["server"], e["tool"], e["outcome"], "exit", e["exit_code"], "bytes", e["bytes"], e["out_path"].replace("\\", "/")[-40:])
    elif t == "tool_settled":
        r = e.get("result")
        r = r if isinstance(r, str) else (r or {}).get("preview", "")
        print("tool_settled", e.get("state"), repr(r[:160]))
    elif t == "assistant":
        x = e["text"] if isinstance(e["text"], str) else e["text"].get("preview", "")
        if x.strip(): print("assistant:", x.strip()[:200].replace("\n", " "))
PY
