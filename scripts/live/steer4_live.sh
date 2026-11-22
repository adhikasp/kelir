#!/usr/bin/env bash
# STEER-4 live drive: no step limit by default, an explicit cap pauses the turn.
# Usage: steer4_live.sh <scenario> <scratch-dir>   (OPENROUTER_API_KEY must be in the environment)
#   nolimit  a real 110-step turn (> the old default of 100) completes with no max_steps
#   cap      --max-steps 5 pauses (exit 0); a --keep session's `say continue` resumes with a fresh count
#   resume   a paused run is continued by `kelir run --resume` (fresh count, no call re-run)
#   stopgap  the old user-config line max_steps = 4294967295 still parses and a turn finishes
set -u
SCN="$1"; S="$2"
D="$(cd "$(dirname "$0")" && pwd)"
BIN="${KELIR_BIN:-$D/../../target/release/kelir.exe}"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
export KELIR_STATE_DIR="$S/state-$SCN"
W="$S/work-$SCN"
rm -rf "$W" "$KELIR_STATE_DIR"; mkdir -p "$W"; cd "$W" || exit 1
cfg() { # $1 = extra config line (may be empty)
  printf 'provider = "openrouter"\nmodel = "%s"\n%s\n' "$MODEL" "$1" > "$S/config-$SCN.toml"
  export KELIR_CONFIG="$S/config-$SCN.toml" KELIR_CONFIG_DIR="$S/no-config"
}
log() { ls "$KELIR_STATE_DIR"/sessions/*/events.jsonl | head -1; }
steps() { grep -c '"type":"step_started"' "$(log)"; }
outcomes() { grep '"type":"turn_ended"' "$(log)" | jq -r .outcome | tr '\n' ' '; }

TASK_N() { # $1 = how many lines
  echo "This is a step-count drill. Work in the current directory. Do these steps strictly one at a time, each in its own separate bash call: never combine, batch or parallelise calls, one tool call per message. For N from 1 to $1 run exactly: echo N >> counter.txt   (N replaced by the number). Do not check anything in between. When all $1 are done, run wc -l counter.txt and answer with the number."
}

case "$SCN" in
nolimit)
  cfg ""
  TASK="$(TASK_N 110)"
  "$BIN" run --json "$TASK" > "$S/out-$SCN.ndjson" 2> "$S/err-$SCN.txt"; code=$?
  echo "exit=$code steps=$(steps) outcomes=$(outcomes)"
  echo "counter lines: $(wc -l < counter.txt)"
  tail -n 1 "$S/out-$SCN.ndjson" | jq -c '{type,outcome,exit_code,steps}'
  cat "$S/err-$SCN.txt" | head -5
  ;;
cap)
  cfg ""
  TASK="$(TASK_N 14)"
  "$BIN" run --keep --json --max-steps 5 "$TASK" > "$S/out-$SCN.ndjson" 2> "$S/err-$SCN.txt" &
  PID=$!
  t=0; until grep -q '"outcome":"paused"' "$(log 2>/dev/null)" 2>/dev/null; do sleep 0.5; t=$((t+1)); [ $t -gt 240 ] && { echo TIMEOUT-1; kill $PID; exit 1; }; done
  echo "after turn 1: steps=$(steps) outcomes=$(outcomes)"
  "$BIN" ctl latest say "continue" | jq -c '{ok:.ok,state:{turn:.state.turn,step:.state.step,turn_steps:.state.turn_steps,max_steps:.state.max_steps}}'
  t=0; until [ "$(grep -c '"outcome":"paused"' "$(log)")" -ge 2 ]; do sleep 0.5; t=$((t+1)); [ $t -gt 240 ] && { echo TIMEOUT-2; kill $PID; exit 1; }; done
  echo "after turn 2: steps=$(steps) outcomes=$(outcomes)"
  echo "counter: $(wc -l < counter.txt) lines; steps per turn:"
  jq -rc 'select(.type=="step_started" or .type=="turn_ended") | [.type,.step,.outcome]|@tsv' "$(log)" | tr '\n' ';'; echo
  "$BIN" ctl latest end | jq -c .ok
  wait $PID; echo "keep-run exit=$?"
  head -5 "$S/err-$SCN.txt"
  ;;
resume)
  cfg "max_steps = 4"
  TASK="$(TASK_N 10)"
  "$BIN" run --json "$TASK" > "$S/out-$SCN.ndjson" 2> "$S/err-$SCN.txt"; echo "run1 exit=$? steps=$(steps) outcomes=$(outcomes)"
  tail -n 1 "$S/out-$SCN.ndjson" | jq -c '{type,outcome,exit_code,steps}'
  head -3 "$S/err-$SCN.txt"
  ID="$(basename "$(dirname "$(log)")")"
  "$BIN" resume --json "$ID" > "$S/out2-$SCN.ndjson" 2> "$S/err2-$SCN.txt"; echo "resume1 exit=$? steps=$(steps) outcomes=$(outcomes)"
  tail -n 1 "$S/out2-$SCN.ndjson" | jq -c '{type,outcome,exit_code,steps}'
  "$BIN" resume --json --max-steps 0 "$ID" > "$S/out3-$SCN.ndjson" 2> "$S/err3-$SCN.txt"; echo "resume2(--max-steps 0) exit=$? steps=$(steps) outcomes=$(outcomes)"
  tail -n 1 "$S/out3-$SCN.ndjson" | jq -c '{type,outcome,exit_code,steps}'
  echo "counter.txt: $(tr '\n' ' ' < counter.txt)"
  ;;
stopgap)
  cfg "max_steps = 4294967295"
  "$BIN" run --json "Run: echo hi >> a.txt . Then answer ok." > "$S/out-$SCN.ndjson" 2> "$S/err-$SCN.txt"; code=$?
  echo "exit=$code steps=$(steps) outcomes=$(outcomes)"
  tail -n 1 "$S/out-$SCN.ndjson" | jq -c '{type,outcome,exit_code,steps}'
  cat "$S/err-$SCN.txt" | head -3
  ;;
*) echo "unknown scenario"; exit 2 ;;
esac
