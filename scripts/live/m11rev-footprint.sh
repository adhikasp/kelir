#!/usr/bin/env bash
# M11 reviewer footprint probe: `kelir attach --follow --plain` against a
# synthetic 100k-event log. Samples the viewer's WorkingSetSize (bytes) on
# Windows. Kills only the PID it started, by exact PID, on exit.
set -uo pipefail
BIN="C:/Users/the maintainer/kelir-wt/m11rev/target/release/kelir.exe"
ROOT="${KELIR_FOOTPRINT_ROOT:-C:/Users/the maintainer/kelir-scratch/m11rev-footprint}"
rm -rf "$ROOT"; mkdir -p "$ROOT/state/sessions/ses_footprint" "$ROOT/work"
python - "$ROOT/state/sessions/ses_footprint/events.jsonl" <<'PY'
import json,sys
with open(sys.argv[1],'w') as f:
    for i in range(100000):
        f.write(json.dumps({"seq":i+1,"ts":"2026-01-01T00:00:00Z","type":"assistant",
          "step":1,"text":f"event {i}","reasoning":None,"tool_calls":[],
          "finish_reason":"stop"})+'\n')
PY
ls -lh "$ROOT/state/sessions/ses_footprint/events.jsonl"
export KELIR_STATE_DIR="$ROOT/state"
export KELIR_HOME="$ROOT/home"
"$BIN" attach ses_footprint --follow --plain \
  > "$ROOT/out.txt" 2> "$ROOT/err.txt" &
PID=$!
cleanup() { kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; }
trap cleanup EXIT
for i in 1 2 3 4 5 6 7 8; do
  sleep 0.5
  # PowerShell reports the exact PID's working set, never by image name.
  WINPID=$(ps -W 2>/dev/null | awk -v p="$PID" '$1==p {print $4}')
  WS=$(powershell.exe -NoProfile -Command "if ($WINPID) { (Get-Process -Id $WINPID -ErrorAction SilentlyContinue).WorkingSet64 }" 2>/dev/null | tr -d '\r')
  echo "sample $i: pid=$PID winpid=${WINPID:-?} working_set_bytes=${WS:-gone}"
done
