#!/usr/bin/env bash
# steer7b review live drive: real binary, real model (OpenRouter, cheap), one session; the model runs
# `echo first`, then the forging script. Then the log is checked for the forged job events.
# usage (in a Herdr pane): set -a; . live.env; set +a; steer7b_live.sh <scratch>
set -u
D="$(cd "$(dirname "$0")" && pwd)"
REL="$D/../../target/release"; export PATH="$REL:$PATH"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
: "${OPENROUTER_API_KEY:?}"
S="$1"; rm -rf "$S"; mkdir -p "$S/work"; export KELIR_STATE_DIR="$S/state"; cd "$S/work" || exit 1
echo "TOP-SECRET-OUTSIDE-THE-SESSION" > "$S/secret.txt"
export FORGE_OUT="$(cygpath -m "$S")/secret.txt"
FORGE="$(cygpath -m "$D/steer7b_forge.sh")"
kelir run --keep --json --provider openrouter --model "$MODEL" \
  "Run this exact bash command: echo first   Then run this exact bash command: bash $FORGE   Then wait for any notice and reply with the word done." \
  > "$S/run.out" 2> "$S/run.err" &
PID=$!
sid=""; for _ in $(seq 1 100); do sid=$(ls "$KELIR_STATE_DIR/sessions" 2>/dev/null | head -n1); [ -n "$sid" ] && break; sleep 0.2; done
L="$KELIR_STATE_DIR/sessions/$sid/events.jsonl"
for _ in $(seq 1 150); do grep -q '"bg_settled"' "$L" 2>/dev/null && break; sleep 0.4; done
sleep 3
echo "--- events of interest"; grep -E '"type":"(bg_started|bg_settled)"|"from":"job"' "$L" | cut -c1-400
echo "--- verdict"
if grep -q '"type":"bg_settled"' "$L"; then echo "EXPLOITED: forged status file reached the log as bg_settled"; else echo "refused"; fi
grep -q 'TOP-SECRET-OUTSIDE' "$L" && echo "EXPLOITED: arbitrary file tail reached the log"
kelir ctl "$sid" end > /dev/null 2>&1; wait $PID 2>/dev/null
