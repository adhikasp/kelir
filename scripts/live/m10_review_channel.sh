#!/usr/bin/env bash
# M10 reviewer live drive: release `kelir channel` against the fake channel
# MCP bridge in scripts/live/review_channel_server.py and a canned mock
# provider. No keys, no network. Run from a Herdr pane.
set -uo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${KELIR_BIN:-$REPO/target/release/kelir}"
ROOT="${KELIR_SCRATCH:-/c/Users/the maintainer/kelir-scratch}/m10rev-live"
PORT="${M10_PORT:-18310}"
PY="${M10_PY:-python}"

rm -rf "$ROOT"; mkdir -p "$ROOT/work" "$ROOT/state"
cd "$ROOT/work" || exit 1

# Canned provider: first request answers with plain text, later ones "all done".
printf '%s\n' '[[],[{"id":"unused","name":"bash","args":{"command":"true"}}]]' > "$ROOT/turns.json"
# An empty first turn array means the canned server writes a tool_calls chunk
# with zero calls; instead use a plain-text turn by scripting the content via
# the mock in the test harness. For the live drive use a tiny inline server.
cat > "$ROOT/mock.py" <<'PY'
import http.server, json, sys
PORT = int(sys.argv[1])
n = {"i": 0}
def sse(chunks):
    return "".join("data: " + json.dumps(c) + "\n\n" for c in chunks) + "data: [DONE]\n\n"
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"
    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length", 0)))
        n["i"] += 1
        text = "LIVE answer from mock provider" if n["i"] == 1 else "all done"
        body = sse([
            {"choices": [{"delta": {"content": text}}]},
            {"choices": [{"delta": {}, "finish_reason": "stop"}]},
            {"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 5, "cost": 0}},
        ]).encode()
        self.send_response(200); self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self, *a): pass
http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
PY

: > "$ROOT/inbox.jsonl"; : > "$ROOT/replies.jsonl"; : > "$ROOT/events.jsonl"; : > "$ROOT/args.jsonl"
W="$(cygpath -m "$ROOT")"
cat > mcp.toml <<EOF
[servers.fake]
command = '$PY'
args = ['$(cygpath -w "$REPO/scripts/live/review_channel_server.py")']
channel = true
[servers.fake.env]
MCP_CHAN_INBOX = '$W/inbox.jsonl'
MCP_CHAN_REPLIES = '$W/replies.jsonl'
MCP_CHAN_EVENTS = '$W/events.jsonl'
MCP_CHAN_ARGS = '$W/args.jsonl'
MCP_CHAN_SECRETS = 'channel-canary-7f3a'
EOF

$PY -I "$(cygpath -w "$ROOT/mock.py")" "$PORT" >/dev/null 2>&1 & MOCK=$!
cleanup() { kill $MOCK $DRIVER 2>/dev/null; wait $MOCK $DRIVER 2>/dev/null; }
trap cleanup EXIT
for _ in $(seq 1 40); do (echo >/dev/tcp/127.0.0.1/$PORT) 2>/dev/null && break; sleep 0.1; done

export KELIR_MCP_CONFIG="$(cygpath -w "$ROOT/work/mcp.toml")"
export KELIR_STATE_DIR="$(cygpath -w "$ROOT/state")"
export KELIR_CONFIG_DIR="$(cygpath -w "$ROOT/noconfig")"
export KELIR_BASE_URL="http://127.0.0.1:$PORT/v1"
MSG='{"message_id":"live-1","conversation_id":"live-chat","sender":"Ada","content":"what is the Lighthouse score?","reply_to":"parent-7"}'
if [ -n "${M10_MSG:-}" ]; then MSG="$M10_MSG"; fi
export OPENROUTER_API_KEY=review-dummy

"$BIN" channel --server fake --max-messages 1 --status-once >"$ROOT/stdout.log" 2>"$ROOT/stderr.log" &
DRIVER=$!
echo "$MSG" >> "$ROOT/inbox.jsonl"

for _ in $(seq 1 100); do
  [ -s "$ROOT/replies.jsonl" ] && break
  sleep 0.1
done
kill $DRIVER 2>/dev/null; wait $DRIVER 2>/dev/null; DRIVER=

echo "=== status line"; cat "$ROOT/stdout.log"
echo "=== replies"; cat "$ROOT/replies.jsonl"
echo "=== reply args"; cat "$ROOT/args.jsonl"
echo "=== bridge events"; cat "$ROOT/events.jsonl"
echo "=== stderr"; cat "$ROOT/stderr.log"
echo "=== spool"; cat "$ROOT/state/spool/fake.jsonl"
echo "=== session user events"; grep -h '"type":"user"' "$ROOT"/state/sessions/*/events.jsonl 2>/dev/null | head -3
echo "=== canary leaks"; grep -rl 'channel-canary-7f3a' "$ROOT/state" "$ROOT/stderr.log" "$ROOT/stdout.log" 2>/dev/null || echo none
