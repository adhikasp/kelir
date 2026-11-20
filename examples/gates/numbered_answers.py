#!/usr/bin/env python3
"""prompt_submit gate: resolve "do 1,4" against the last numbered list.

When a prompt is only a short instruction plus item numbers ("do 1,4",
"go with 2 and 3", "1-3"), the gate finds the last numbered list in the
assistant's most recent message (it reads the session log, whose path is in the
event) and rewrites the prompt to quote those items. Anything else is allowed
untouched.

    [gates]
    prompt_submit = "python /path/to/numbered_answers.py"
"""
import json
import re
import sys

SHORT = re.compile(
    r"^\s*(?:(?:do|go with|take|pick|yes to|ok(?:ay)?|apply|implement|fix)\s+)?"
    r"(?P<nums>\d+(?:\s*(?:,|and|&|-|to)\s*\d+)*)\s*[.!]?\s*$",
    re.IGNORECASE,
)
ITEM = re.compile(r"^\s*(\d+)[.)]\s+(.*\S)\s*$")


def numbers(spec: str) -> list:
    out = []
    for part in re.split(r"\s*(?:,|and|&)\s*", spec.strip(), flags=re.IGNORECASE):
        span = re.split(r"\s*(?:-|to)\s*", part, flags=re.IGNORECASE)
        if len(span) == 2 and span[0].isdigit() and span[1].isdigit():
            lo, hi = int(span[0]), int(span[1])
            out.extend(range(lo, min(hi, lo + 30) + 1))
        elif part.strip().isdigit():
            out.append(int(part))
    return out


def last_list(log_path: str) -> dict:
    items = {}
    try:
        with open(log_path, encoding="utf-8") as handle:
            for line in handle:
                try:
                    event = json.loads(line)
                except ValueError:
                    continue
                if event.get("type") != "assistant":
                    continue
                text = event.get("text")
                if isinstance(text, dict):  # a spilled payload: skip
                    continue
                found = {}
                for row in (text or "").splitlines():
                    match = ITEM.match(row)
                    if match:
                        found[int(match.group(1))] = match.group(2)
                if found:
                    items = found
    except OSError:
        return {}
    return items


def main() -> None:
    event = json.load(sys.stdin)
    match = SHORT.match(event.get("text") or "")
    if not match:
        return print(json.dumps({"decision": "allow"}))
    items = last_list(event.get("log", ""))
    wanted = numbers(match.group("nums"))
    chosen = [(n, items[n]) for n in wanted if n in items]
    if not chosen or len(chosen) != len(wanted):
        return print(json.dumps({"decision": "allow"}))
    body = "\n".join(f"{n}. {text}" for n, text in chosen)
    text = f"{event['text'].strip()}\n\nThe items I mean, from your last numbered list:\n{body}"
    print(json.dumps({"decision": "rewrite", "reason": "resolved item numbers", "input": {"text": text}}))


if __name__ == "__main__":
    main()
