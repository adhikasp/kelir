#!/usr/bin/env bash
# STEER-12 review: A/B of the OLD base prompt (git show b7b60c6:src/prompt.md) vs the NEW embedded one,
# on read-heavy tasks, against a real cheap model. Run inside a Herdr pane:
#   hc run --bg --name live-steer12-ab --cwd <scratch> "C:/Program Files/Git/bin/bash.exe" -c \
#     "set -a; . <env file with OPENROUTER_API_KEY>; set +a; bash scripts/live/steer12_ab.sh <scratch> <kelir.exe> <repo clone> <runs>"
# The key is only read from the environment; nothing here prints it.
set -u
R="$1"; K="$2"; REPO="$3"; N="${4:-3}"
MODEL="${MODEL:-deepseek/deepseek-v4.1-flash-1017}"
mkdir -p "$R/ab"
cat > "$R/ab/old.toml" <<EOF
prompt = "$R/old-prompt.md"
EOF
: > "$R/ab/new.toml"
T1="Explain how the gate lock works in this repo (who takes it, where, what happens when it is held). Read only: do not modify any file. Answer in under 120 words."
T2="Find where retries of provider errors are decided (file and function names, and the backoff rule). Read only: do not modify any file. Answer in under 80 words."
for i in $(seq 1 "$N"); do
  for t in 1 2; do
    for arm in old new; do
      eval "TASK=\$T$t"
      D="$R/ab/$arm-t$t-r$i"; rm -rf "$D"; mkdir -p "$D/state"
      echo "[ab] $arm t$t run $i"
      env KELIR_STATE_DIR="$D/state" KELIR_CONFIG_DIR="$R/cfg-empty" KELIR_HOME="$D/home" \
        timeout 600 "$K" run --config "$R/ab/$arm.toml" --provider openrouter --model "$MODEL" \
        --cwd "$REPO" --max-steps 60 "$TASK" > "$D/stdout.txt" 2> "$D/stderr.txt"
      echo "[ab] rc=$?"
    done
  done
done
echo "[ab] DONE"
