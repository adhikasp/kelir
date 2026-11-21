#!/usr/bin/env bash
# Opt-in compiler wrapper for sccache (docs/BUILD.md). Cargo calls this in place
# of rustc; the wrapper forwards to sccache when it is installed and straight to
# rustc otherwise, so a clone that has never heard of sccache still builds.
#
# Enable per shell (not per repo):
#   export RUSTC_WRAPPER="$(git rev-parse --show-toplevel)/scripts/rustc-wrapper.sh"
#   export CARGO_INCREMENTAL=0   # sccache will not cache incremental builds
#   cargo build --tests
# Or inline:
#   RUSTC_WRAPPER="$PWD/scripts/rustc-wrapper.sh" CARGO_INCREMENTAL=0 cargo test

set -eu
# Cargo invokes `RUSTC_WRAPPER <rustc-path> <rustc args...>`, so the real
# compiler is already $1. Do not resolve `rustc` from PATH here: that would be
# the rustup shim, which sccache would then call as a second compiler.
if command -v sccache >/dev/null 2>&1; then
  exec sccache "$@"
fi
shift # no sccache: drop the compiler path Cargo passed and call it directly
exec "$@"
