#!/usr/bin/env bash
# STEER-6 reviewer: does OpenRouter honour the flat `reasoning_effort` field on the cheap model?
# Usage: steer6_review_effort.sh <scratch-dir> [runs-per-arm]   (OPENROUTER_API_KEY in the environment)
# Prints reasoning_tokens and completion_tokens per request; never prints the key.
set -u
S="$1"; N="${2:-3}"; mkdir -p "$S"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
PROMPT='Design a small dependency-resolution algorithm for a package manager that must detect cycles, pick the newest compatible versions and report conflicts. Give only the final pseudocode, under 25 lines.'
: "${OPENROUTER_API_KEY:?}"
probe() { # label extra-json
  local body
  body=$(python -c "
import json,sys
b={'model':'$MODEL','messages':[{'role':'user','content':sys.argv[1]}],'max_tokens':6000}
b.update(json.loads(sys.argv[2]))
print(json.dumps(b))" "$PROMPT" "$2")
  curl -sS https://openrouter.ai/api/v1/chat/completions \
    -H "Authorization: Bearer $OPENROUTER_API_KEY" -H 'content-type: application/json' -d "$body" \
  | python -c "
import json,sys
r=json.load(sys.stdin)
if 'error' in r: print('$1','ERROR',str(r['error'])[:200]); sys.exit()
u=r['usage']; d=u.get('completion_tokens_details') or {}
print('$1','completion',u.get('completion_tokens'),'reasoning',d.get('reasoning_tokens'))"
}
# ARMS (space separated) limits the arms, e.g. ARMS="flat-medium".
ARMS="${ARMS:-none flat-low flat-medium flat-high obj-low obj-high}"
for i in $(seq 1 "$N"); do
  for arm in $ARMS; do
    case "$arm" in
      none) probe none '{}' ;;
      flat-*) probe "$arm" "{\"reasoning_effort\":\"${arm#flat-}\"}" ;;
      obj-*) probe "$arm" "{\"reasoning\":{\"effort\":\"${arm#obj-}\"}}" ;;
    esac
  done
done
