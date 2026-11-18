#!/usr/bin/env bash
# M2 status check: run kelir's TUI against the canned mock inside this Herdr
# pane, so a driver outside can watch the OSC 7501 status flip and the shell's
# exit line.
#
# Usage (the pane's command): m2_status_check.sh <scratch-dir> [port]
# The driver: `hc run --bg --name osc-check --cwd <scratch> "bash" -c '<this>'`,
# then `hc send osc-check "hi"` and poll `hc get osc-check` for `status`.
#
# What to look for:
#   - while the mock's `sleep 8` tool call runs: status {"state": "working"}
#   - after the answer: {"state": "done"}
#   - after /quit or a double Ctrl-C: the pane shows `kelir exit: 0` and
#     `after: ...` on fresh lines below the transcript, never on the banner row.
#
# Env: KELIR_TEST_PANIC=1 in the pane exercises the panic hook instead.
#      M2_CHECK_INPUT=1 asks for one typed line after kelir exits: a terminal
#      that was left in raw mode cannot echo it, so seeing `typed: ...` proves
#      the shell got a cooked terminal back.
set -u
S="${1:?usage: m2_status_check.sh <scratch-dir> [port]}"
PORT="${2:-8971}"
D="$(cd "$(dirname "$0")" && pwd)"
BIN="${KELIR_BIN:-$D/../../target/release/kelir}"
mkdir -p "$S" || exit 1
export KELIR_STATE_DIR="$S/state"
export KELIR_BASE_URL="http://127.0.0.1:$PORT/v1"
export OPENROUTER_API_KEY="${OPENROUTER_API_KEY:-mock-key}"
export KELIR_HERDR=1
python "$D/canned_server.py" "$PORT" "sleep 8" &
SERVER=$!
cleanup() { kill "$SERVER" 2>/dev/null; }
trap cleanup EXIT
sleep 1
cd "$S" || exit 1
"$BIN" tui --model mock/model
echo "kelir exit: $?"
echo "after: $?"
if [ "${M2_CHECK_INPUT:-0}" = "1" ]; then
  printf "type a line: "
  read -r line
  echo "typed: ${line:-<nothing>}"
fi
cleanup
