#!/usr/bin/env bash
# Run the whole "does it flash?" matrix once (bounded: 2 binaries x ~12 scenarios, each <= ~60 s).
# Usage: flash_matrix.sh  (output: $FLASH/summary.jsonl and per-scenario .report.txt; progress on stdout)
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
for b in head wip; do
  for s in calls bg cancel kill tui; do bash "$HERE/flash_run.sh" $b $s canned pane | head -8; done
  for s in calls bg mix; do bash "$HERE/flash_run.sh" $b $s canned detached | head -8; done
  bash "$HERE/flash_run.sh" $b mix canned hidden | head -8
  for s in calls bg tui; do bash "$HERE/flash_run.sh" $b $s live pane | head -8; done
  bash "$HERE/flash_run.sh" $b calls live detached | head -8
done
echo MATRIX-DONE
