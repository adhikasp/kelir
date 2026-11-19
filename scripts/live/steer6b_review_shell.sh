#!/usr/bin/env bash
# STEER-6 round-2 reviewer live drive (no model): the release binary's shell route, via `kelir bg run`.
# Usage: steer6b_review_shell.sh <scratch-dir> [shell-json-array]   e.g. '["cmd.exe","/C"]'
set -u
S="$1"; SHELLCFG="${2:-}"
D="$(cd "$(dirname "$0")" && pwd)"
BIN="${KELIR_BIN:-$D/../../target/release/kelir.exe}"
rm -rf "$S/state" "$S/work" "$S/cfg"; mkdir -p "$S/work" "$S/cfg"
export KELIR_STATE_DIR="$S/state"
if [ -n "$SHELLCFG" ]; then printf 'shell = %s\n' "$SHELLCFG" > "$S/cfg/config.toml"; else : > "$S/cfg/config.toml"; fi
export KELIR_CONFIG="$S/cfg/config.toml"
cd "$S/work" || exit 1
run() { # label, command
  local id out
  id=$("$BIN" bg run "$2" 2>"$S/err-$1.txt" | head -c 200)
  echo "[$1] started: $id"
  "$BIN" bg wait ${id%% *} >/dev/null 2>&1 || true
}
# 1. backslash runs 1..9000 through a quoted heredoc (bash only)
if [ -z "$SHELLCFG" ]; then
  for n in 1 2 3 4 5 7 8 9 16 100 1000 5999 6001 9000; do
    run "bs$n" "cat > b$n.txt <<'KELIR_EOF'
$(printf '\%.0s' $(seq 1 $n))
KELIR_EOF"
    sz=$(wc -c < b$n.txt); echo "bs $n -> $((sz-1)) bytes (want $n)"
  done
  run unicode "printf 'é日本語🎉 \\x\n' > u.txt"; echo "unicode: $(cat u.txt)"
  run exit7 "exit 7"; "$BIN" bg ls 2>&1 | tail -3
fi
# 2. cmd.exe / PowerShell: a short and a 20 KB command, exit code, output as the job recorded it
if [ -n "$SHELLCFG" ]; then
  case "$SHELLCFG" in
    *cmd.exe*) SHORT='echo short-ran> short.txt & echo hello-out'; LONGHEAD='rem '; LONGTAIL=$'\r\necho long-ran> long.txt'; EXIT='exit /b 7' ;;
    *) SHORT="Set-Content short.txt short-ran; 'hello-out'"; LONGHEAD='# '; LONGTAIL=$'\nSet-Content long.txt long-ran'; EXIT='exit 7' ;;
  esac
  run short "$SHORT"
  PAD=$(head -c 20000 /dev/zero | tr '\0' 'x')
  run long "$LONGHEAD$PAD$LONGTAIL"
  run exit "$EXIT"
  echo "short.txt: $(cat short.txt 2>&1 | tr -d '\r')"; echo "long.txt: $(cat long.txt 2>&1 | tr -d '\r')"
  "$BIN" bg ls 2>&1 | cut -c1-120
  for f in "$S"/state/bg/*.out; do echo "--- $(basename $f)"; head -c 300 "$f" | tr -d '\r'; done
fi
