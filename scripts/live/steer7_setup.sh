#!/usr/bin/env bash
# STEER-7 review live drive, part 1: two real sessions (B = victim, A = attacked) on one state dir,
# each with a real bg job; A's model runs the attacker from inside a bash tool call (a "test child").
# Usage: steer7_setup.sh <scratch>   (OPENROUTER_API_KEY from the environment; never printed)
set -u
D="$(cd "$(dirname "$0")" && pwd)"
REL="$D/../../target/release"; export PATH="$REL:$PATH"
BIN="$REL/kelir.exe"; MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
: "${OPENROUTER_API_KEY:?}"
S="$1/${2:-steer7}"; rm -rf "$S"; mkdir -p "$S/work"
export KELIR_STATE_DIR="$S/state"; cd "$S/work" || exit 1
STATEW="$(cygpath -w "$KELIR_STATE_DIR")"
ATK="$(cygpath -w "$D/steer7_attack.py")"
say(){ echo "[$(date +%T.%3N)] $*" | tee -a "$S/driver.log"; }
# --- victim B
"$BIN" run --keep --json --provider openrouter --model "$MODEL" 'Run this exact bash command: kelir bg run "sleep 601"   Then run: sleep 2   Then reply "ready".' > "$S/B.out" 2> "$S/B.err" &
echo $! > "$S/B.pid"
for _ in $(seq 1 300); do B="$(ls "$KELIR_STATE_DIR/sessions" 2>/dev/null | head -n1)"; [ -n "$B" ] && break; sleep 0.1; done
say "B=$B"
for _ in $(seq 1 600); do grep -q '"type":"turn_ended"' "$KELIR_STATE_DIR/sessions/$B/events.jsonl" 2>/dev/null && break; sleep 0.2; done
say "B turn ended; B bg: $(ls "$KELIR_STATE_DIR/sessions/$B/bg" 2>/dev/null | tr '\n' ' ')"
# --- attacked A; its model runs the child-vantage attack from a bash tool call
cat > "$S/child_wrap.sh" <<WRAP
#!/usr/bin/env bash
A="\${KELIR_SOCK##*kelir-}"
python -I "$ATK" child "$STATEW" "\$A" "$B" > "$S/child.out" 2>&1
echo done
WRAP
"$BIN" run --keep --json --provider openrouter --model "$MODEL" "${A_TASK:-Run this exact bash command: kelir bg run \"sleep 602\"   Then run this exact bash command: bash $(cygpath -m "$S")/child_wrap.sh   Then reply \"ready\".}" > "$S/A.out" 2> "$S/A.err" &
echo $! > "$S/A.pid"
for _ in $(seq 1 300); do A="$(ls "$KELIR_STATE_DIR/sessions" | grep -v "^$B\$" | head -n1)"; [ -n "$A" ] && break; sleep 0.1; done
say "A=$A"
if [ -z "${CLEAN:-}" ]; then for _ in $(seq 1 900); do [ -s "$S/child.out" ] && grep -q SUMMARY "$S/child.out" && break; sleep 0.2; done; else for _ in $(seq 1 600); do [ "$(grep -c '"type":"turn_ended"' "$KELIR_STATE_DIR/sessions/$A/events.jsonl" 2>/dev/null)" -ge 1 ] && break; sleep 0.2; done; fi
echo "$A" > "$S/A.id"; echo "$B" > "$S/B.id"
say "child attack done; keeping sessions alive (A pid $(cat $S/A.pid), B pid $(cat $S/B.pid))"
wait
