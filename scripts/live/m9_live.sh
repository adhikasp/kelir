#!/usr/bin/env bash
# M9 live drive: a real cheap-model job run twice with the release binary.
# The key comes from the env file OUTSIDE the repo; never printed or copied.
# Usage (from a Herdr pane): bash scripts/live/m9_live.sh
set -uo pipefail
set -a
. /c/Users/the maintainer/kelir-scratch/live.env
set +a

BIN=/c/Users/the maintainer/kelir-wt/m9rev/target/release/kelir.exe
SCRATCH=/c/Users/the maintainer/kelir-scratch/m9rev-live
STATE="$SCRATCH/state"
CFGDIR="$SCRATCH/config"
mkdir -p "$STATE" "$CFGDIR" "$SCRATCH/work"

cat > "$CFGDIR/jobs.toml" <<'EOF'
[jobs.livedrive]
task = "Reply with exactly: M9 LIVE OK. Then write the same to notes/live.txt using bash."
schedule = "review-only"
EOF

run1() {
  (cd "$SCRATCH/work" && KELIR_STATE_DIR="$STATE" KELIR_CONFIG_DIR="$CFGDIR" \
    "$BIN" job run livedrive)
}
run1
echo "RUN1_EXIT=$?"
run1
echo "RUN2_EXIT=$?"
echo "STATUS:"; cat "$STATE/jobs/livedrive/STATUS.md"
echo "HISTORY:"; cat "$STATE/jobs/livedrive/runs.jsonl"
echo "SESSIONS:"; (cd "$SCRATCH/work" && KELIR_STATE_DIR="$STATE" KELIR_CONFIG_DIR="$CFGDIR" "$BIN" sessions ls --json)
echo "NOTES:"; cat "$STATE/jobs/livedrive/notes/live.txt" 2>/dev/null || echo "(no note)"
