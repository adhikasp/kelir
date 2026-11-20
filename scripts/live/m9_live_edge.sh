#!/usr/bin/env bash
# M9 live drive, scenario 2: a second concurrent run must skip (75), and a
# hung model request with a timeout must exit 124 with STATUS=timeout.
# Runs in a Herdr pane with the key sourced from an env file outside the repo.
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
[jobs.livetimeout]
task = "Say hello"
timeout = "3s"
EOF

cd "$SCRATCH/work" || exit 1
export KELIR_STATE_DIR="$STATE" KELIR_CONFIG_DIR="$CFGDIR"
# Scenario: concurrent run against the live holder. Start one held open by a
# slow prompt, then a second while it is running.
"$BIN" job run livedrive >/dev/null 2>&1 &
HOLDER=$!
sleep 4
"$BIN" job run livedrive 2>&1 | head -2
echo "SKIP_EXIT=${PIPESTATUS[0]}"
wait $HOLDER
echo "HOLDER_EXIT=$?"
# A timed-out run.
"$BIN" job run livetimeout >/dev/null 2>&1
echo "TIMEOUT_EXIT=$?"
echo "TIMEOUT_STATUS:"; cat "$STATE/jobs/livetimeout/STATUS.md" 2>/dev/null | head -8
