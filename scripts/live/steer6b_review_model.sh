#!/usr/bin/env bash
# STEER-6 round-2 live drive against OpenRouter through a logging proxy.
# Usage: steer6b_review_model.sh <scenario> <scratch-dir>
#   s2   clean state dir, /models held 3 s by the proxy: is parallel_tool_calls sent on the very first request?
#   s3   proxy answers the first request carrying reasoning_effort with HTTP 400: one retry without it, remembered
#   reg  a tool session: stdin-reading command, backslash command, edit multi-hunk, a slow command (promotion)
# OPENROUTER_API_KEY must be in the environment (the pane sources the env file).
set -u
SCN="$1"; S="$2"
D="$(cd "$(dirname "$0")" && pwd)"
BIN="${KELIR_BIN:-$D/../../target/release/kelir.exe}"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
rm -rf "$S/state" "$S/work" "$S/requests.jsonl" "$S/cfg" "$S/out.ndjson" "$S/err.txt"
mkdir -p "$S/work" "$S/cfg"
export KELIR_STATE_DIR="$S/state"
: > "$S/cfg/config.toml"
PORT=$((20000 + RANDOM % 1000))
case "$SCN" in
  s2) export DELAY_MODELS=3 ;;
  s3) export REJECT_FIELD=reasoning_effort ;;
esac
python "$D/steer6b_review_proxy.py" "$PORT" "$S/requests.jsonl" > /dev/null 2>&1 &
PROXY=$!
trap 'kill $PROXY 2>/dev/null' EXIT
sleep 1
export KELIR_BASE_URL="http://127.0.0.1:$PORT/api/v1"
cd "$S/work" || exit 1
case "$SCN" in
  s2) for i in 1 2 3 4; do printf 'file %s first line\nsecond\n' $i > f$i.txt; done
      TASK='Read the first line of each of f1.txt f2.txt f3.txt f4.txt (they are independent) and report all four first lines.' ;;
  s3) TASK='Run: echo hello-s3 . Then say done.' ;;
  reg) printf 'def alpha():\n    return 1\n\ndef beta():\n    return 2\n\nVALUE = "old"\n' > m.py
      TASK='Do these in order, each with the named tool. 1) bash: run exactly this command: printf "a\\\\b\n" > bs.txt && cat bs.txt   2) bash: run: head -c 5 | wc -c   (it reads stdin; report what it prints). 3) edit tool on m.py, ONE call with two edits: rename alpha to first, rename beta to second. 4) bash: python m.py && python -c "import m; print(m.first(), m.second())". 5) bash: run exactly: sleep 45; echo slept   and report. Then finish.' ;;
esac
START=$(date +%s)
"$BIN" run --json --config "$S/cfg/config.toml" --model "$MODEL" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt"
echo "exit=$? seconds=$(( $(date +%s) - START ))"
tail -c 500 "$S/err.txt"
