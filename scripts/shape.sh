#!/usr/bin/env bash
# Shape gate: the rules that keep the rewrite's structure from growing back.
# Called by scripts/gate.sh (full gate and --quick) and by CI, and runnable on
# its own with `bash scripts/shape.sh [--self-test]`.
#
# Rules (issue #20 §2):
#   1. each src/ file is <= 800 lines up to the first #[cfg(test)], unless
#      listed in scripts/shape-allow.txt with a reason;
#   2. no milestone/review tag in src/;
#   3. struct Session has <= 15 fields;
#   4. Command::new only in src/sys/runner.rs and the bg shim;
#   5. #![feature(..)] only in src/lib.rs and only from #23's Adopt list;
#   6. no rust-version in Cargo.toml;
#   7. CI's toolchain matches rust-toolchain.toml (unsuffixed; the whole pin
#      lives in the file, not duplicated in the workflow).
#
# --self-test seeds each violation in isolation in a temporary copy of the
# tree (copying only the files the checks read), asserts that the specific
# rule's failure message appears, then that the clean tree passes. It never
# writes into the checkout.
set -uo pipefail

root="$(git -C "$(dirname "$0")/.." rev-parse --show-toplevel)" || exit 2
cd "$root" || exit 2
src_dir=src
allow=scripts/shape-allow.txt
failures=0
verbose=1

note() { [ "$verbose" = 1 ] && echo "shape: $*"; }
bad() { echo "shape: FAIL — $*" >&2; failures=$((failures + 1)); }
good() { note "$*"; }

# Allow-list: path: reason. Loaded once; a bare path is an error.
declare -A allowed=()
load_allow() {
  local file="$1" line path reason
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in ''|\#*) continue ;; esac
    path="${line%%:*}"
    reason="${line#*:}"
    if [ "$path" = "$line" ] || [ -z "${reason// /}" ]; then
      bad "$file: expected '<path>: <reason>', got: $line"
      continue
    fi
    allowed["${path#./}"]=1
  done <"$file"
}
load_allow "$allow"

# 1. File length, counting from the top to the first #[cfg(test)].
check_lengths() {
  local f n
  while IFS= read -r f; do
    n=$(awk '/^[[:space:]]*#\[cfg\(test\)\]/{exit} END{print NR}' "$f")
    if [ "$n" -gt 800 ] && [ -z "${allowed[$f]:-}" ]; then
      bad "$f has $n lines before #[cfg(test)] (max 800; reason needed in $allow)"
    fi
  done < <(find "$src_dir" -name '*.rs' -type f -print | sort)
  good "file lengths <= 800"
}

# 2. Milestone and review tags. The word boundaries match the plan's pattern.
check_tags() {
  local hits
  hits=$(grep -rEn '\b(M[0-9]+[A-Za-z-]*|STEER-[0-9]+[a-z]*|retro)\b' "$src_dir" --include='*.rs' || true)
  if [ -n "$hits" ]; then
    bad "milestone/review tags in src/: $hits"
  fi
  good "no milestone/review tags"
}

# 3. Session field count. Walks the struct body; no dependency on its file.
check_session() {
  local n where
  where=$(grep -rlE '^(pub(\([^)]*\))? )?struct Session[[:space:]]*\{' "$src_dir" --include='*.rs' | head -1)
  if [ -z "$where" ]; then
    bad "struct Session not found in $src_dir"
    return
  fi
  n=$(awk '
    /^(pub(\([^)]*\))? )?struct Session[[:space:]]*\{/ { inside=1; next }
    inside && /^}/ { exit }
    inside && /^[[:space:]]*(pub(\([^)]*\))? )?[A-Za-z_][A-Za-z0-9_]*:/ { count++ }
    END { print count+0 }
  ' "$where")
  if [ "$n" -gt 15 ]; then
    bad "struct Session in $where has $n fields (max 15)"
  fi
  good "struct Session has $n fields"
}

# 4. One child runner. The shim may start the detached process it owns.
check_commands() {
  local hits
  hits=$(grep -rn 'Command::new' "$src_dir" --include='*.rs' \
    | grep -v "^$src_dir/sys/runner.rs:" \
    | grep -v "^$src_dir/features/bg/shim.rs:" || true)
  if [ -n "$hits" ]; then
    bad "Command::new outside sys/runner.rs and the bg shim: $hits"
  fi
  good "Command::new confined to the runner and shim"
}

# 5. Feature allow-list, from issue #23's two Adopt tables.
feature_allowed() {
  case "$1" in
    # unstable language features (issue #23 §4 Adopt)
    type_info|default_field_values|try_blocks|try_blocks_heterogeneous|gen_blocks|iter_macro|\
    return_type_notation|impl_trait_in_assoc_type|type_alias_impl_trait|async_trait_bounds|\
    deref_patterns|postfix_match|super_let|exhaustive_patterns|trait_alias|\
    derive_from|macro_metavar_expr|macro_metavar_expr_concat|macro_derive|macro_attr|decl_macro|\
    impl_restriction|mut_restriction|final_associated_functions|must_not_suspend|new_range|\
    type_changing_struct_update|import_trait_associated_functions|stmt_expr_attributes|\
    closure_lifetime_binder|associated_type_defaults|named_fn_trait_parameters|\
    impl_trait_in_bindings|async_fn_in_trait|return_position_impl_trait_in_trait|\
    async_closure|let_chains|if_let_guard|never_type|cfg_select|optimize_attribute|\
    lint_reasons|exclusive_range_pattern|inline_const|associated_type_bounds|\
    precise_capturing|precise_capturing_in_traits|raw_ref_op|unsafe_attributes|\
    unsafe_extern_blocks|min_exhaustive_patterns|diagnostic_namespace|do_not_recommend|\
    generic_arg_infer|impl_trait_projections|cfg_boolean_literals|c_str_literals|\
    if_let_rescope|shorter_tail_lifetimes|expr_fragment_specifier_2024|\
    windows_process_extensions_raw_attribute|windows_process_extensions_force_quotes|\
    process_setsid|unix_send_signal|unix_kill_process_group|exit_status_error|fs_home_dirs|\
    random|rename_noreplace|nonpoison_mutex|once_cell_try|duration_constructors|\
    normalize_lexically|error_reporter|iter_intersperse|str_split_remainder|\
    str_lines_remainder|vec_peek_mut|hash_map_macro|file_buffered) return 0 ;;
  esac
  return 1
}
check_features() {
  local file line names name
  while IFS=: read -r file line; do
    [ -n "$file" ] || continue
    if [ "$file" != "$src_dir/lib.rs" ]; then
      bad "$file:$line: #![feature] outside src/lib.rs"
    fi
  done < <(grep -rn '#!\[feature' "$src_dir" --include='*.rs' | cut -d: -f1,2 || true)
  # Parse every feature name on the #![feature(..)] lines in lib.rs.
  while IFS= read -r line; do
    names="${line#*#\![feature(}"
    names="${names%%)*}"
    while IFS= read -r name; do
      name="$(echo "$name" | tr -d '[:space:]')"
      [ -n "$name" ] || continue
      if ! feature_allowed "$name"; then
        bad "src/lib.rs: feature '$name' is not on #23's Adopt list"
      fi
    done < <(echo "$names" | tr ',' '\n')
  done < <(grep '#!\[feature(' "$src_dir/lib.rs" || true)
  good "features only in src/lib.rs, all on the Adopt list"
}

# 6. No compatibility floor: the pinned toolchain is the only floor.
check_rust_version() {
  if grep -qE '^[[:space:]]*rust-version[[:space:]]*=' Cargo.toml; then
    bad "rust-version reappeared in Cargo.toml (rust-toolchain.toml is the pin)"
  fi
  good "no rust-version in Cargo.toml"
}

# 7. CI installs the pin from rust-toolchain.toml, with no other toolchain.
check_ci_toolchain() {
  local pin workflow named explicit
  pin="$(awk -F'"' '/^channel[[:space:]]*=/ {print $2; exit}' rust-toolchain.toml)"
  if [ -z "$pin" ]; then
    bad "rust-toolchain.toml has no channel"
    return
  fi
  while IFS= read -r workflow; do
    named="$(grep -nE 'toolchain:[[:space:]]*[^[:space:]]+' "$workflow" || true)"
    if [ -n "$named" ]; then
      # An unsuffixed toolchain equal to the pin is fine. So is an expression
      # that reads the pin from rust-toolchain.toml in the same workflow: the
      # file remains the only place that spells a channel.
      explicit="$(echo "$named" | grep -vF "toolchain: $pin" \
        | grep -vF 'toolchain: ${{ steps.toolchain.outputs.pin }}' || true)"
      if [ -n "$explicit" ]; then
        bad "$workflow names a toolchain other than rust-toolchain.toml: $explicit"
      fi
    fi
  done < <(find .github/workflows -type f \( -name '*.yml' -o -name '*.yaml' \) -print | sort)
  good "CI toolchain matches rust-toolchain.toml ($pin)"
}

# 8. The generated source map has one row per src file.
check_source_map() {
  [ -f docs/reference/source-map.md ] || return 0
  if ! bash "$PWD/scripts/source-map-check.sh" >/dev/null; then
    bad "docs/reference/source-map.md is out of date (run scripts/source-map-check.sh)"
  fi
  good "source map covers every src file"
}

check_all() {
  failures=0
  check_lengths
  check_tags
  check_session
  check_commands
  check_features
  check_rust_version
  check_ci_toolchain
  check_source_map
  return "$failures"
}

# Mutation self-test: copy just the inputs the checks read into a scratch
# tree, then restore that pristine snapshot before every probe. Each probe
# seeds one violation and must make `check_all` fail with the message of THAT
# rule, so disabling any single rule fails its probe. The clean tree must pass
# at the end. No writes to the checkout.
self_test() {
  local tmp pristine check seed message output rc=0
  tmp="$(mktemp -d)" || exit 2
  trap 'rm -rf "$tmp"' EXIT
  mkdir -p "$tmp/src/sys" "$tmp/src/tui" "$tmp/scripts" "$tmp/.github/workflows"
  cp "$src_dir/lib.rs" "$tmp/src/lib.rs"
  cp "$src_dir/core/session/types.rs" "$tmp/src/types.rs"
  cp "$src_dir/sys/runner.rs" "$tmp/src/sys/runner.rs"
  cp "$src_dir/tui/editor.rs" "$tmp/src/tui/editor.rs"
  cp "$allow" "$tmp/scripts/shape-allow.txt"
  cp Cargo.toml rust-toolchain.toml "$tmp/"
  cp .github/workflows/ci.yml "$tmp/.github/workflows/ci.yml"
  pristine="$tmp/pristine"
  mkdir -p "$pristine"
  cp -R "$tmp/src" "$tmp/scripts" "$tmp/.github" "$tmp/Cargo.toml" \
    "$tmp/rust-toolchain.toml" "$pristine/"

  reset_tree() {
    rm -rf "$tmp/src" "$tmp/scripts" "$tmp/.github" "$tmp/Cargo.toml" \
      "$tmp/rust-toolchain.toml"
    cp -R "$pristine/src" "$pristine/scripts" "$pristine/.github" \
      "$pristine/Cargo.toml" "$pristine/rust-toolchain.toml" "$tmp/"
  }

  local status
  seed_and_expect_fail() {
    check="$1" seed="$2" message="$3"
    reset_tree
    output="$( cd "$tmp" && "$seed" >/dev/null 2>&1 && check_all 2>&1 )"
    status=$?
    if [ "$status" -eq 0 ] || ! grep -Fq "$message" <<<"$output"; then
      echo "self-test: $check did not report '$message'" >&2
      echo "$output" | sed 's/^/self-test:   /' >&2
      rc=1
    else
      echo "self-test: $check correctly failed on its seeded violation"
    fi
  }

  seed_length() {
    awk '{ if ($0 == "#[cfg(test)]") { for (i = 0; i < 805; i++) print "// seeded line" } print }' \
      "$pristine/src/tui/editor.rs" >"$tmp/src/tui/editor.rs"
  }
  seed_tag() { echo '// STEER-9 seeded violation' >>"$tmp/src/tui/editor.rs"; }
  seed_session() {
    # Insert inside the Session struct, before its closing brace, so awk counts
    # the extra fields in that struct rather than in a later one.
    awk '/^}/ && inside { for (i = 1; i <= 16; i++) print "    pub(crate) extra_" i ": u8," } { print } \
      /struct Session[[:space:]]*\{/ { inside=1 }' \
      "$pristine/src/types.rs" >"$tmp/src/types.rs"
  }
  seed_command() { echo 'fn f() { let _ = Command::new("sh"); }' >>"$tmp/src/tui/editor.rs"; }
  # `never_patterns` is on #23's Watch list, not its Adopt list (R2): the
  # rule must reject it, and this seed proves the name check is live.
  seed_feature() { echo '#![feature(never_patterns)]' >"$tmp/src/lib.rs"; }
  seed_feature_place() { echo '#![feature(random)]' >"$tmp/src/tui/editor.rs"; }
  seed_rust_version() { echo 'rust-version = "1.88"' >>"$tmp/Cargo.toml"; }
  seed_ci() { echo '          toolchain: stable' >>"$tmp/.github/workflows/ci.yml"; }

  seed_and_expect_fail length seed_length 'lines before #[cfg(test)]'
  seed_and_expect_fail tags seed_tag 'milestone/review tags'
  seed_and_expect_fail session-fields seed_session 'has 31 fields'
  seed_and_expect_fail command-new seed_command 'Command::new outside'
  seed_and_expect_fail feature-name seed_feature "feature 'never_patterns' is not on"
  seed_and_expect_fail feature-place seed_feature_place '#![feature] outside'
  seed_and_expect_fail rust-version seed_rust_version 'rust-version reappeared'
  seed_and_expect_fail ci-toolchain seed_ci 'names a toolchain other than'

  # The clean tree (same snapshot every rule starts from) must be accepted.
  reset_tree
  if ! ( cd "$tmp" && check_all >/dev/null 2>&1 ); then
    echo "self-test: ERROR — clean tree failed" >&2
    rc=1
  fi
  [ "$rc" = 0 ] && echo "self-test: PASS (8 isolated seeds, each rule's own failure asserted; clean tree accepted)"
  rm -rf "$tmp"
  trap - EXIT
  return "$rc"
}

case "${1:-}" in
  --self-test) self_test ;;
  '') check_all ;;
  -h|--help) sed -n '2,22p' "$0" ;;
  *) echo "shape: unknown argument: $1" >&2; exit 2 ;;
esac
