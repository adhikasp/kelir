#!/usr/bin/env bash
# steer7d review live drive: real binary, real model (OpenRouter, cheap). The model starts a real bg job, runs
# the child-vantage attack script from a bash tool call, then replies. The log is then checked.
# usage (in a Herdr pane): set -a; . live.env; set +a; steer7d_live.sh <scratch>
set -u
D="$(cd "$(dirname "$0")" && pwd)"
REL="$D/../../target/release"; export PATH="$REL:$PATH"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
: "${OPENROUTER_API_KEY:?}"
S="$1"; rm -rf "$S"; mkdir -p "$S/work"; export KELIR_STATE_DIR="$S/state"; cd "$S/work" || exit 1
CHILD="$(cygpath -m "$D/steer7d_child.py")"; SCAN="$(cygpath -m "$D/steer7c_memscan.py")"
kelir run --keep --json --provider openrouter --model "$MODEL" \
  "Run this exact bash command: kelir bg run \"sleep 120\"   Then run this exact bash command: python -I $CHILD $SCAN   Then reply with the word done." \
  > "$S/run.out" 2> "$S/run.err" &
PID=$!
sid=""; for _ in $(seq 1 100); do sid=$(ls "$KELIR_STATE_DIR/sessions" 2>/dev/null | head -n1); [ -n "$sid" ] && break; sleep 0.2; done
L="$KELIR_STATE_DIR/sessions/$sid/events.jsonl"
for _ in $(seq 1 200); do [ "$(grep -c '"type":"turn_ended"' "$L" 2>/dev/null)" -ge 1 ] && break; sleep 0.4; done
echo "--- attack output (from the tool result)"
grep -o 'SUMMARY[^"]*' "$L" | head -2
grep -o '{\\"attack\\"[^}]*}' "$L" | head -12
echo "--- job events"; grep -E '"type":"(bg_started|bg_settled)"' "$L" | cut -c1-200
for id in $(ls "$KELIR_STATE_DIR/sessions/$sid/bg" 2>/dev/null | grep -o '^bg_[a-z0-9]*' | sort -u | head -3); do kelir ctl "$sid" cancel "$id" > /dev/null 2>&1; done
kelir ctl "$sid" end > /dev/null 2>&1; wait $PID 2>/dev/null
