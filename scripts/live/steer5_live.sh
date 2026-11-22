#!/usr/bin/env bash
# STEER-5 live drive (reviewer). Usage: steer5_live.sh <scenario> <scratch-dir>
#   flaky   : 429 (Retry-After 3), 429 (no header), 503, then ok -> the turn must finish, with waits
#   stop    : a 60 s Retry-After wait is ended by `ctl stop` within 1 s -> outcome stopped
#   now     : the same wait, but `ctl say --now` -> does the wait end at once?
#   pause   : budget 4 s vs Retry-After 3 forever -> paused, exit 0; then `kelir run --resume` retries
#   cut     : a stream cut before any tool call -> retried
#   overlap : two real kelir vs OpenRouter, max_concurrent_requests=1 -> peak in flight must be 1
#   control : the same without the knob -> peak is expected to reach 2
# OPENROUTER_API_KEY must be in the environment for overlap/control (the pane sources the env file).
set -u
SC="$1"; S="$2"
D="$(cd "$(dirname "$0")" && pwd)"
BIN="${KELIR_BIN:-$D/../../target/release/kelir.exe}"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
rm -rf "$S/$SC"; mkdir -p "$S/$SC/work" "$S/$SC/cfg"; W="$S/$SC"
export KELIR_STATE_DIR="$W/state"
say() { echo "[$(date +%T.%3N)] $*" | tee -a "$W/driver.log"; }
PORT=$((20000 + RANDOM % 2000))
SERVER=""
start_flaky() { python "$D/steer5_flaky_server.py" "$PORT" "$W/requests.jsonl" "$1" > /dev/null 2>&1 & SERVER=$!; sleep 1; }
cleanup() { [ -n "$SERVER" ] && kill "$SERVER" 2> /dev/null; [ -n "${PROXY:-}" ] && kill "$PROXY" 2> /dev/null; [ -n "${K1:-}" ] && kill "$K1" 2> /dev/null; [ -n "${K2:-}" ] && kill "$K2" 2> /dev/null; true; }
trap cleanup EXIT
export KELIR_BASE_URL="http://127.0.0.1:$PORT/api/v1"
export OPENROUTER_API_KEY="${OPENROUTER_API_KEY:-canned-key}"
cd "$W/work" || exit 1
sid() { ls "$W"/state/sessions 2> /dev/null | head -n1; }
types() { python - "$1" <<'PY'
import json,sys
for l in open(sys.argv[1], encoding="utf-8"):
    e=json.loads(l)
    if e["type"] in ("error","turn_ended","session_ended"):
        print({k:v for k,v in e.items() if k in ("type","where","status","attempt","retry_in_ms","retryable","outcome","reason")}, (e.get("final_text") or "")[:120] if e["type"]=="turn_ended" else "")
PY
}
case "$SC" in
flaky)
  start_flaky "429:3,429:-,503,ok"
  say "run (human output)"
  T0=$(date +%s.%N)
  "$BIN" run --model test/m "say hi" ; say "exit=$?"
  say "elapsed=$(python -c "import time;print(round(time.time()-$T0,1))")s"
  cat "$W/requests.jsonl"; types "$W"/state/sessions/*/events.jsonl ;;
stop|now)
  start_flaky "429:60,ok"
  printf '[retry]\nmax_wait = 600\n' > "$W/cfg/config.toml"
  "$BIN" run --keep --json --config "$W/cfg/config.toml" --model test/m "go" > "$W/out.ndjson" 2> "$W/err.txt" & K1=$!
  for _ in $(seq 1 100); do grep -q '"retry_in_ms"' "$W"/state/sessions/*/events.jsonl 2> /dev/null && break; sleep 0.1; done
  ID=$(sid); say "wait started for session $ID"
  T0=$(date +%s.%N)
  if [ "$SC" = stop ]; then "$BIN" ctl "$ID" stop "enough" | head -c 300; echo
  else "$BIN" ctl "$ID" say --now "change of plan: say hi" | head -c 300; echo; fi
  for _ in $(seq 1 120); do grep -q '"type":"turn_ended"' "$W"/state/sessions/*/events.jsonl 2> /dev/null && break; sleep 0.1; done
  say "turn ended after $(python -c "import time;print(round(time.time()-$T0,2))")s"
  types "$W"/state/sessions/*/events.jsonl; cat "$W/requests.jsonl"
  "$BIN" ctl "$ID" end > /dev/null; wait $K1; say "process exit=$?" ;;
pause)
  start_flaky "429:3,429:3,429:3,429:3,429:3,ok"
  printf '[retry]\nmax_wait = 4\n' > "$W/cfg/config.toml"
  "$BIN" run --json --config "$W/cfg/config.toml" --model test/m "go" > "$W/out.ndjson" 2> "$W/err.txt"; say "run exit=$?"
  types "$W"/state/sessions/*/events.jsonl; cat "$W/err.txt"
  ID=$(sid); say "resume $ID"
  "$BIN" resume --json --config "$W/cfg/config.toml" "$ID" > "$W/out2.ndjson" 2> "$W/err2.txt"; say "resume exit=$?"
  types "$W"/state/sessions/*/events.jsonl; cat "$W/requests.jsonl" ;;
cut)
  start_flaky "cut,ok"
  "$BIN" run --model test/m "say hi"; say "exit=$?"
  types "$W"/state/sessions/*/events.jsonl; cat "$W/requests.jsonl" ;;
overlap|control)
  PROXY_PORT=$PORT
  python "$D/steer5_overlap_proxy.py" "$PROXY_PORT" "$W/overlap.jsonl" > /dev/null 2>&1 & PROXY=$!; sleep 1
  : > "$W/cfg/config.toml"
  if [ "$SC" = overlap ]; then printf '[providers.openrouter]\nmax_concurrent_requests = 1\n' > "$W/cfg/config.toml"; fi
  TASK='Run these three bash commands, one per call, in order: echo one; echo two; echo three. Then reply with the single word done.'
  mkdir -p "$W/w1" "$W/w2"
  (cd "$W/w1" && "$BIN" run --json --config "$W/cfg/config.toml" --model "$MODEL" "$TASK" > "$W/out1.ndjson" 2> "$W/err1.txt"; echo "exit1=$?" >> "$W/driver.log") & K1=$!
  (cd "$W/w2" && "$BIN" run --json --config "$W/cfg/config.toml" --model "$MODEL" "$TASK" > "$W/out2.ndjson" 2> "$W/err2.txt"; echo "exit2=$?" >> "$W/driver.log") & K2=$!
  wait $K1 $K2
  python - "$W/overlap.jsonl" <<'PY'
import json,sys
ev=[json.loads(l) for l in open(sys.argv[1])]
starts=[e for e in ev if e["ev"]=="start"]; ends=[e for e in ev if e["ev"]=="end"]
print("requests:",len(starts),"peak in flight:",max(e["inflight"] for e in starts))
PY
  cat "$W/driver.log" ;;
esac
