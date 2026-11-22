#!/usr/bin/env bash
# m6b F6b on Linux: kill -9 the session while a REAL node MCP server (npx server-everything) is mid-call.
# usage: m6b_linux_orphan.sh <kelir-binary> <canned_server.py>    (no keys, a canned local model)
set -u
BIN="$1"; CANNED="$2"; W="$(mktemp -d)"; PORT=18765
mkdir -p "$W/proj/.kelir" "$W/state" "$W/home"
cat > "$W/proj/.kelir/config.toml" <<T
[servers.everything]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-everything"]
timeout = 150
T
python3 "$CANNED" $PORT "\"\$KELIR_BIN\" mcp everything trigger-long-running-operation duration=100 steps=4" &
MODEL=$!
sleep 1
export KELIR_STATE_DIR="$W/state" KELIR_HOME="$W/home" KELIR_CONFIG_DIR="$W/cfg" KELIR_BASE_URL="http://127.0.0.1:$PORT" OPENROUTER_API_KEY=x
cd "$W/proj"
setsid "$BIN" run --json --model test/model "go" > "$W/out" 2> "$W/err" &
sleep 20
SESSION=$(pgrep -f "$BIN run --json --model test/model" | head -1)
echo "session pid: $SESSION"
echo "--- tree before kill"; ps -eo pid,ppid,pgid,stat,cmd --forest | grep -E "$SESSION|server-everything|npm exec|mcp-server" | grep -v grep
kill -9 "$SESSION"
sleep 8
echo "--- after kill -9 + 8s (anything left that mentions the project's server)"
LEFT=$(ps -eo pid,ppid,stat,cmd | grep -E "server-everything|mcp-server-everything" | grep -v grep)
echo "$LEFT"
echo "orphans: $(echo -n "$LEFT" | grep -c .)"
echo "$LEFT" | awk '{print $1}' | xargs -r kill -9
kill $MODEL 2>/dev/null; rm -rf "$W"
