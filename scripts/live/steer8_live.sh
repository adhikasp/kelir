#!/usr/bin/env bash
# STEER-8 review live drive. Usage: steer8_live.sh <scratch-dir>   (OPENROUTER_API_KEY from the environment)
# One keep-alive session on a cheap model: status at zero steps, during a running tool, after the turn,
# warnings (warn_prompt_tokens=1500), then a resume. Raw outputs land in <scratch>.
set -u
S="$1"; D="$(cd "$(dirname "$0")" && pwd)"
BIN="${KELIR_BIN:-$D/../../target/release/kelir.exe}"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
export KELIR_STATE_DIR="$S/state"; W="$S/work"
rm -rf "$S/state" "$W"; mkdir -p "$W"; cd "$W" || exit 1
printf 'provider = "openrouter"\nmodel = "%s"\nwarn_prompt_tokens = 1500\n' "$MODEL" > "$S/config.toml"
export KELIR_CONFIG="$S/config.toml" KELIR_CONFIG_DIR="$S/no-config"
"$BIN" run --keep --json "Run exactly: echo one > a.txt . Then run: sleep 25 . Then run: cat a.txt and answer with its content." > "$S/out.ndjson" 2> "$S/err.txt" &
PID=$!
until ls "$KELIR_STATE_DIR"/sessions/*/events.jsonl >/dev/null 2>&1; do sleep 0.3; done
ID=$(basename "$(dirname "$(ls "$KELIR_STATE_DIR"/sessions/*/events.jsonl | head -1)")")
echo "session=$ID"
"$BIN" ctl "$ID" status --json > "$S/st0.json"; echo "st0 rc=$?"
# wait for the sleep to be running
for i in $(seq 1 120); do grep -q 'sleep 25' "$KELIR_STATE_DIR/sessions/$ID/events.jsonl" && break; sleep 0.5; done
sleep 3; "$BIN" ctl "$ID" status --json > "$S/st1.json"; "$BIN" ctl "$ID" status > "$S/st1.txt"
until grep -q '"type":"turn_ended"' "$KELIR_STATE_DIR/sessions/$ID/events.jsonl"; do sleep 1; done
"$BIN" ctl "$ID" status --json > "$S/st2.json"; "$BIN" ctl "$ID" status > "$S/st2.txt"
"$BIN" ctl "$ID" end >/dev/null; wait $PID
echo "ended"; cp "$KELIR_STATE_DIR/sessions/$ID/events.jsonl" "$S/events.jsonl"
# resume (zero new steps)
"$BIN" resume "$ID" --help >/dev/null 2>&1
"$BIN" run --keep --json --resume "$ID" > "$S/out2.ndjson" 2> "$S/err2.txt" &
P2=$!; sleep 4
"$BIN" ctl "$ID" status --json > "$S/st3.json"; "$BIN" ctl "$ID" status > "$S/st3.txt"
"$BIN" ctl "$ID" end >/dev/null; wait $P2
cp "$KELIR_STATE_DIR/sessions/$ID/events.jsonl" "$S/events2.jsonl"
echo finished
