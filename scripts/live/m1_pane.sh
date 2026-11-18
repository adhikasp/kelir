#!/usr/bin/env bash
# M1 live drive: start a --keep session in this pane. Usage: m1_pane.sh <scratch-dir> <task...>
# The key is expected in the environment (source the env file in the pane command first).
set -u
S="$1"; shift
D="$(cd "$(dirname "$0")" && pwd)"
export KELIR_STATE_DIR="$S/state"
export PATH="$PATH"
BIN="${KELIR_BIN:-$D/../../target/release/kelir.exe}"
cd "$S" || exit 1
"$BIN" run --keep --json --model "${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}" "$@" > "$S/out.ndjson" 2> "$S/err.txt"
echo "kelir exit: $?"
