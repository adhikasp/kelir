#!/usr/bin/env bash
# M3 live drive: compact a real session with a focus, then ask a question that needs the
# pinned first task and the human messages; read cached_tokens afterwards.
# Usage: m3_compact.sh <scratch-dir> [keep]     (OPENROUTER_API_KEY must be in the environment)
#   keep = the compact_keep to run with (default: the product default, 4)
#
# A pass-through logging proxy (scripts/live/logging_proxy.py) records the request bodies
# (never headers) so the order of the messages after the compaction can be read back.
set -u
S="$1"
KEEP="${2:-}"
D="$(cd "$(dirname "$0")" && pwd)"
export KELIR_STATE_DIR="$S/state"
BIN="${KELIR_BIN:-$D/../../target/release/kelir.exe}"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
rm -rf "$S/state" "$S/work" "$S/requests.jsonl" "$S/cfg"
mkdir -p "$S/work" "$S/cfg"
# $3 = extra config lines (e.g. "window = 1250" to make the auto trigger fire early)
: > "$S/cfg/config.toml"
if [ -n "$KEEP" ]; then printf 'compact_keep = %s\n' "$KEEP" >> "$S/cfg/config.toml"; fi
if [ -n "${3:-}" ]; then printf '%s\n' "$3" >> "$S/cfg/config.toml"; fi
DRV="$S/driver.log"
: > "$DRV"
say() { echo "[$(date +%T.%3N)] $*" | tee -a "$DRV"; }

PORT=$((18000 + RANDOM % 1000))
python "$D/logging_proxy.py" "$PORT" "$S/requests.jsonl" > /dev/null 2>&1 &
PROXY=$!
sleep 1
export KELIR_BASE_URL="http://127.0.0.1:$PORT/api/v1"
CFGARG=()
[ -s "$S/cfg/config.toml" ] && CFGARG=(--config "$S/cfg/config.toml")

cd "$S/work" || exit 1
TASK='Your task code word is KUMQUAT-72. Step 1: create three files a.txt, b.txt and c.txt, each containing its own file name, using one bash call per file (three calls). Step 2: reply with the single word ready and wait for further instructions from me.'
"$BIN" run --keep --json "${CFGARG[@]}" --model "$MODEL" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt" &
PID=$!
LOG=""
for _ in $(seq 1 200); do
  LOG="$(ls "$S"/state/sessions/*/events.jsonl 2> /dev/null | head -n1)"
  [ -n "$LOG" ] && break
  sleep 0.05
done
[ -n "$LOG" ] || { say "no session log"; exit 3; }
SID="$(basename "$(dirname "$LOG")")"
say "session $SID keep=${KEEP:-default}"
count() { grep -c "\"type\":\"$1\"" "$LOG" 2> /dev/null || true; }
wait_count() { # type n timeout
  local t=0
  while [ "$(count "$1")" -lt "$2" ]; do
    kill -0 "$PID" 2> /dev/null || { say "process gone waiting for $1 >= $2"; return 1; }
    sleep 0.2; t=$((t + 1)); [ "$t" -gt $(($3 * 5)) ] && { say "timeout waiting for $1 >= $2"; return 2; }
  done
}
ctl() { "$BIN" ctl "$SID" "$@" 2>&1 | tr -d '\r' | tee -a "$DRV"; }

wait_count turn_ended 1 120
say "turn 1 done"
ctl say "Human note one: whenever you give a final answer, include the number 9081."
wait_count turn_ended 2 120
ctl say "Human note two: the build target is called ZEBRA."
wait_count turn_ended 3 120
say "before compaction: $(count assistant) assistant, $(count usage) usage events"
ctl compact "keep the file names and the code word"
sleep 1
ctl say "Question: in one line, what was my original task's code word, the number from my first note, and the build target from my second note?"
wait_count turn_ended 4 180
say "compacted events: $(count compacted)"
ctl say "Now list the three files you created, in one line."
wait_count turn_ended 5 120
ctl end
wait "$PID"
say "kelir exit $?"
kill "$PROXY" 2> /dev/null
say "done"
echo "m3_compact finished"
