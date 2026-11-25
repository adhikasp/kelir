#!/usr/bin/env bash
# Reclaim disk from worktrees nobody is building in: delete their `target/`.
#
# usage: scripts/target-sweep.sh [--repo DIR] [--older-than HOURS] [--min-mb MB] [--yes]
#
# Every job runs in its own worktree, and every worktree pays for its own
# `target/`: about 1.2 GB for a full test build at the project's debug-info
# setting (docs/BUILD.md, *What a target costs*). A finished job's tree is often
# still worth keeping — a review, a bisect, a branch nobody has pushed — so
# `git worktree remove` is the wrong tool: it deletes source. `target/` is only
# a cache, so deleting it loses nothing but compile time, and the worktree stays
# exactly where it was. On the dev box the farm reached 167 GB over 110
# worktrees, nearly all of it `target/` of jobs that had finished.
#
# The candidate set is what git itself lists as a worktree of this repo
# (`git worktree list --porcelain`), never a directory that merely looks like
# one, and inside a candidate only `<worktree>/target`. A target is skipped when
#   * it belongs to the worktree this script runs from, or to the caller's cwd,
#   * git reports the worktree `locked` or `prunable`, or its directory is gone,
#   * `target` is a symlink or not a directory,
#   * `target` has no cargo `CACHEDIR.TAG` with the standard signature (cargo
#     writes one; a directory named `target` that cargo never wrote survives),
#   * it is smaller than `--min-mb` (default 100),
#   * anything inside it was written in the last `--older-than` hours
#     (default 24). This is the only guard against a build that is running
#     right now: run the sweep when the farm is idle. A build that starts in the
#     window between the check and `rm` fails, and fails loudly; it cannot
#     produce a wrong result, because only the cache is gone.
# Nothing is removed without `--yes`; the default is a dry run that prints what
# each candidate would cost. Exit 0 means every removal that was asked for
# happened; a refused `rm` is reported and exits 1.
set -uo pipefail

# The usage text is the file's own header: every line after the shebang, up to
# the first line of code, so it cannot drift out of date.
usage() {
	awk 'NR > 1 && /^set -uo pipefail$/ { exit } NR > 1' "$0"
	exit 2
}
die() {
	echo "target-sweep: $*" >&2
	exit 2
}

repo=""
hours=24
min_mb=100
yes=0
while [ $# -gt 0 ]; do
	case "$1" in
	--repo)
		repo="${2:?--repo needs a value}"
		shift 2
		;;
	--older-than)
		hours="${2:?--older-than needs a value}"
		shift 2
		;;
	--min-mb)
		min_mb="${2:?--min-mb needs a value}"
		shift 2
		;;
	--yes | --force)
		yes=1
		shift
		;;
	--dry-run)
		yes=0
		shift
		;;
	-h | --help) usage ;;
	*) die "unknown argument: $1 (try --help)" ;;
	esac
done
case "$hours" in
'' | *[!0-9]*) die "--older-than must be a whole number of hours, not '$hours'" ;;
esac
case "$min_mb" in
'' | *[!0-9]*) die "--min-mb must be a whole number of megabytes, not '$min_mb'" ;;
esac

script_dir=$(cd "$(dirname "$0")" && pwd -P) || die "cannot resolve the script's own directory"
if [ -z "$repo" ]; then
	repo=$(git -C "$script_dir/.." rev-parse --show-toplevel 2>/dev/null) || die "not inside a git checkout (use --repo DIR)"
fi
repo=$(cd "$repo" 2>/dev/null && pwd -P) || die "--repo is not a directory: ${repo:-}"
git -C "$repo" rev-parse --git-common-dir >/dev/null 2>&1 || die "not a git checkout: $repo"

# The worktree this script runs from is never a candidate: it is the checkout
# whose target/ the caller may be measuring against, and its own build is the
# one most likely to be live. The caller's cwd gets the same protection.
self=$(cd "$script_dir/.." && pwd -P) || exit 2
cwd_top=$(git -C "$(pwd -P)" rev-parse --show-toplevel 2>/dev/null || true)
if [ -n "$cwd_top" ]; then
	cwd_top=$(cd "$cwd_top" && pwd -P)
fi

# `find -newer` wants a file, and the cut-off has to be computed the same way on
# GNU and BSD date (the tests' age_hours helper exists for the same reason).
stamp=$(mktemp 2>/dev/null || mktemp -t target-sweep) || die "cannot create a stamp file"
trap 'rm -f "$stamp"' EXIT
if date -d "now" +%s >/dev/null 2>&1; then
	touch -t "$(date -d "$hours hours ago" +%Y%m%d%H%M.%S)" "$stamp" ||
		die "cannot set the --older-than cut-off"
else
	touch -t "$(date -v-"${hours}"H +%Y%m%d%H%M.%S)" "$stamp" ||
		die "cannot set the --older-than cut-off"
fi

# Cargo's cache tag (https://bford.info/cachedir/). A directory named `target`
# without this line was written by somebody else and is never touched.
tag='Signature: 8a477f597d28d172789f06886806bc55'

worktrees=0
targets=0
stale=0
skipped=0
freed_mb=0
failed=0

say_skip() {
	printf "skip    %6s MB  %s  (%s)\n" "$2" "$1" "$3"
	skipped=$((skipped + 1))
}

flush() {
	[ -n "$path" ] || return 0
	worktrees=$((worktrees + 1))
	if [ "$locked" = 1 ]; then
		say_skip "$path" 0 "locked worktree"
		return
	fi
	if [ "$prunable" = 1 ]; then
		say_skip "$path" 0 "prunable worktree: git already calls it stale"
		return
	fi
	if [ ! -d "$path" ]; then
		say_skip "$path" 0 "worktree directory is gone"
		return
	fi
	if [ "$path" = "$self" ]; then
		say_skip "$path" 0 "the worktree this script runs from"
		return
	fi
	if [ -n "$cwd_top" ] && [ "$path" = "$cwd_top" ]; then
		say_skip "$path" 0 "the caller's worktree"
		return
	fi
	t="$path/target"
	# No target at all is not a skip: nothing was ever built there, and saying so
	# for every one of a hundred trees drowns the lines that matter.
	[ -e "$t" ] || [ -L "$t" ] || return 0
	targets=$((targets + 1))
	if [ -L "$t" ]; then
		say_skip "$t" 0 "symlink, not a directory cargo wrote"
		return
	fi
	if [ ! -d "$t" ]; then
		say_skip "$t" 0 "not a directory"
		return
	fi
	if [ ! -f "$t/CACHEDIR.TAG" ] || [ "$(head -n 1 "$t/CACHEDIR.TAG" 2>/dev/null)" != "$tag" ]; then
		say_skip "$t" 0 "no cargo CACHEDIR.TAG: not a cargo target"
		return
	fi
	mb=$(du -sm "$t" 2>/dev/null | cut -f1)
	mb=${mb:-0}
	if [ "$mb" -lt "$min_mb" ]; then
		say_skip "$t" "$mb" "smaller than --min-mb $min_mb"
		return
	fi
	if [ -n "$(find "$t" -newer "$stamp" -print -quit 2>/dev/null)" ]; then
		say_skip "$t" "$mb" "written in the last ${hours}h"
		return
	fi
	stale=$((stale + 1))
	if [ "$yes" = 1 ]; then
		if rm -rf -- "$t"; then
			freed_mb=$((freed_mb + mb))
			printf "remove  %6s MB  %s\n" "$mb" "$path"
		else
			failed=$((failed + 1))
			printf "failed  %6s MB  %s  (rm refused)\n" "$mb" "$path" >&2
		fi
	else
		freed_mb=$((freed_mb + mb))
		printf "remove  %6s MB  %s  (dry run)\n" "$mb" "$path"
	fi
}

# `git worktree list --porcelain` is one blank-line-terminated block per
# worktree: `worktree <path>`, `HEAD <sha>`, `branch <ref>`/`detached`, and
# optionally `locked [reason]` or `prunable [reason]`.
path=""
locked=0
prunable=0
while IFS= read -r line; do
	case "$line" in
	"worktree "*)
		flush
		path=${line#worktree }
		locked=0
		prunable=0
		;;
	locked*) locked=1 ;;
	prunable*) prunable=1 ;;
	"")
		flush
		path=""
		;;
	esac
done < <(git -C "$repo" worktree list --porcelain 2>/dev/null)
flush

if [ "$yes" = 1 ]; then
	printf "target-sweep: %s worktrees, %s with a target, %s stale, %s removed (%s MB), %s skipped, %s failed\n" \
		"$worktrees" "$targets" "$stale" "$stale" "$freed_mb" "$skipped" "$failed"
else
	printf "target-sweep: %s worktrees, %s with a target, %s stale, %s skipped, %s MB reclaimable (dry run; pass --yes to remove)\n" \
		"$worktrees" "$targets" "$stale" "$skipped" "$freed_mb"
fi
[ "$failed" = 0 ] || exit 1
