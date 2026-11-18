#!/usr/bin/env bash
# M0 re-verification live drive, meant to run inside a Herdr pane with the key already in the env.
D="$(dirname "$0")"
export KELIR_SCRATCH="${KELIR_SCRATCH:-$HOME/kelir-scratch/M0b}"
export KELIR_BIN="${KELIR_BIN:-$D/../../target/release/kelir.exe}"
echo "##### S1"; bash "$D/m0_s1_hello.sh" 2>&1 | cut -c1-200
echo "##### S2"; bash "$D/m0_s2_cli.sh" 2>&1 | cut -c1-200
echo "##### F1 live"; bash "$D/m0_f1_live.sh" 2>&1 | cut -c1-200
echo "##### DONE"
