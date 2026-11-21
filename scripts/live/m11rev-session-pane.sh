#!/usr/bin/env bash
# M11 reviewer live drive: release `kelir run --json --keep` against a real
# cheap OpenRouter model in a Herdr pane. The key is read from an env file
# outside the repo and never printed or copied into the repo.
set -uo pipefail
R=/c/Users/the maintainer/kelir-scratch/m11rev-live
cd "$R" || exit 1
set -a
. /c/Users/the maintainer/kelir-scratch/live.env
set +a
export KELIR_STATE_DIR="$R/state"
export KELIR_HOME="$R/home"
mkdir -p "$R/work" "$R/state" "$R/home"
cd "$R/work" || exit 1
export KELIR_CONFIG="$R/work/config.toml"
exec "$R/kelir.exe" run --keep "Say exactly LIVE_M11_READY and nothing else."
