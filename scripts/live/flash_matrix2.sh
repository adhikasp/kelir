#!/usr/bin/env bash
# Second pass: the scenarios whose first run was invalid (bg JSON quoting, live provider) or whose PATH was wrong,
# for head, wip (snapshot) and wipnow (current working tree).
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
for b in head wip wipnow; do
  for s in bg cancel; do bash "$HERE/flash_run.sh" $b $s canned pane | head -8; done
  for s in bg mix; do bash "$HERE/flash_run.sh" $b $s canned detached | head -8; done
  [ $b != wipnow ] || { bash "$HERE/flash_run.sh" $b calls canned pane | head -8; bash "$HERE/flash_run.sh" $b kill canned pane | head -8; bash "$HERE/flash_run.sh" $b tui canned pane | head -8; bash "$HERE/flash_run.sh" $b calls canned detached | head -8; }
  for s in calls bg tui; do bash "$HERE/flash_run.sh" $b $s live pane | head -8; done
  bash "$HERE/flash_run.sh" $b bg live detached | head -8
done
echo MATRIX2-DONE
