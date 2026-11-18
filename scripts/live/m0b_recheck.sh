#!/usr/bin/env bash
# M0 re-verification F1-F6 against the release binary, with a canned SSE server (no keys, no network).
# Usage: m0b_recheck.sh   (needs python, jq, a release build)
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
K="${KELIR_BIN:-$REPO/target/release/kelir}"
ROOT="${KELIR_SCRATCH:-$HOME/kelir-scratch/M0b}/recheck"
rm -rf "$ROOT"; mkdir -p "$ROOT"
export OPENROUTER_API_KEY=dummy
PORT=18100

case_run() { # name turns-json
  local name=$1 turns=$2
  PORT=$((PORT+1))
  mkdir -p "$ROOT/$name/work"; cd "$ROOT/$name/work"
  export KELIR_STATE_DIR="$ROOT/$name/state" KELIR_BASE_URL="http://127.0.0.1:$PORT/v1"
  printf '%s' "$turns" > "$ROOT/$name/turns.json"
  python -I "$REPO/scripts/live/canned_server.py" $PORT "@$ROOT/$name/turns.json" >/dev/null 2>&1 &
  SP=$!; for i in $(seq 1 30); do (echo > /dev/tcp/127.0.0.1/$PORT) 2>/dev/null && break; sleep 0.2; done
  T0=$(date +%s.%N 2>/dev/null || date +%s)
  "$K" run --json go > ../out.ndjson 2>../err.txt; CODE=$?
  WALL=$(( $(date +%s) - ${T0%.*} ))
  kill $SP 2>/dev/null
  SESS=$(ls -d "$KELIR_STATE_DIR"/sessions/*)
}

echo "== F1: 'sleep 6 & echo started'"
case_run f1 '[[{"id":"c1","name":"bash","args":{"command":"sleep 6.123 & echo started"}}]]'
echo "exit=$CODE wall=${WALL}s result=$(jq -r 'select(.type=="tool_settled")|.result' ../out.ndjson | tr -d '\n')"
echo "leftover sleep after kelir exit: $(powershell -NoProfile -Command "@(Get-CimInstance Win32_Process -Filter \"Name='sleep.exe'\" | Where-Object { \$_.CommandLine -like '*6.123*' }).Count" | tr -d '\r')"

echo "== F3: call id ../../escaped"
case_run f3 '[[{"id":"../../escaped","name":"bash","args":{"command":"echo pwned"}}]]'
echo "exit=$CODE; commands/: $(ls "$SESS/commands" | tr '\n' ' ')"; echo "escaped exists: $([ -e "$SESS/escaped" ] || [ -e "$SESS/../escaped" ] && echo YES || echo no)"
jq -c 'select(.type=="tool_settled")|{call_id,out_path}' ../out.ndjson

echo "== F4: repeated call_0"
case_run f4 '[[{"id":"call_0","name":"bash","args":{"command":"echo first"}}],[{"id":"call_0","name":"bash","args":{"command":"echo second"}}]]'
for p in $(jq -r 'select(.type=="tool_settled")|.out_path' ../out.ndjson | tr -d '\r'); do echo "$p => $(cat "$p")"; done

echo "== F5: mixed CRLF/LF edit"
mkdir -p "$ROOT/f5/work"; printf 'a\r\nb\nc\n' > "$ROOT/f5/work/mixed.txt"
case_run f5 '[[{"id":"e","name":"edit","args":{"path":"mixed.txt","old":"b\nc","new":"B\nC"}}]]'
jq -c 'select(.type=="tool_settled")|{outcome,result}' ../out.ndjson | tail -1; od -c mixed.txt | head -3

echo "== F6: 50 KB edit payload"
BIG=$(python -I -c "print('x'*50000)")
case_run f6 "[[{\"id\":\"e\",\"name\":\"edit\",\"args\":{\"path\":\"big.txt\",\"old\":\"\",\"new\":\"$BIG\"}}]]"
echo "exit=$CODE events.jsonl bytes=$(wc -c < "$SESS/events.jsonl") (50000-byte payload)"
jq -c 'select(.type=="assistant" or .type=="tool_started")|{type,len:(tostring|length),keys:keys}' "$SESS/events.jsonl"
ls -R "$SESS/commands" | head -10
echo "file written: $(wc -c < big.txt) bytes"
