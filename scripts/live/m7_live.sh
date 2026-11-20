#!/usr/bin/env bash
# M7 live drive (real model through OpenRouter, cheap model). Run in a Herdr pane with the env
# file sourced INSIDE the pane command (never printed):
#   hc run --bg --name live-m7-x --cwd <scratch> "<git-bash>" -c 'set -a; . <envfile>; set +a; bash m7_live.sh <scn> <scratch>'
# scn: two-bg | depth3 | orphan | brief
set -u
D="$(cd "$(dirname "$0")" && pwd)"
export PATH="$D/../../target/release:$PATH"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
: "${OPENROUTER_API_KEY:?}"
SCN="$1"; S="$2"
rm -rf "$S"; mkdir -p "$S/work"; export KELIR_STATE_DIR="$S/state" KELIR_HOME="$S/home"; mkdir -p "$KELIR_HOME"
cd "$S/work" || exit 1
report() { echo "---- kelir ls"; kelir ls --all; echo "---- report"; python "$D/m7_live_report.py" "$KELIR_STATE_DIR"; }
wait_for() { local n=$1; shift; for _ in $(seq 1 "$n"); do "$@" && return 0; sleep 1; done; return 1; }
case "$SCN" in
two-bg)
  kelir run --keep --json --provider openrouter --model "$MODEL" \
   "Do exactly this. Step 1: in ONE bash call run these two commands in the background of your own session: kelir run --child --background 'Reply with only the number 17 doubled.' ; kelir run --child --background 'Reply with only the capital of France in one word.' . Step 2: do nothing else until two messages starting with [child ...] arrive. Step 3: reply with one line: both answers." > root.out 2> root.err &
  PID=$!
  have() { [ "$(grep -c '"from":"child"' "$KELIR_STATE_DIR"/sessions/*/events.jsonl 2>/dev/null | awk -F: '{s+=$2} END{print s+0}')" -ge 2 ]; }
  wait_for 240 have; echo "wakes seen: $?"
  sleep 20
  kelir ctl latest end > /dev/null 2>&1; wait $PID 2>/dev/null
  report ;;
depth3)
  printf '%s' 'Reply with only the single word BOTTOM.' > t3.txt
  printf '%s' 'Run exactly this bash command and reply with only its output: kelir run --child < t3.txt' > t2.txt
  printf '%s' 'Run exactly this bash command and reply with only its output: kelir run --child < t2.txt' > t1.txt
  kelir run --json --provider openrouter --model "$MODEL" \
   "Run exactly this bash command and reply with only its output: kelir run --child < t1.txt" > root.out 2> root.err
  echo "root exit=$?"; report ;;
orphan)
  kelir run --keep --json --provider openrouter --model "$MODEL" \
   "In ONE bash call run: kelir run --child --background 'Run the bash command: sleep 7311 . Then reply done.' ; kelir run --child --background 'Run the bash command: sleep 7312 . Then reply done.' . Then run the bash command: sleep 7313" > root.out 2> root.err &
  PID=$!
  up() { [ "$(kelir ctl latest status --json 2>/dev/null | python -c 'import json,sys; print(json.load(sys.stdin)["state"]["children"])' 2>/dev/null)" = "2" ]; }
  wait_for 120 up; echo "two children live: $?"
  sleep 12
  echo "---- kelir ls (live)"; kelir ls --all
  echo "---- root pid = $PID"; echo "$PID" > "$S/root.pid"
  ;;
brief)
  mkdir -p docs; cp "$D"/../../docs/reference/{children,cli,config,events,verbs}.md docs/ 2>/dev/null
  python "$D/logging_proxy.py" 18911 "$S/proxy.jsonl" > /dev/null 2>&1 &
  PROXY=$!
  export KELIR_BASE_URL=http://127.0.0.1:18911/api/v1
  kelir run --json --provider openrouter --model "$MODEL" \
   "Read these files, one bash call each, with cat: docs/children.md, docs/cli.md, docs/config.md, docs/events.md, docs/verbs.md . After the fifth, run exactly this bash command: kelir brief 'In docs/config.md, which keys cap the number of children? Answer in one line.' . Then reply with the brief's answer verbatim." > root.out 2> root.err
  echo "root exit=$?"; kill $PROXY 2>/dev/null
  report
  python "$D/m7_live_brief.py" "$KELIR_STATE_DIR" "$S/proxy.jsonl" ;;
esac
