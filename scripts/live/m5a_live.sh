#!/usr/bin/env bash
# M5-A live drive (reviewer): sources + skills against a real model, with a source edited mid-session.
# Usage: m5a_live.sh <scratch-dir>      (OPENROUTER_API_KEY must be in the environment)
# A pass-through logging proxy (scripts/live/logging_proxy.py) records request bodies (never headers)
# so the cache prefix across the mid-session edit can be compared byte for byte.
set -u
S="$1/m5a"
D="$(cd "$(dirname "$0")" && pwd)"
BIN="${KELIR_BIN:-$D/../../target/release/kelir.exe}"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
: "${OPENROUTER_API_KEY:?OPENROUTER_API_KEY must be set}"
rm -rf "$S"; mkdir -p "$S/proj/.claude/skills/sift" "$S/home" "$S/cfg"
# Isolate: never this machine's real ~/.claude/skills or config dir.
export KELIR_STATE_DIR="$S/state" KELIR_HOME="$S/home" KELIR_CONFIG_DIR="$S/cfg"
printf '# Project rules\n\nThe project codeword is PAPAYA-4417. Always state it when asked.\n' > "$S/proj/AGENTS.md"
printf -- '---\nname: sift\ndescription: use when a log is too long to read\n---\n\n# Sift\n\nThe sift code is SKILL-CODE-9031.\n' > "$S/proj/.claude/skills/sift/SKILL.md"
# PAD=1: pad AGENTS.md with ~8 KB so the prefix is long enough for the provider to cache it.
PADTXT=""
if [ "${PAD:-}" = "1" ]; then for i in $(seq 1 120); do PADTXT="$PADTXT
- Rule $i: keep steps small, verify what you changed, and put long output in a file."; done; fi
printf '# Project rules

The project codeword is PAPAYA-4417. Always state it when asked.
%s
' "$PADTXT" > "$S/proj/AGENTS.md"
DRV="$S/driver.log"; : > "$DRV"
say() { echo "[$(date +%T.%3N)] $*" | tee -a "$DRV"; }
PORT=$((18000 + RANDOM % 1000))
python "$D/logging_proxy.py" "$PORT" "$S/requests.jsonl" > /dev/null 2>&1 &
PROXY=$!
trap 'kill $PROXY 2>/dev/null; [ -n "${KP:-}" ] && kill $KP 2>/dev/null' EXIT
sleep 1
export KELIR_BASE_URL="http://127.0.0.1:$PORT/api/v1"
cd "$S/proj" || exit 1
TASK='First, from your instructions, state the project codeword. Second, run `sleep 10` in bash. Third, read the sift skill (its directory is named in your system prompt) with bash and report its code. Finish with the single word ready.'
"$BIN" run --keep --json --cwd "$S/proj" --model "$MODEL" "$TASK" > "$S/out.ndjson" 2> "$S/err.txt" &
KP=$!
LOG=""
for _ in $(seq 1 200); do
  LOG="$(ls "$S"/state/sessions/*/events.jsonl 2>/dev/null | head -n1)"; [ -n "$LOG" ] && break; sleep 0.05
done
[ -n "$LOG" ] || { say "no session log"; exit 3; }
count() { grep -c "\"type\":\"$1\"" "$LOG" 2>/dev/null || true; }
wait_count() { local t=0; while [ "$(count "$1")" -lt "$2" ]; do sleep 0.2; t=$((t+1)); [ "$t" -gt $(($3*5)) ] && return 1; done; return 0; }
say "session log $LOG"
# Wait until the model is inside `sleep 10` (a tool_started with sleep), then edit AGENTS.md.
t=0; until grep -q 'sleep 10' "$LOG" && [ "$(count tool_started)" -ge 1 ]; do sleep 0.2; t=$((t+1)); [ $t -gt 300 ] && { say "no sleep call seen"; break; }; done
say "editing AGENTS.md mid-session (tool_started=$(count tool_started))"
printf '# Project rules\n\nThe project codeword is MANGO-8802 (changed). Always state it when asked.\n' > "$S/proj/AGENTS.md"
wait_count turn_ended 1 180 || say "turn 1 did not end"
say "turn 1 ended; asking again"
"$BIN" ctl latest say 'Per the latest project instructions, what is the project codeword now? One line.' >> "$DRV" 2>&1
wait_count turn_ended 2 120 || say "turn 2 did not end"
"$BIN" ctl latest end >> "$DRV" 2>&1
wait $KP; say "kelir exit $?"
cp "$LOG" "$S/events.jsonl"
python - "$S" <<'PY' | tee -a "$DRV"
import json, sys
S = sys.argv[1]
ev = [json.loads(l) for l in open(S + "/events.jsonl", encoding="utf-8") if l.strip()]
st = [e for e in ev if e["type"] == "session_started"][0]
print("sources[]:", [(s["name"], s["path"].replace("\\", "/").split("/")[-3:], s["hash"]) for s in st["sources"]])
print("system bytes:", len(st["system"]["text"]), "contains PAPAYA:", "PAPAYA-4417" in st["system"]["text"],
      "| skill body in system:", "SKILL-CODE-9031" in st["system"]["text"])
print("source events:", [(e["kind"], e["name"], str(e["text"])[:60].replace("\n", " ")) for e in ev if e["type"] == "source"])
for e in ev:
    if e["type"] == "assistant":
        t = e["text"] if isinstance(e["text"], str) else e["text"].get("preview", "")
        if t.strip(): print("assistant:", t.strip()[:200].replace("\n", " "))
reqs = [json.loads(l)["body"] for l in open(S + "/requests.jsonl", encoding="utf-8") if l.strip()]
print("requests:", len(reqs))
for i in range(1, len(reqs)):
    a, b = reqs[i-1]["messages"], reqs[i]["messages"]
    same = json.dumps(b[:len(a)], sort_keys=False) == json.dumps(a, sort_keys=False)
    print(f"req {i} extends req {i-1} byte-for-byte: {same} ({len(a)} -> {len(b)} msgs); system same: {a[0]==b[0]}")
for e in ev:
    if e["type"] in ("step_finished", "usage") or "usage" in e:
        u = e.get("usage") or e
        print("usage:", {k: u.get(k) for k in ("prompt_tokens", "completion_tokens", "cached_tokens", "input", "cached") if k in u})
PY
