#!/usr/bin/env bash
# One "does it flash?" scenario. Run it from a Herdr pane (or a detached launcher) with winspy running;
# flash_run.sh does both. Keys come from the environment only (live mode sources an env file outside the repo).
#
# Usage: flash_scn.sh <bin-dir> <scenario> <canned|live> <scratch-root>
#   <bin-dir>   a directory holding the kelir.exe under test (it is put first on PATH, like the model's `kelir bg ...`)
#   scenario    calls   10 bash tool calls (2 turns x 5 parallel)
#               bg      `kelir bg run` x10 from a tool call, then a call promoted at 2 s (KELIR_PROMOTE_AFTER=2)
#               cancel  3 long jobs started by a call, then `kelir bg cancel` on each
#               kill    the session is killed (taskkill /F, no /T) while a call is running, then cleaned up by PID
#               mix     calls + bg + promotion in one session (used from a console-less parent)
#               tui     the interactive session (`kelir`), one prompt that makes a bash call, then Ctrl-C Ctrl-C
set -u
BD="$1"; SCN="$2"; MODE="$3"; ROOT="$4"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export PATH="$(cygpath -u "$BD"):$PATH"
BIN="$BD/kelir.exe"
S="$ROOT/$SCN-$(basename "$BD")-$MODE"
rm -rf "$S"; mkdir -p "$S/work"
export KELIR_STATE_DIR="$S/state"
cd "$S/work" || exit 1
say() { echo "[$(date +%T.%3N)] $*"; }
MODEL_ARGS=()
SERVER_PID=""
PORT=$((18200 + RANDOM % 700))

if [ "$MODE" = live ]; then
  : "${OPENROUTER_API_KEY:?OPENROUTER_API_KEY must be set (source the env file in the pane command)}"
  MODEL_ARGS=(--provider openrouter --model "${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}")
else
  export OPENROUTER_API_KEY=dummy
fi

# canned_start '<turns json>': a scripted OpenAI-compatible server, only for canned mode.
canned_start() {
  [ "$MODE" = canned ] || return 0
  printf '%s' "$1" > "$S/turns.json"
  python -I "$HERE/canned_server.py" "$PORT" "@$S/turns.json" > /dev/null 2>&1 &
  SERVER_PID=$!
  export KELIR_BASE_URL="http://127.0.0.1:$PORT/v1"
  for _ in $(seq 1 50); do (echo > "/dev/tcp/127.0.0.1/$PORT") 2>/dev/null && return 0; sleep 0.1; done
  say "canned server did not come up"; return 1
}
cleanup() { [ -n "$SERVER_PID" ] && taskkill //F //T //PID "$(cat "/proc/$SERVER_PID/winpid" 2>/dev/null || echo "$SERVER_PID")" > /dev/null 2>&1; }
trap cleanup EXIT
winpid() { cat "/proc/$1/winpid" 2>/dev/null || echo "$1"; }
LOG=""
find_log() { for _ in $(seq 1 100); do LOG="$(ls "$S"/state/sessions/*/events.jsonl 2>/dev/null | head -n1)"; [ -n "$LOG" ] && return 0; sleep 0.1; done; return 1; }
wait_for() { local t=0; while [ "$(grep -c "$1" "$LOG" 2>/dev/null || true)" -lt "$2" ]; do sleep 0.1; t=$((t+1)); [ "$t" -gt $(($3*10)) ] && return 1; done; return 0; }

turn5() { # turn5 <prefix> -> one JSON turn of 5 parallel bash calls
  local p="$1" i out=""
  for i in 1 2 3 4 5; do out="$out{\"id\":\"$p$i\",\"name\":\"bash\",\"args\":{\"command\":\"echo $p$i; sleep 0.2\"}},"; done
  printf '[%s]' "${out%,}"
}
BGLOOP='for i in 1 2 3 4 5 6 7 8 9 10; do kelir bg run "sleep 3; echo job$i" ; done'

case "$SCN" in
calls)
  canned_start "[$(turn5 a),$(turn5 b)]" || exit 2
  TASK="Make exactly 10 separate bash tool calls, one per command, in this order: echo c1, echo c2, ... echo c10. Then reply done."
  say "run: 10 bash tool calls"
  "$BIN" run --json "${MODEL_ARGS[@]}" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt"; say "exit=$?"
  say "tool_settled events: $(grep -c '"type":"tool_settled"' "$S"/state/sessions/*/events.jsonl)"
  ;;
bg)
  canned_start "[[{\"id\":\"g1\",\"name\":\"bash\",\"args\":{\"command\":\"${BGLOOP//\"/\\\"}\"}}],[{\"id\":\"g2\",\"name\":\"bash\",\"args\":{\"command\":\"sleep 6; echo promoted-done\"}}]]" || exit 2
  TASK="First run this exact bash command: $BGLOOP   Then, as a second separate call, run: sleep 6; echo promoted-done   Then reply done."
  export KELIR_PROMOTE_AFTER=2
  say "run: bg x10 + a call promoted after ${KELIR_PROMOTE_AFTER}s"
  "$BIN" run --json "${MODEL_ARGS[@]}" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt"; say "exit=$?"
  say "promoted: $(grep -c '"outcome":"promoted"' "$S"/state/sessions/*/events.jsonl)  bg_started: $(grep -c '"type":"bg_started"' "$S"/state/sessions/*/events.jsonl)"
  say "waiting for the jobs to settle"; sleep 9
  "$BIN" bg ls > "$S/bgls.txt" 2>&1; head -20 "$S/bgls.txt"
  ;;
cancel)
  canned_start "[[{\"id\":\"x1\",\"name\":\"bash\",\"args\":{\"command\":\"kelir bg run \\\"sleep 120\\\"; kelir bg run \\\"sleep 121\\\"; kelir bg run \\\"bash -c 'sleep 122 & sleep 123 & wait'\\\"\"}}]]" || exit 2
  TASK="Run exactly this one bash command, then reply started: kelir bg run \"sleep 120\"; kelir bg run \"sleep 121\"; kelir bg run \"bash -c 'sleep 122 & sleep 123 & wait'\""
  "$BIN" run --json "${MODEL_ARGS[@]}" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt"; say "exit=$?"
  # `bg ls`/`bg cancel` from outside a call find jobs through KELIR_SESSION_DIR (a bash call has it set)
  SD="$(dirname "$(ls -d "$S"/state/sessions/*/events.jsonl | head -n1)")"; export KELIR_SESSION_DIR="$SD"
  "$BIN" bg ls > "$S/bgls.txt" 2>&1; cat "$S/bgls.txt"
  for id in $(grep -o 'bg_[A-Za-z0-9_]*' "$S/bgls.txt" | sort -u); do say "cancel $id"; "$BIN" bg cancel "$id"; done
  sleep 3; "$BIN" bg ls; unset KELIR_SESSION_DIR
  ;;
kill)
  canned_start "[[{\"id\":\"k1\",\"name\":\"bash\",\"args\":{\"command\":\"echo begin; sleep 61; sleep 62 & sleep 63 & wait\"}}]]" || exit 2
  TASK="Run exactly this one bash command and nothing else: echo begin; sleep 61; sleep 62 & sleep 63 & wait"
  "$BIN" run --json "${MODEL_ARGS[@]}" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt" &
  KP=$!
  find_log || { say "no log"; exit 3; }
  wait_for '"type":"tool_started"' 1 30 || say "tool never started"
  sleep 2
  say "killing the session mid-call (taskkill /F /PID, no /T)"
  taskkill //F //PID "$(winpid $KP)" > /dev/null 2>&1; wait $KP 2>/dev/null
  sleep 3
  LEFT="$(powershell.exe -NoProfile -Command "Get-CimInstance Win32_Process | Where-Object { \$_.CommandLine -match 'sleep(\.exe)?\"? 6[123]' } | ForEach-Object { \$_.ProcessId }" | tr -d '\r')"
  say "leftover sleep 61/62/63 pids after the kill: [$(echo $LEFT)]"
  for p in $LEFT; do taskkill //F //T //PID "$p" > /dev/null 2>&1; done
  ;;
mix)
  canned_start "[$(turn5 m),[{\"id\":\"m9\",\"name\":\"bash\",\"args\":{\"command\":\"for i in 1 2 3; do kelir bg run \\\"sleep 3; echo job\$i\\\" ; done\"}}],[{\"id\":\"m10\",\"name\":\"bash\",\"args\":{\"command\":\"sleep 5; echo promoted-done\"}}]]" || exit 2
  TASK="Run echo m1..m5 as five bash calls; then run: for i in 1 2 3; do kelir bg run \"sleep 3; echo job\$i\" ; done ; then run: sleep 5; echo promoted-done. Then reply done."
  export KELIR_PROMOTE_AFTER=2
  "$BIN" run --json "${MODEL_ARGS[@]}" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt"; say "exit=$?"
  sleep 6
  ;;
direct)
  # kelir.exe ITSELF has no console (DETACHED_PROCESS, then a hidden console): 5 parallel calls, 3 `kelir bg run`, one promoted call.
  canned_start "[$(turn5 d),[{\"id\":\"d9\",\"name\":\"bash\",\"args\":{\"command\":\"for i in 1 2 3; do kelir bg run \\\"sleep 3; echo job\$i\\\" ; done\"}}],[{\"id\":\"d10\",\"name\":\"bash\",\"args\":{\"command\":\"sleep 5; echo promoted-done\"}}]]" || exit 2
  TASK="Run echo d1..d5 as five bash calls; then run three kelir bg run jobs; then run: sleep 5; echo promoted-done. Then reply done."
  export KELIR_PROMOTE_AFTER=2
  for how in detached hidden; do
    say "kelir.exe launched $how"
    rm -rf "$S/state"
    python -I "$HERE/launch_detached.py" $how 90 "$S/out-$how.ndjson" -- "$(cygpath -w "$BIN")" run --json "${MODEL_ARGS[@]}" "$TASK"
    sleep 6
  done
  ;;
tui)
  canned_start "[[{\"id\":\"t1\",\"name\":\"bash\",\"args\":{\"command\":\"echo tui-bash-call; sleep 1\"}}]]" || exit 2
  # The pane this runs in is the TUI's terminal; flash_run.sh types the prompt with `hc send`, then Ctrl-C Ctrl-C.
  say "starting the TUI"
  "$BIN" "${MODEL_ARGS[@]}"; say "tui exit=$?"
  ;;
*) say "unknown scenario $SCN"; exit 64 ;;
esac
say "done $SCN"
