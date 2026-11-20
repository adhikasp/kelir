#!/usr/bin/env bash
# STEER-12 review: does the 'notes file' rule leave junk in the repo? N fresh clones, NEW prompt,
# a refactor that invites a plan, then `git status` / the commit's file list.
# Run in a Herdr pane with the OpenRouter env sourced (see steer12_ab.sh).
set -u
R="$1"; K="$2"; SRC="$3"; N="${4:-3}"
MODEL="${MODEL:-deepseek/deepseek-v4.1-flash-1017}"
TASK="Plan this change carefully (list every use with file:line first), then make it: rename the constant HEAD_BYTES in src/spill.rs to HEAD_LEN everywhere it is used under src/ and tests/. Do not build or run tests. Commit the result with git (message: rename HEAD_BYTES). Then say what you did."
for i in $(seq 1 "$N"); do
  D="$R/notes/r$i"; rm -rf "$D"; mkdir -p "$D/state"
  git clone -q --local "$SRC" "$D/repo"
  git -C "$D/repo" config commit.gpgsign false
  git -C "$D/repo" config user.name reviewer
  git -C "$D/repo" config user.email reviewer@example.invalid
  echo "[notes] run $i"
  env KELIR_STATE_DIR="$D/state" KELIR_CONFIG_DIR="$R/cfg-empty" KELIR_HOME="$D/home" \
    timeout 900 "$K" run --provider openrouter --model "$MODEL" --cwd "$D/repo" --max-steps 80 "$TASK" > "$D/stdout.txt" 2> "$D/stderr.txt"
  echo "[notes] rc=$?"
  { echo "--- status"; git -C "$D/repo" status --short; echo "--- last commit files"; git -C "$D/repo" show --stat --format=%s HEAD | head -20; } > "$D/after.txt"
done
echo "[notes] DONE"
