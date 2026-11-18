#!/usr/bin/env bash
# Build and test HEAD on another machine reachable through Herdr (`hc --on <machine>`).
# usage: scripts/remote-check.sh <machine> [workdir-name]
# The machine needs a Rust toolchain and git. Prints fmt/clippy/test summary; exit 1 on any failure.
set -euo pipefail
M="${1:?usage: remote-check.sh <hc-machine>}"; W="${2:-kelir-verify}"
cd "$(git rev-parse --show-toplevel)"
B="$(mktemp -u).bundle"
git bundle create "$B" --all >/dev/null 2>&1
base64 -w0 "$B" | hc file write --on "$M" "$W/k.b64" >/dev/null
rm -f "$B"
hc exec --on "$M" --timeout 600 -- bash -lc "cd ~/$W && base64 -d k.b64 > k.bundle && rm -rf src-tree && git clone -q k.bundle src-tree && cd src-tree \
 && echo \"HEAD: \$(git log --oneline | head -1)\" && cargo fmt --check && echo FMT-OK \
 && cargo clippy --all-targets -- -D warnings 2>&1 | tail -15 && echo CLIPPY-DONE \
 && cargo test 2>&1 | grep -E '^test result|FAILED|panicked|^error' | head -40"
