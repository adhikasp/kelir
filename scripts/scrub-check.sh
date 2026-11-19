#!/usr/bin/env bash
# Fail if any *tracked* file matches a pattern from a file kept outside the repo.
#
# usage: scripts/scrub-check.sh <pattern-file>
#
# The pattern file is one literal pattern per line; blank lines and lines
# starting with `#` are skipped. Matching is case-insensitive and literal (no
# regex: a name is a name). The file lives outside the repo on purpose — a
# committed list of the names to avoid would defeat the check, and this script
# never names them itself.
set -euo pipefail
P="${1:?usage: scrub-check.sh <pattern-file>}"
if [ ! -f "$P" ]; then
  echo "scrub-check: no pattern file: $P" >&2
  exit 2
fi
cd "$(git rev-parse --show-toplevel)"

matched=0
while IFS= read -r pattern || [ -n "$pattern" ]; do
  pattern="${pattern%$'\r'}"
  case "$pattern" in
    '' | '#'*) continue ;;
  esac
  if hits="$(git grep -n -I -i -F -e "$pattern" -- . || true)" && [ -n "$hits" ]; then
    echo "scrub-check: '$pattern' appears in tracked files:" >&2
    printf '%s\n' "$hits" | sed 's/^/  /' >&2
    matched=$((matched + 1))
  fi
done <"$P"

if [ "$matched" -ne 0 ]; then
  echo "scrub-check: $matched pattern(s) matched; this tree is not publishable" >&2
  exit 1
fi
echo "scrub-check: clean ($(git ls-files | wc -l | tr -d ' ') tracked files)"
