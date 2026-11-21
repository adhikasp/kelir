#!/usr/bin/env bash
# M11 reviewer live drive: attach to the session in m11rev-live1 from a second
# Herdr pane. Uses only the release binary and a scratch state dir.
set -uo pipefail
R=/c/Users/the maintainer/kelir-scratch/m11rev-live
cd "$R" || exit 1
export KELIR_STATE_DIR="$R/state"
export KELIR_HOME="$R/home"
export KELIR_CONFIG="$R/work/config.toml"
ID="$(basename "$(find "$R/state/sessions" -mindepth 1 -maxdepth 1 -type d | head -1)")"
echo "ATTACHING to $ID"
exec "$R/kelir.exe" attach "$ID" --follow --plain
