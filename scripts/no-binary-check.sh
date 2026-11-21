#!/usr/bin/env bash
# Fail if any image/binary file would be added to the repository.
#
# usage: scripts/no-binary-check.sh [staged|range <base> <head>]
#   staged (default)  check files staged for commit
#   range <base> <head>  check files added between two refs (used in CI)
#
# Screenshots and other binaries do not belong in git: they bloat history and
# cannot be scrubbed of private data. Put images in the PR description with
# `gh pr edit --attach ./image.png` instead.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
mode="${1:-staged}"
case "$mode" in
  staged)
    files="$(git diff --cached --name-only --diff-filter=A)"
    ;;
  range)
    base="${2:?usage: no-binary-check.sh range <base> <head>}"
    head="${3:?usage: no-binary-check.sh range <base> <head>}"
    files="$(git diff --name-only --diff-filter=A "$base" "$head")"
    ;;
  *)
    echo "no-binary-check: unknown mode '$mode'" >&2
    exit 2
    ;;
esac
if [ -z "$files" ]; then
  echo "no-binary-check: clean (no added files)"
  exit 0
fi
# Check both extension and actual content, so a binary disguised with a
# .txt extension is still caught.
bad=0
while IFS= read -r f; do
  [ -z "$f" ] && continue
  ext="$(printf '%s' "${f##*.}" | tr '[:upper:]' '[:lower:]')"
  case "$ext" in
    png|jpg|jpeg|gif|webp|bmp|ico|pdf|zip|gz|tar|bin|exe|dll|so|dylib|woff|woff2|ttf|otf|mp4|mov|avi|mp3|wav)
      echo "no-binary-check: binary file added: $f" >&2
      bad=1
      continue
      ;;
  esac
  if [ -f "$f" ] && ! LC_ALL=C grep -qI . "$f"; then
    echo "no-binary-check: binary content in added file: $f" >&2
    bad=1
  fi
done <<<"$files"
if [ "$bad" -ne 0 ]; then
  echo "no-binary-check: images/binaries must not be committed. Upload them with gh pr edit --attach instead." >&2
  exit 1
fi
echo "no-binary-check: clean"
