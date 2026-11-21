#!/usr/bin/env bash
# M11 live drive: release `kelir run --keep` against a real cheap model in a
# Herdr pane. Keys come from the environment only (the OPENROUTER_API_KEY or
# KELIR_GO_API_KEY the provider reads); no key is stored in the repo.
# Usage: M11_LIVE_ROOT=/scratch/dir bash scripts/live/m11rev-session-pane.sh
set -uo pipefail
R="${M11_LIVE_ROOT:-$HOME/kelir-m11-live}"
mkdir -p "$R/work" "$R/state" "$R/home"
cd "$R" || exit 1
export KELIR_STATE_DIR="$R/state"
export KELIR_HOME="$R/home"
cd "$R/work" || exit 1
export KELIR_CONFIG="$R/work/config.toml"
exec "${KELIR_BIN:-$R/kelir.exe}" run --keep "Say exactly LIVE_M11_READY and nothing else."
