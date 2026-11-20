#!/usr/bin/env bash
# M8 live drive: example gates against a real cheap model. Run inside a Herdr pane:
#   hc run --bg --name live-m8-a --cwd <scratch> bash -c 'set -a; . <envfile>; set +a; bash scripts/live/m8-gates.sh <scenario> <scratch>'
# Scenarios: hedge | deny-note | numbered | herdr | herdr-out | herdr-hint | git
# Keys come from the environment only. Prints the session's gate events at the end.
set -u
scn="${1:?scenario}"; scratch="${2:?scratch dir}"
repo="$(cd "$(dirname "$0")/../.." && pwd)"
case "$(uname -s)" in MINGW*|MSYS*) win=1 ;; *) win=0 ;; esac
if [ "$win" = 1 ]; then py=python; repo="$(cd "$repo" && pwd -W)"; scratch="$(cd "$scratch" && pwd -W)"; else py=python3; fi
bin="$repo/target/release/kelir"; [ -x "$bin" ] || bin="$bin.exe"
ex="$repo/examples/gates"
w="$scratch/$scn"; rm -rf "$w"; mkdir -p "$w/cwd" "$w/state"
export KELIR_STATE_DIR="$w/state" KELIR_HOME="$w/home" KELIR_CONFIG_DIR="$w/no-config"
cfg="$w/config.toml"
base='provider = "openrouter"
model = "deepseek/deepseek-v4.1-flash-1017"
'
case "$scn" in
  hedge)
    printf '%s[gates]\nturn_end = "%s %s/verify_before_claim.py"\n' "$base" "$py" "$ex" > "$cfg"
    task="Without running any command or reading any file, answer in one sentence from memory and start with the word 'Probably': which TCP port does PostgreSQL listen on by default?" ;;
  deny-note)
    printf '%s[gates]\nbefore_tool = { name = "no-rm", command = "%s %s/deny_rm.py", tools = ["bash"] }\nafter_tool = { command = "%s %s/note_check.py", tools = ["edit"], env = { NOTE_CHECK_CMD = "%s %s/checker.py" } }\n' "$base" "$py" "$w" "$py" "$ex" "$py" "$w" > "$cfg"
    cat > "$w/deny_rm.py" <<'PY'
import json, sys
e = json.load(sys.stdin)
cmd = (e.get("input") or {}).get("command", "")
if "rm " in cmd or cmd.startswith("rm"):
    print(json.dumps({"decision": "deny", "reason": "deleting files is not allowed in this session"}))
PY
    cat > "$w/checker.py" <<'PY'
import sys
t = open(sys.argv[1], encoding="utf-8").read()
if "TODO" in t:
    print("checker: file still contains TODO"); sys.exit(1)
PY
    task="Using your edit tool create the file notes.txt in the current directory containing the single line 'TODO write this'. Then delete notes.txt with bash rm. Then tell me exactly what happened in each step, quoting any gate or tool messages." ;;
  numbered)
    printf '%s[gates]\nprompt_submit = "%s %s/numbered_answers.py"\n' "$base" "$py" "$ex" > "$cfg"
    task="Reply with exactly this numbered list and nothing else:
1. say apple
2. say banana
3. say cherry
4. say date" ;;
  herdr|herdr-out|herdr-hint)
    printf '%s' "$base" > "$cfg"
    [ "$scn" = herdr-hint ] && printf '[hint]
model = "deepseek/deepseek-v4.1-flash-1017"
' >> "$cfg"
    task="How many agents share my Herdr space right now? Answer from live data, list them." ;;
  git)
    printf '%s[gates]\nsession_start = "%s %s/git_sync.py"\nsession_end = { command = "%s %s/git_sync.py", env = { GIT_SYNC_ADD_NEW = "1" } }\n' "$base" "$py" "$ex" "$py" "$ex" > "$cfg"
    git init -q --bare "$w/remote.git"; git clone -q "$w/remote.git" "$w/cwd" 2>/dev/null
    ( cd "$w/cwd" && git config user.email r@example.com && git config user.name rev && git config commit.gpgsign false && echo base > base.txt && git add . && git commit -qm init && git push -q origin HEAD )
    task="Create a file hello.txt containing the word hi using your edit tool, then say done." ;;
  *) echo "unknown scenario"; exit 2 ;;
esac
cd "$w/cwd" || exit 1
if [ "$scn" = herdr-out ]; then unset HC_TERMINAL_ID HC_TERMINAL_SOCKET HC_SCOPE; fi
export KELIR_CONFIG="$cfg"
if [ "$scn" = numbered ]; then
  # A kept session: first turn makes the list, then a second prompt "do 2,4" goes through ctl say.
  "$bin" run --keep --json "$task" > "$w/out.ndjson" 2> "$w/err.txt" &
  kpid=$!
  trap 'kill $kpid 2>/dev/null' EXIT
  sid=""; for _ in $(seq 1 60); do sid="$(ls -t "$w/state/sessions" 2>/dev/null | head -1)"; [ -n "$sid" ] && grep -q '"type":"turn_ended"' "$w/state/sessions/$sid/events.jsonl" 2>/dev/null && break; sleep 1; done
  "$bin" ctl latest say "do 2,4" > "$w/ctl.txt" 2>&1
  for _ in $(seq 1 60); do [ "$(grep -c '"type":"turn_ended"' "$w/state/sessions/$sid/events.jsonl")" -ge 2 ] && break; sleep 1; done
  "$bin" ctl latest end >> "$w/ctl.txt" 2>&1; wait $kpid; echo "exit=$?"
else
  "$bin" run --json "$task" > "$w/out.ndjson" 2> "$w/err.txt"
  echo "exit=$?"
fi
sid="$(ls -t "$w/state/sessions" | head -1)"
log="$w/state/sessions/$sid/events.jsonl"
echo "--- gate events"; grep '"type":"gate"' "$log" | cut -c1-400
echo "--- tool_settled"; grep '"type":"tool_settled"' "$log" | cut -c1-300
echo "--- user events from gate/ctl"; grep '"type":"user"' "$log" | cut -c1-300
echo "--- final"; grep '"final_text"' "$w/out.ndjson" | tail -1 | cut -c1-600
echo "--- stderr"; head -c 600 "$w/err.txt"
echo "log=$log"
