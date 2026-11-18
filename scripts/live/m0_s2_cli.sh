#!/usr/bin/env bash
# Live scenario 2: the M0 task in miniature. A tiny Python CLI + test in a scratch git repo;
# kelir adds a flag and a test and makes the suite pass.
source "$(dirname "$0")/m0_common.sh" s2
git init -q . && git config user.email t@example.com && git config user.name t
cat > greet.py <<'PY'
import argparse


def greet(name, punctuation="!"):
    return f"Hello, {name}{punctuation}"


def main(argv=None):
    parser = argparse.ArgumentParser(description="Greet someone")
    parser.add_argument("name")
    parser.add_argument("--punctuation", default="!")
    args = parser.parse_args(argv)
    print(greet(args.name, args.punctuation))


if __name__ == "__main__":
    main()
PY
cat > test_greet.py <<'PY'
import greet


def test_default():
    assert greet.greet("Ann") == "Hello, Ann!"


def test_punctuation():
    assert greet.greet("Ann", "?") == "Hello, Ann?"
PY
git add -A && git commit -qm init
"$KELIR_BIN" run --json --provider openrouter --model "$MODEL" \
  "This is a small Python CLI (greet.py) with tests in test_greet.py. Add a --shout flag to the CLI that upper-cases the whole greeting, add a test for it in test_greet.py (put the logic in a function so it is testable), and make the whole suite pass with python -m pytest (install nothing; if pytest is missing use python -m unittest style instead). Show me the final git diff stat." > out.ndjson
echo "exit=$?"
jq -c 'select(.type=="usage")|{step,prompt_tokens,completion_tokens,cached_tokens,cache_write_tokens,cost}' out.ndjson
jq -c 'select(.type=="result")|{steps,outcome}' out.ndjson
jq -s '[.[]|select(.type=="usage")|.cost]|add' out.ndjson
git status --short; python -m pytest -q 2>&1 | tail -3
