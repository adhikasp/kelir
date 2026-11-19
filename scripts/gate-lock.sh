#!/usr/bin/env bash
# Hold the one machine-wide gate lock for this repository, then run a command.
#
# usage: scripts/gate-lock.sh [--timeout SECONDS] -- <command> [args...]
#
# Three agents build and test on one desktop (AGENTS.md, BUILD-INFRA rule 1).
# Two gates at once do not fail; they get slow and start to flake, and a flake
# costs a re-run (2-4x). So exactly one gate runs at a time across every
# worktree of this repo, and this is where the queue is: the lock lives in the
# shared git directory (.git of the main worktree, or of the whole clone), so
# every worktree, branch and clone of it on this machine contends for the same
# lock. Override the path with KELIR_GATE_LOCK (a shared gate driver outside
# the repo must point here so both serialize).
#
# The command runs with the lock held; its stdout, stderr and exit code are
# passed through. A lock whose owner is gone is taken over after a note.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
timeout_s="${KELIR_GATE_TIMEOUT:-1800}"
while [ $# -gt 0 ]; do
  case "$1" in
    --timeout) timeout_s="${2:?--timeout needs a value}"; shift 2 ;;
    --) shift; break ;;
    -h | --help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "gate-lock: unknown argument: $1" >&2; exit 2 ;;
  esac
done
if [ $# -eq 0 ]; then
  echo "usage: scripts/gate-lock.sh [--timeout SECONDS] -- <command> [args...]" >&2
  exit 2
fi

if [ -n "${KELIR_GATE_LOCK:-}" ]; then
  lock="$KELIR_GATE_LOCK"
else
  common="$(git -C "$here/.." rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || common=""
  if [ -z "$common" ]; then
    echo "gate-lock: not a git checkout and KELIR_GATE_LOCK is unset" >&2
    exit 2
  fi
  lock="$common/kelir-gate.lock"
fi
mkdir -p "$(dirname "$lock")"

# Only the process that took the lock may release it: a waiter that times out
# (or is interrupted) must leave the holder's lock exactly where it is.
held=0
release() {
  if [ "$held" = 1 ]; then
    held=0
    rm -rf "$lock" 2>/dev/null
  fi
}
trap release EXIT

waited=0
noted=0
while :; do
  if mkdir "$lock" 2>/dev/null; then
    held=1
    printf 'pid %s\nstarted %s\ncmd %s\n' "$$" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >"$lock/owner"
    break
  fi
  owner="$(sed -n 's/^pid //p' "$lock/owner" 2>/dev/null | head -1)"
  if [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; then
    # The holder is gone (a killed gate, a closed pane): take it over. `mv`
    # makes exactly one waiter win the race; the rest retry the mkdir.
    if mv "$lock" "$lock.stale.$$" 2>/dev/null; then
      echo "gate-lock: removed a stale lock (pid $owner is gone)" >&2
      rm -rf "$lock.stale.$$"
    fi
    continue
  fi
  if [ "$noted" = 0 ] || { [ $((waited % 30)) -eq 0 ] && [ "$waited" -gt 0 ]; }; then
    noted=1
    echo "gate-lock: waiting ${waited}s for the gate (held by pid ${owner:-unknown}, started $(sed -n 's/^started //p' "$lock/owner" 2>/dev/null | head -1))" >&2
  fi
  if [ "$timeout_s" != 0 ] && [ "$waited" -ge "$timeout_s" ]; then
    echo "gate-lock: gave up after ${waited}s waiting for $lock" >&2
    exit 75
  fi
  sleep 2
  waited=$((waited + 2))
done

"$@"
rc=$?
release
trap - EXIT
exit "$rc"
