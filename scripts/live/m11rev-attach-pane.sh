#!/usr/bin/env bash
# M11 live drive: attach from a second Herdr pane to the session started by
# m11rev-session-pane.sh. Uses only the release binary and a scratch state dir.
# Usage: M11_LIVE_ROOT=/scratch/dir bash scripts/live/m11rev-attach-pane.sh
set -uo pipefail
R="${M11_LIVE_ROOT:-$HOME/kelir-m11-live}"
cd "$R" || exit 1
export KELIR_STATE_DIR="$R/state"
export KELIR_HOME="$R/home"
export KELIR_CONFIG="$R/work/config.toml"
ID="$(basename "$(find "$R/state/sessions" -mindepth 1 -maxdepth 1 -type d | head -1)")"
echo "ATTACHING to $ID"
exec "${KELIR_BIN:-$R/kelir.exe}" attach "$ID" --follow --plain
