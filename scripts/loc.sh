#!/usr/bin/env bash
set -euo pipefail

# Usage: scripts/loc.sh [src tests]. Paths are useful for inspecting a baseline
# worktree without changing the current checkout.
src=${1:-src}
tests=${2:-tests}

for tree in "$src" "$tests"; do
    if [[ ! -d "$tree" ]]; then
        printf '%s: not a directory\n' "$tree" >&2
        exit 1
    fi
done

report() {
    local tree=$1 total code comments inline tests
    total=$(find "$tree" -name '*.rs' -type f -print0 | xargs -0 awk 'END { print NR }')
    read -r code comments inline < <(find "$tree" -name '*.rs' -type f -print0 | xargs -0 awk '
        FNR == 1 { testing = 0 }
        /^[[:space:]]*#\[cfg\(test\)\]/ { testing = 1 }
        testing { inline++; next }
        /^[[:space:]]*$/ { next }
        /^[[:space:]]*\/\// { comments++; next }
        { code++ }
        END { printf "%d %d %d\n", code, comments, inline }
    ')
    tests=$(find "$tree" -name '*.rs' -type f -print0 | xargs -0 grep -hE '^[[:space:]]*#\[(tokio::)?test([]([[:space:]]|$)|(flavor = "multi_thread")\])$' | wc -l)
    printf '%s: total %s; code %s; comments %s; inline tests %s; tests %s\n' \
        "$tree" "$total" "$code" "$comments" "$inline" "$tests"
}

report "$src"
report "$tests"

printf '\nLargest files:\n'
find "$src" "$tests" -name '*.rs' -type f -print0 |
    xargs -0 wc -l | sort -rn | awk 'NR > 1 && NR <= 11 { printf "%8d %s\n", $1, $2 }'

printf '\nMilestone tags in src: '
find "$src" -name '*.rs' -type f -print0 |
    xargs -0 grep -hoE '\b(M[0-9]+[A-Za-z-]*|STEER-[0-9]+[a-z]*|retro)\b' | wc -l

if [[ -f "$src/session.rs" ]] && grep -qE '^(pub )?struct Session\b' "$src/session.rs"; then
    printf '\nSession fields:\n'
    awk '
        /^(pub )?struct Session[[:space:]]*\{/ { inside = 1; next }
        inside && /^\}/ { exit }
        inside && /^[[:space:]]*(pub(\([^)]*\))? )?[A-Za-z_][A-Za-z0-9_]*:/ {
            sub(/^[[:space:]]*/, "")
            print
        }
    ' "$src/session.rs"
fi
