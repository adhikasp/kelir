#!/usr/bin/env bash
# M3 live drive: kill -9 a real run at five points of a ~10-step task, resume each time, finish.
# Usage: m3_kill.sh <scratch-dir>      (OPENROUTER_API_KEY must already be in the environment)
#
# The task makes every step append one line to counter.txt, so a command that kelir
# re-ran on resume shows up as a duplicate line. Kill points are chosen from the event
# log: mid-tool, right after a settle, mid-stream, between `assistant` and `tool_started`,
# and a random moment. Where each kill really landed is printed from the log tail.
set -u
S="$1"
D="$(cd "$(dirname "$0")" && pwd)"
export KELIR_STATE_DIR="$S/state"
BIN="${KELIR_BIN:-$D/../../target/release/kelir.exe}"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
W="$S/work"
rm -rf "$W" "$S/state"
mkdir -p "$W"
cd "$W" || exit 1
DRV="$S/driver.log"
: > "$DRV"
say() { echo "[$(date +%T.%3N)] $*" | tee -a "$DRV"; }

TASK='This is a durability drill. Work in the current directory. Do these steps strictly one at a time, each in its own separate bash call (never combine or batch calls). For N from 1 to 14 run exactly: echo "step N" >> counter.txt && sleep 3   (with N replaced by the number). If a call result ever says its outcome is unknown, first run cat counter.txt to see whether that step already ran, and run the step only if its line is missing. When all 14 lines are present, run wc -l counter.txt and answer with the number.'

LOG=""
count() { grep -c "\"type\":\"$1\"" "$LOG" 2>/dev/null || true; }
landing() { tail -n 3 "$LOG" | jq -c '{seq,type,call_id,step}' | tr '\n' ' '; }
killpid() { # $1 = msys pid
  local w
  w="$(cat "/proc/$1/winpid" 2>/dev/null || echo "$1")"
  taskkill //F //PID "$w" > /dev/null 2>&1
  wait "$1" 2> /dev/null
}
wait_until() { # $1 = bash condition, $2 = pid, $3 = timeout seconds
  local t=0
  while ! eval "$1"; do
    kill -0 "$2" 2> /dev/null || return 1
    sleep 0.02
    t=$((t + 1))
    [ "$t" -gt $(($3 * 50)) ] && return 2
  done
  return 0
}

say "start: $BIN model=$MODEL"
"$BIN" run --json --model "$MODEL" "$TASK" > "$S/out0.ndjson" 2> "$S/err0.txt" &
PID=$!
# Wait for the log to appear.
for _ in $(seq 1 200); do
  LOG="$(ls "$S"/state/sessions/*/events.jsonl 2> /dev/null | head -n1)"
  [ -n "$LOG" ] && break
  sleep 0.05
done
[ -n "$LOG" ] || { say "no session log appeared"; exit 3; }
SID="$(basename "$(dirname "$LOG")")"
say "session $SID"

# Each kill point is a condition over the log (counts are cumulative, so they are
# taken relative to the baseline at the start of that process).
kill_at() { # $1 = label, $2 = condition (uses $B_ss $B_as $B_st $B_se), $3 = extra delay
  local label="$1" cond="$2" delay="${3:-0}"
  B_ss=$(count step_started); B_as=$(count assistant); B_st=$(count tool_started); B_se=$(count tool_settled)
  if wait_until "$cond" "$PID" 90; then
    [ "$delay" != "0" ] && sleep "$delay"
    killpid "$PID"
    say "KILLED $label  | landed after: $(landing)"
    say "  counter.txt now has $(wc -l < counter.txt 2> /dev/null || echo 0) line(s); ended=$(count session_ended)"
  else
    say "$label: process ended or timed out before the condition ($?); not killed"
    return 1
  fi
}

resume() { # n
  say "RESUME #$1"
  "$BIN" resume --json "$SID" > "$S/out$1.ndjson" 2> "$S/err$1.txt" &
  PID=$!
  sleep 0.3
}

# K1: mid-tool, first call running (started, not settled).
kill_at "K1 mid-tool" '[ $(count tool_started) -gt $(count tool_settled) ] && [ $(count tool_started) -ge 2 ]' 0.3
# K2: right after a settle (two more settles in the resumed process).
resume 1
kill_at "K2 after-settle" '[ $(count tool_settled) -ge $((B_se + 2)) ]' 0
# K3: mid-stream: a step started with no assistant yet.
resume 2
kill_at "K3 mid-stream" '[ $(count step_started) -gt $(count assistant) ] && [ $(count tool_settled) -ge $((B_se + 1)) ]' 0.4
# K4: between the assistant message and tool_started. A tight poll cannot hit a window
# of microseconds, so this resume runs with the documented test hook KELIR_TEST_CRASH_AT=usage
# (abort right after `usage` is fsync'd, before any tool call starts). Still the real
# binary and the real model.
say "RESUME #3 with KELIR_TEST_CRASH_AT=usage"
KELIR_TEST_CRASH_AT=usage "$BIN" resume --json "$SID" > "$S/out3.ndjson" 2> "$S/err3.txt" &
PID=$!
wait "$PID"
say "KILLED K4 (hook, exit $?) | landed after: $(landing)"
say "  counter.txt now has $(wc -l < counter.txt 2> /dev/null || echo 0) line(s); ended=$(count session_ended)"
# K5: a random moment.
resume 4
R=$((RANDOM % 25))
say "K5 random delay 2.$R s"
sleep "2.$R"
if kill -0 "$PID" 2> /dev/null; then
  killpid "$PID"
  say "KILLED K5 random | landed after: $(landing)"
  say "  counter.txt now has $(wc -l < counter.txt 2> /dev/null || echo 0) line(s); ended=$(count session_ended)"
else
  say "K5: already finished"
fi

# Finish.
resume 5
wait "$PID"
say "final resume exit: $?"
say "---- counter.txt ----"
cat counter.txt | tee -a "$DRV"
say "---- summary ----"
say "resumed events: $(count resumed)  unknown settles: $(grep -c '"outcome":"unknown"' "$LOG")  session_ended: $(count session_ended)  turn_ended: $(count turn_ended)"
say "kelir exit lines: done"
echo "m3_kill finished"
