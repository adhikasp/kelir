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
# lock. Override the path with KELIR_GATE_LOCK (a shared gate driver outside the
# repo must point it here so both serialize).
#
# The lock is a file holding one record, and it is published with `ln` (portable create-or-fail) of a
# record written beforehand: create-or-fail, on a file, a directory or a symlink
# alike. So a lock that exists always names its owner, and two gates can never
# both publish (buildinfra-review B2: the old script `mkdir`ed and then wrote
# `owner`, and a waiter that had waited 10 s stole the ownerless gap).
#
#     token <random token, unique per acquisition>
#     pid <holder pid>  start <its start time>  cmd <its command line>
#     started <ISO 8601 UTC>
#
# The holder runs the command in a process group of its own, and the command's
# side publishes the group as `$lock.grp.<token>` before it execs. That record
# is what ties the lock to the gate's whole process tree: `kill -9` of the
# wrapper leaves the command running, and the next gate waits for the recorded
# group instead of running on top of it (S3). The child writes it because the
# child is the process that survives.
#
# A lock is taken over only when the recorded gate is verifiably gone:
#   * the recorded pid is gone, or a different process has it (same pid, other
#     start time: pid reuse);
#   * the recorded process group, if the command published one, has no live
#     member;
#   * and the "pid is gone, no live group" verdict held on two consecutive
#     looks, which covers a child that is still starting up;
# The takeover then re-reads the record after moving the lock aside atomically
# and hands it back untouched if it is not the lock it examined (S4a). A holder
# only ever removes a lock that still carries its own token (S4b).
#
# A path this script did not write is never taken over: an outside driver's
# plain `mkdir` (S2), or any other path without a readable record, is waited on
# and the message says how to clear it by hand. A directory with an `owner` file
# is the format of the version before this one; it is taken over on a dead pid
# once it is a minute old, so an upgrade cannot wedge the queue behind it.
#
# The command's stdout, stderr and exit code are passed through. On INT/TERM
# that process group is signalled (only it, never the console's group: AGENTS.md
# hard rule) and reaped before the lock is released; if it survives that, the
# lock is left in place rather than handed to a second gate. A waiter that gives
# up exits 75 and leaves the lock alone.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
timeout_s="${KELIR_GATE_TIMEOUT:-1800}"
while [ $# -gt 0 ]; do
  case "$1" in
    --timeout) timeout_s="${2:?--timeout needs a value}"; shift 2 ;;
    --) shift; break ;;
    -h | --help) sed -n '2,55p' "$0"; exit 0 ;;
    *) echo "gate-lock: unknown argument: $1" >&2; exit 2 ;;
  esac
done
if [ $# -eq 0 ]; then
  echo "usage: scripts/gate-lock.sh [--timeout SECONDS] -- <command> [args...]" >&2
  exit 2
fi
case "$timeout_s" in
  '' | *[!0-9]*) echo "gate-lock: --timeout must be a number of seconds: $timeout_s" >&2; exit 2 ;;
esac

if [ -n "${KELIR_GATE_LOCK:-}" ]; then
  lock="${KELIR_GATE_LOCK%/}"
else
  common="$(git -C "$here/.." rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || common=""
  if [ -z "$common" ]; then
    echo "gate-lock: not a git checkout and KELIR_GATE_LOCK is unset" >&2
    exit 2
  fi
  lock="${common%/}/kelir-gate.lock"
fi
mkdir -p "$(dirname "$lock")" || exit 2

# -------------------------------------------------------------- process facts
#
# `kill -0` cannot tell a live holder from a recycled pid, so liveness is always
# decided with a start-time token: field 22 of /proc/<pid>/stat is fixed for the
# life of a process and differs for a reused pid. MSYS (Git Bash on Windows) and
# Linux both have /proc; `ps -o lstart=` is the fallback. Anything unreadable is
# "alive if it exists", which is the safe side: a wrong "stale" verdict is two
# gates at once, a wrong "alive" is a wait.

# proc_stat <pid>: sets p_state, p_pgid, p_start. False if unreadable.
proc_stat() {
  local line rest
  [ -r "/proc/$1/stat" ] || return 1
  line=$(<"/proc/$1/stat") || return 1
  # The command name is in parentheses and may hold spaces or a ')', so cut
  # through the last ') ' instead of splitting the whole line.
  rest="${line##*) }"
  # shellcheck disable=SC2086
  set -- $rest
  # $@ starts at stat field 3: $1 is the state, $3 the process group (field 5),
  # $20 the start time (field 22).
  [ $# -ge 20 ] || return 1
  p_state="$1" p_pgid="$3" p_start="${20}"
}

pid_start() { # <pid>
  local out
  if proc_stat "$1"; then printf '%s\n' "$p_start"; return 0; fi
  command -v ps >/dev/null 2>&1 || return 1
  out=$(ps -o lstart= -p "$1" 2>/dev/null) || return 1
  out=$(printf '%s' "$out" | tr -s ' ' | sed 's/^ //; s/ $//')
  [ -n "$out" ] || return 1
  printf '%s\n' "$out"
}

pid_cmd() { # <pid>
  local cmd=""
  [ -r "/proc/$1/cmdline" ] && cmd=$(tr '\0' ' ' <"/proc/$1/cmdline" 2>/dev/null)
  [ -n "$cmd" ] || cmd=$(ps -o args= -p "$1" 2>/dev/null || true)
  printf '%s' "$cmd" | tr '\n\t' '  ' | sed 's/ *$//'
}

# alive <pid> <start>: that exact process, not a recycled pid.
alive() {
  [ -n "$1" ] || return 1
  kill -0 "$1" 2>/dev/null || return 1
  case "${2:-}" in
    '' | unknown) return 0 ;;
  esac
  [ "$(pid_start "$1" 2>/dev/null)" = "$2" ]
}

# group_alive <pgid>: used only in the "keep the lock" direction, so a false
# positive costs a wait, never two gates.
group_alive() {
  case "${1:-}" in
    '' | '-' | 0) return 1 ;;
  esac
  kill -0 "-$1" 2>/dev/null
}


# ----------------------------------------------------------------- the record

my_token="$(date -u +%Y%m%dT%H%M%SZ).$$.$RANDOM$RANDOM"
my_start="$(pid_start "$$" 2>/dev/null || echo unknown)"
my_cmd="$(pid_cmd "$$")"
grp_file="$lock.grp.$my_token"

rec_format="" # file | old | none
rec_token="" rec_pid="" rec_start="" rec_group=""

read_record() {
  rec_format="none" rec_token="" rec_pid="" rec_start="" rec_group=""
  local src="$lock" line
  if [ -d "$lock" ]; then
    # The pre-atomic format: a directory with an `owner` file.
    [ -f "$lock/owner" ] || return 1
    rec_format="old"
    src="$lock/owner"
  elif [ -f "$lock" ]; then
    rec_format="file"
  else
    return 1
  fi
  [ -r "$src" ] || return 1
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      token\ *) rec_token="${line#token }" ;;
      pid\ *) rec_pid="${line#pid }" ;;
      start\ *) rec_start="${line#start }" ;;
    esac
  done <"$src"
  if [ "$rec_format" = "file" ] && [ -z "$rec_token" ]; then
    # A file that carries no record: a driver's own lock file, a stray path.
    rec_format="none"
    return 1
  fi
  [ -n "$rec_pid" ]
}

# The command's group record, written by the command's own process group. Only
# believed when it carries the token of the lock that was read.
read_group() {
  rec_group=""
  local f g
  [ -n "$rec_token" ] || return 0
  f="$lock.grp.$rec_token"
  [ -r "$f" ] || return 0
  g=$(sed -n 's/^pgid //p' "$f" 2>/dev/null | head -1)
  [ -n "$g" ] && rec_group="$g"
}

write_record() { # <file> <pid> <start>
  printf 'token %s\npid %s\nstart %s\ncmd %s\nstarted %s\n' \
    "$my_token" "$2" "$3" "$my_cmd" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >"$1"
}

# ------------------------------------------------------------------- the gate

held=0 child_pid="" child_pgid="" nap_pid="" verdict_pending=0

# Sleeping in the foreground defers an INT/TERM trap until the sleep returns,
# and a wrapper that lingers after its signal is a leaked process (AGENTS.md
# hard rule; nextest reports it as LEAK). So every wait is interruptible.
nap() { # <seconds>
  sleep "$1" &
  nap_pid=$!
  wait "$nap_pid" 2>/dev/null
  nap_pid=""
}

release() {
  [ "$held" = 1 ] || return 0
  held=0
  local tok=""
  [ -f "$lock" ] && tok=$(sed -n 's/^token //p' "$lock" 2>/dev/null | head -1)
  if [ "$tok" = "$my_token" ]; then
    rm -f "$lock" 2>/dev/null
  else
    # Ours was taken over (a verdict that was wrong) or removed by hand: never
    # remove a lock that belongs to somebody else (S4b).
    echo "gate-lock: leaving $lock in place: it no longer carries this gate's token" >&2
  fi
  rm -f "$grp_file" 2>/dev/null
}

# Publish the record atomically with noclobber: create-or-fail, so one gate
# wins and a lock without an owner cannot exist.
try_publish() {
  # POSIX `set -C` makes `>` fail if the path already exists (file OR directory),
  # atomically, on Linux, macOS and MSYS. The temp record is written first, then
  # renamed into place only if the lock path is free - but rename would replace a
  # directory, so instead write DIRECTLY to the lock under noclobber. If it
  # fails, nothing was created. This is the portable replacement for GNU ln -T.
  if ( set -C; write_record "$lock" "$$" "$my_start" ) 2>/dev/null; then
    held=1
    return 0
  fi
  return 1
}

# Move a stale lock aside, then believe it only if it is still the lock that was
# judged stale: between the verdict and the move another gate may have taken
# over and published, and that gate's lock must be handed back, not deleted.
takeover() { # <reason>
  local reason="$1" tomb="$lock.stale.$my_token" moved="" try=0
  mv "$lock" "$tomb" 2>/dev/null || return 1
  if [ -f "$tomb" ]; then
    moved=$(sed -n 's/^token //p' "$tomb" 2>/dev/null | head -1)
  elif [ -f "$tomb/owner" ]; then
    moved="old"
  fi
  if { [ -n "$rec_token" ] && [ "$moved" = "$rec_token" ]; } ||
    { [ "$rec_format" = "old" ] && [ "$moved" = "old" ]; }; then
    rm -rf "$tomb" 2>/dev/null
    echo "gate-lock: took over a stale lock ($reason)" >&2
    return 0
  fi
  while :; do
    if ( set -C; cat "$tomb" >"$lock" ) 2>/dev/null; then
      rm -f "$tomb" 2>/dev/null
      echo "gate-lock: a lock changed while it was examined; it was kept for its owner" >&2
      return 1
    fi
    try=$((try + 1))
    if [ "$try" -ge 60 ]; then
      echo "gate-lock: ERROR: cannot hand $lock back to its owner; it is at $tomb" >&2
      echo "gate-lock: refusing to run without the lock. Check that path by hand." >&2
      exit 2
    fi
    sleep 1
  done
}

wait_for_lock() {
  local waited=0 noted=0 stale foreign foreign_noted=0
  while :; do
    if try_publish; then
      verdict_pending=0
      return 0
    fi
    stale="" foreign=""
    if read_record; then
      read_group
      if alive "$rec_pid" "$rec_start"; then
        verdict_pending=0
      elif [ -n "$rec_group" ] && group_alive "$rec_group"; then
        verdict_pending=0
      elif [ "$verdict_pending" = 1 ]; then
        if kill -0 "$rec_pid" 2>/dev/null; then
          stale="pid $rec_pid now belongs to a different process (pid reuse)"
        else
          stale="pid $rec_pid is gone and its process group is empty"
        fi
        verdict_pending=0
      else
        # First look: a child may still be starting (it publishes its group
        # before it execs). One more poll decides.
        verdict_pending=1
      fi
    else
      foreign="1"
    fi
    if [ -n "$stale" ] && takeover "$stale"; then
      continue
    fi
    if [ -n "$foreign" ]; then
      [ "$foreign_noted" = 0 ] && {
        foreign_noted=1
        echo "gate-lock: $lock holds no record this script wrote (an outside driver's lock?)." >&2
        echo "gate-lock: waiting for it; if no gate holds it, remove that path by hand (only then)." >&2
      }
    elif [ "$noted" = 0 ] || { [ $((waited % 30)) -eq 0 ] && [ "$waited" -gt 0 ]; }; then
      noted=1 foreign_noted=0
      echo "gate-lock: waiting ${waited}s for the gate (held by pid ${rec_pid:-unknown})" >&2
    fi
    if [ "$timeout_s" != 0 ] && [ "$waited" -ge "$timeout_s" ]; then
      echo "gate-lock: gave up after ${waited}s waiting for $lock" >&2
      exit 75
    fi
    nap 2
    waited=$((waited + 2))
  done
}

# The command runs in a process group of its own; this prologue publishes the
# group (so it survives this wrapper) and `exec`s the command, so no extra
# process is left behind. `$0` is `_` and "$@" is the command unchanged.
read -r -d '' group_prologue <<'EOP'
printf 'pgid %s\n' "$$" >"$KELIR_GATE_GROUP_FILE" || exit 1
unset KELIR_GATE_GROUP_FILE
exec "$@"
EOP

signal_group() { # <SIG>
  if [ -z "$child_pgid" ] || [ "$child_pgid" = "$$" ] || [ "$child_pgid" = 0 ]; then
    [ -n "$child_pid" ] && kill -"$1" "$child_pid" 2>/dev/null
    return 0
  fi
  kill -"$1" "-$child_pgid" 2>/dev/null
}

group_gone_within() { # <seconds>
  local i=0 limit=$(( $1 * 4 ))
  while group_alive "$child_pgid"; do
    [ "$i" -ge "$limit" ] && return 1
    sleep 0.25
    i=$((i + 1))
  done
  return 0
}

stop_command() { # <SIG> <grace seconds>
  signal_group "$1"
  group_gone_within "$2" && return 0
  signal_group KILL
  group_gone_within 10
}

on_signal() { # <SIG> <exit code>
  trap - EXIT INT TERM
  [ -n "$nap_pid" ] && kill "$nap_pid" 2>/dev/null
  if [ -n "$child_pid" ]; then
    stop_command "$1" 10
    wait "$child_pid" 2>/dev/null
    child_pid=""
  fi
  if group_alive "$child_pgid"; then
    # It survived KILL: keep the lock rather than hand the box to a second gate.
    echo "gate-lock: process group $child_pgid is still alive; leaving the lock in place" >&2
    exit "$2"
  fi
  release
  exit "$2"
}

# From here on a signal must release the lock and leave no child behind.
trap release EXIT
trap 'on_signal INT 130' INT
trap 'on_signal TERM 143' TERM

wait_for_lock

# A background process group must not read the terminal: it would stop on
# SIGTTIN while holding the lock, and the gate would wait for itself.
if [ -t 0 ]; then
  set -m
  KELIR_GATE_GROUP_FILE="$grp_file" bash -c "$group_prologue" _ "$@" </dev/null &
  child_pid=$!
  set +m
else
  set -m
  KELIR_GATE_GROUP_FILE="$grp_file" bash -c "$group_prologue" _ "$@" &
  child_pid=$!
  set +m
fi
child_pgid="$child_pid"

wait "$child_pid"
rc=$?
child_pid=""
release
trap - EXIT INT TERM
exit "$rc"
