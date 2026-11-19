#!/usr/bin/env bash
# STEER-6 reviewer live drive: one scenario per call, inside a Herdr pane.
# Usage: steer6_review_live.sh <scenario> <scratch-dir> [extra-config-line]
#   scenario: heredoc | edit | batch | compact
# OPENROUTER_API_KEY must be in the environment (the pane sources the env file).
# A logging proxy records request bodies (never headers) next to the session log.
set -u
SCN="$1"; S="$2"; EXTRA="${3:-}"
D="$(cd "$(dirname "$0")" && pwd)"
BIN="${KELIR_BIN:-$D/../../target/release/kelir.exe}"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
rm -rf "$S/state" "$S/work" "$S/requests.jsonl" "$S/cfg" "$S/out.ndjson" "$S/err.txt"
mkdir -p "$S/work" "$S/cfg"
export KELIR_STATE_DIR="$S/state"
# A warm model list (WARM_MODELS=<file>) so the 1.5 s model-list fetch cannot time out through the proxy.
if [ -n "${WARM_MODELS:-}" ]; then mkdir -p "$S/state"; cp "$WARM_MODELS" "$S/state/models-openrouter.json"; fi
printf '%s\n' "$EXTRA" > "$S/cfg/config.toml"
PORT=$((19000 + RANDOM % 1000))
python "$D/steer6_review_proxy.py" "$PORT" "$S/requests.jsonl" > /dev/null 2>&1 &
PROXY=$!
trap 'kill $PROXY 2>/dev/null' EXIT
sleep 1
export KELIR_BASE_URL="http://127.0.0.1:$PORT/api/v1"
cd "$S/work" || exit 1
case "$SCN" in
  heredoc)
    TASK='Use exactly ONE bash tool call, with a single heredoc (cat > gen.txt <<'"'"'EOF'"'"'), to create gen.txt with 900 lines, line N being: "line N of the generated file, padded with filler text to be long enough". Write all 900 lines out literally in that one command, no loops, no python. Then in a second bash call run: wc -l gen.txt. Report the number.' ;;
  edit)
    printf 'def alpha():\n    return 1\n\ndef beta():\n    return 2\n\ndef gamma():\n    return 3\n\nVALUE = "old"\n' > m.py
    TASK='In m.py: rename alpha to first, beta to second, gamma to third, and change VALUE to "new". Do it with the edit tool, in as few calls as possible. Then run: python m.py && cat m.py' ;;
  batch)
    for i in 1 2 3 4; do printf 'file %s first line\nsecond\n' $i > f$i.txt; done
    TASK='Read the first line of each of f1.txt f2.txt f3.txt f4.txt (they are independent) and report all four first lines.' ;;
  compact)
    for i in 1 2 3 4 5 6; do seq 1 200 | sed "s/^/file $i row /" > g$i.txt; done
    TASK='For each of g1.txt through g6.txt, one file at a time with its own bash call, print the file with cat and then tell me the row count in one short line. After the last, summarise all six counts.' ;;
  *) echo "unknown scenario $SCN"; exit 2 ;;
esac
CFGARG=(--config "$S/cfg/config.toml")
START=$(date +%s)
"$BIN" run --json "${CFGARG[@]}" --model "$MODEL" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt"
echo "exit=$? seconds=$(( $(date +%s) - START ))"
tail -c 600 "$S/err.txt"
