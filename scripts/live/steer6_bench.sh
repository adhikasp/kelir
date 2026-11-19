#!/usr/bin/env bash
# Measure what a fixed scripted session puts on the wire before and after the
# STEER-6 loop changes. Mock only: no provider, no key, no network beyond
# 127.0.0.1.
#
# usage: scripts/live/steer6_bench.sh <old-binary> <new-binary>
#
# Both binaries run the same scripted conversation (three bash calls, one
# multi-hunk `edit`, one final text) against a recording OpenAI-compatible
# server. What is printed is what a real session pays on every step:
#
#   request k   bytes of request body k as the mock received it
#   system      bytes of the system message (the prompt rewrite)
#   tools       bytes of the tool JSON the model is shown (the `edits` schema)
#
# The system and tools blocks are frozen per session, so their delta is paid
# once; the messages array is re-sent every step, which is why it is measured
# per request as well.
set -euo pipefail

OLD="${1:?usage: steer6_bench.sh <old-binary> <new-binary>}"
NEW="${2:?usage: steer6_bench.sh <old-binary> <new-binary>}"
cd "$(git rev-parse --show-toplevel)"

work="$(mktemp -d)"
server_pid=""
cleanup() {
  # Kill exactly what this script started, by pid (never by image name).
  if [ -n "$server_pid" ]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

echo "the benchmark server is written from this script; see scripts/live/steer6_bench.py"
python "$(dirname "$0")/steer6_bench.py" --old "$OLD" --new "$NEW"
