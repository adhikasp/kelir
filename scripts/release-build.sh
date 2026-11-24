#!/usr/bin/env bash
# Build the binary that ships (issue #23).
#
# The size-optimised profile is in Cargo.toml. `-Z build-std` is a *Cargo* flag,
# so it has to be on the command line: in `profile.release.rustflags` it would
# reach rustc, which does not know it. It is deliberately not in `.cargo/config.toml`
# either: there it would apply to tests too, and a test must keep `panic = "unwind"`
# so it can catch a panic.
set -eu
cd "$(dirname "$0")/.."
if cargo -Z help 2>/dev/null | grep -q -- '-Z build-std'; then
  cargo -Z build-std=std,panic_abort \
        -Z build-std-features=optimize_for_size \
        build --release "$@"
else
  echo "release-build: this toolchain cannot build std; using the stable-flag profile" >&2
  cargo build --release "$@"
fi
