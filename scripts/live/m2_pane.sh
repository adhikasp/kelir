#!/usr/bin/env bash
# M2 live drive: start the interactive TUI in this pane. Usage: m2_pane.sh <scratch-dir> [kelir args...]
# The key is expected in the environment (source the env file in the pane command first).
set -u
S="$1"; shift
D="$(cd "$(dirname "$0")" && pwd)"
export KELIR_STATE_DIR="$S/state"
BIN="${KELIR_BIN:-$D/../../target/release/kelir.exe}"
cd "$S" || exit 1
"$BIN" tui --model "${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}" "$@"
echo "kelir exit: $?"
