#!/usr/bin/env bash
# Doc-drift check for docs/reference/source-map.md: every .rs file under src/
# has exactly one table row, and no row names a file that no longer exists.
# The prose groups files by directory; only the `src/…` rows are machine-checked.
set -euo pipefail
root="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)"
cd "$root"
map=docs/reference/source-map.md

find src -name '*.rs' -type f -print | sort >/tmp/source-map-files.$$
grep -oE '^\| `src/[^`]+`' "$map" | tr -d '|` ' | sort >/tmp/source-map-rows.$$
missing=$(comm -23 /tmp/source-map-files.$$ /tmp/source-map-rows.$$)
stale=$(comm -13 /tmp/source-map-files.$$ /tmp/source-map-rows.$$)
rc=0
if [ -n "$missing" ]; then
  echo "source-map: missing rows:" >&2
  echo "$missing" >&2
  rc=1
fi
if [ -n "$stale" ]; then
  echo "source-map: rows for files that do not exist:" >&2
  echo "$stale" >&2
  rc=1
fi
if [ "$rc" = 0 ]; then
  echo "source-map: ok ($(wc -l </tmp/source-map-files.$$) files)"
fi
rm -f /tmp/source-map-files.$$ /tmp/source-map-rows.$$
exit "$rc"
