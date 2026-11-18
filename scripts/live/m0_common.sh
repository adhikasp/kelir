#!/usr/bin/env bash
# Shared setup for the M0 live drive. Keys come from the environment only.
# Usage: source m0_common.sh <scenario-name>
set -u
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
KELIR_BIN="${KELIR_BIN:-$REPO/target/release/kelir}"
MODEL="${KELIR_LIVE_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
SCRATCH="${KELIR_SCRATCH:-$HOME/kelir-scratch/M0}"  # keep it outside the repo
export KELIR_STATE_DIR="$SCRATCH/state"
: "${OPENROUTER_API_KEY:?OPENROUTER_API_KEY must be set}"
WORK="$SCRATCH/$1"
rm -rf "$WORK"; mkdir -p "$WORK"; cd "$WORK"
