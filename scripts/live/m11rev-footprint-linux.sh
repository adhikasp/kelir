#!/usr/bin/env bash
# M11 reviewer footprint probe (Linux, run through `hc exec --on <machine>`).
# Builds the release binary from the gate's committed HEAD, generates a
# 100k-event log, samples VmRSS of `kelir attach --follow --plain`, and kills
# only the PID it started. No process is killed by image name.
set -uo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
ROOT="${HOME}/m11rev-footprint-linux"
mkdir -p "$ROOT"
TREE="${HOME}/kelir-verify-m11rev/src-tree"
BIN="${TREE}/target/release/kelir"
if [ ! -x "$BIN" ]; then
  (cd "$TREE" && cargo build --release -j 4 > "$ROOT/build.log" 2>&1) || {
    echo "release build failed"; tail -20 "$ROOT/build.log" 2>/dev/null; exit 1; }
fi
rm -rf "$ROOT/state" "$ROOT/work"; mkdir -p "$ROOT/state/sessions/ses_footprint" "$ROOT/work"
python3 - "$ROOT/state/sessions/ses_footprint/events.jsonl" <<'PY'
import json,sys
with open(sys.argv[1],'w') as f:
    for i in range(100000):
        f.write(json.dumps({"seq":i+1,"ts":"2026-01-01T00:00:00Z","type":"assistant",
          "step":1,"text":f"event {i}","reasoning":None,"tool_calls":[],
          "finish_reason":"stop"})+'\n')
PY
ls -lh "$ROOT/state/sessions/ses_footprint/events.jsonl"
cd "$ROOT/work" || exit 1
KELIR_STATE_DIR="$ROOT/state" KELIR_HOME="$ROOT/home" \
  "$BIN" attach ses_footprint --follow --plain > "$ROOT/out.txt" 2> "$ROOT/err.txt" &
PID=$!
cleanup() { kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; }
trap cleanup EXIT
for i in 1 2 3 4 5 6 7 8; do
  sleep 0.5
  RSS=$(awk '/VmRSS:/ {print $2}' "/proc/$PID/status" 2>/dev/null)
  echo "sample $i: pid=$PID rss_kb=${RSS:-gone}"
done
