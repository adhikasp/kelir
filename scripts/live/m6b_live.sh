#!/usr/bin/env bash
# M6 round-2 live drive (reviewer): a real model drives `kelir mcp` against the test servers and real
# public servers (npx server-everything, server-filesystem, uvx mcp-server-time).
# Usage: m6b_live.sh <scratch-dir> <scenario: a|b>     (OPENROUTER_API_KEY from the environment)
#   a  list, jq pipes (test server, everything, filesystem), a tool argument with a canary
#   b  a long-running real-server call (the reviewer kills the session mid-call from outside)
set -u
S="$1/m6b$2"
D="$(cd "$(dirname "$0")" && pwd)"
BIN="${KELIR_BIN:-$D/../../target/release/kelir.exe}"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
: "${OPENROUTER_API_KEY:?OPENROUTER_API_KEY must be set}"
rm -rf "$S"; mkdir -p "$S/proj/.kelir" "$S/home" "$S/cfg" "$S/fsroot"
echo "hello from the scratch file" > "$S/fsroot/note.txt"
export KELIR_STATE_DIR="$S/state" KELIR_HOME="$S/home" KELIR_CONFIG_DIR="$S/cfg"
TS="$(cd "$D/../../tests/support" && (cygpath -m "$PWD" 2>/dev/null || pwd))"
FS="$(cd "$S/fsroot" && (cygpath -m "$PWD" 2>/dev/null || pwd))"
BINDIR="$(cd "$(dirname "$BIN")" && pwd)"; export PATH="$BINDIR:$PATH"
ENV_CANARY="CANARY-ENV-live-$RANDOM$RANDOM"
echo "$ENV_CANARY" > "$S/env_canary.txt"
PY=python; command -v python3 >/dev/null 2>&1 && ! command -v python >/dev/null 2>&1 && PY=python3
{
  echo "[servers.demo]"
  echo "command = '$PY'"
  echo "args = ['$TS/mcp_server.py']"
  echo "description = 'kelir test server: echo, structured, big, fail, image'"
  echo "[servers.s]"
  echo "command = '$PY'"
  echo "args = ['$TS/m6b_server.py']"
  echo "env = { M6B_SECRET = '$ENV_CANARY' }"
  echo "description = 'reviewer server: echo, argv, leak'"
  echo "[servers.everything]"
  echo "command = 'npx'"
  echo "args = ['-y', '@modelcontextprotocol/server-everything']"
  echo "description = 'MCP reference server'"
  echo "timeout = 150"
  echo "[servers.fs]"
  echo "command = 'npx'"
  echo "args = ['-y', '@modelcontextprotocol/server-filesystem', '$FS']"
  echo "description = 'filesystem server on a scratch dir'"
  echo "timeout = 150"
} > "$S/proj/.kelir/config.toml"
case "$2" in
  a) TASK="Use bash and the kelir CLI only. 1) Run \`kelir mcp ls\`. 2) Run \`kelir mcp demo structured | jq .answer\` and report the number. 3) Run \`kelir mcp s echo token=CANARY-TOOLARG-live text=hi\`. 4) Run \`kelir mcp everything get-sum a=40 b=2\`. 5) Run \`kelir mcp fs list_directory path=$FS | jq -r .content\` and report the file name. Finish with the single word ready." ;;
  b) TASK='Use bash. Run exactly: kelir mcp everything trigger-long-running-operation duration=120 steps=4 . It takes two minutes; wait for it, then say done.' ;;
esac
cd "$S/proj" || exit 1
"$BIN" run --json --cwd "$S/proj" --model "$MODEL" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt" &
SESSION=$!
echo "$SESSION" > "$S/session.pid"
wait "$SESSION"
echo "kelir exit $?"
cp "$S"/state/sessions/*/events.jsonl "$S/events.jsonl"
python - "$S" <<'PY'
import json, sys
S = sys.argv[1]
ev = [json.loads(l) for l in open(S + "/events.jsonl", encoding="utf-8") if l.strip()]
for e in ev:
    t = e["type"]
    if t == "tool_started":
        print("tool_started", json.dumps(e.get("input"))[:220])
    elif t == "mcp_started":
        print("  mcp_started", e["server"], e["tool"], json.dumps(e["args"])[:100], "parent=", e.get("parent_call_id"))
    elif t == "mcp_settled":
        print("  mcp_settled", e["server"], e["tool"], e["outcome"], "exit", e["exit_code"], "bytes", e["bytes"])
    elif t == "tool_settled":
        r = e.get("result")
        r = r if isinstance(r, str) else (r or {}).get("preview", "")
        print("tool_settled", e.get("state"), repr(r[:200]))
    elif t == "assistant":
        x = e["text"] if isinstance(e["text"], str) else e["text"].get("preview", "")
        if x.strip(): print("assistant:", x.strip()[:200].replace("\n", " "))
canary = open(S + "/env_canary.txt").read().strip()
leaks = []
for name in ("events.jsonl", "out.ndjson", "err.txt"):
    if canary in open(S + "/" + name, encoding="utf-8", errors="replace").read():
        leaks.append(name)
print("env canary leaks in:", leaks or "none")
PY
