#!/usr/bin/env bash
# Shared helpers for the M4 live drive. Keys come from the environment only
# (the pane command sources an env file outside the repo before calling a scenario).
# Usage: source m4_common.sh <scratch-dir> <scenario-name>
set -u
D="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REL="$D/../../target/release"
export PATH="$REL:$PATH"                       # the model types plain `kelir ...`
BIN="${KELIR_BIN:-$REL/kelir.exe}"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
: "${OPENROUTER_API_KEY:?OPENROUTER_API_KEY must be set}"
S="$1/$2"
rm -rf "$S"; mkdir -p "$S/work"
export KELIR_STATE_DIR="$S/state"
cd "$S/work" || exit 1
LOGF="$S/driver.log"; : > "$LOGF"
say() { echo "[$(date +%T.%3N)] $*" | tee -a "$LOGF"; }

# Windows process list: lines "pid name cmdline" whose command line matches a .NET regex.
procs() {
  powershell.exe -NoProfile -Command "Get-CimInstance Win32_Process | Where-Object { \$_.CommandLine -match '$1' } | ForEach-Object { \"\$(\$_.ProcessId) \$(\$_.Name) \$(\$_.CommandLine)\" }" | tr -d '\r'
}
winpid() { cat "/proc/$1/winpid" 2>/dev/null || echo "$1"; }
killwin() { taskkill //F //PID "$1" > /dev/null 2>&1; }
alive() { tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q "$1"; }

LOG=""
find_log() {
  for _ in $(seq 1 200); do
    LOG="$(ls "$S"/state/sessions/*/events.jsonl 2>/dev/null | head -n1)"
    [ -n "$LOG" ] && return 0
    sleep 0.05
  done
  return 1
}
# wait_for <grep -c pattern> <min count> <timeout s>: poll the event log.
wait_for() {
  local t=0
  while [ "$(grep -c "$1" "$LOG" 2>/dev/null || true)" -lt "$2" ]; do
    sleep 0.1; t=$((t+1)); [ "$t" -gt $(($3*10)) ] && return 1
  done
  return 0
}
