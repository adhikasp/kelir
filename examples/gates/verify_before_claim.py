#!/usr/bin/env python3
"""turn_end gate: do not accept a hedged final answer when nothing was checked.

Reads the event on stdin. If the final answer hedges ("probably", "I think",
"from memory", ...) and no tool ran this turn, it denies once: the model is sent
back with the reason as a message. kelir blocks at most once per turn, and this
script also allows when the event says the turn was already blocked.

Enable it (nothing is on by default):

    [gates]
    turn_end = "python /path/to/verify_before_claim.py"
"""
import json
import re
import sys

HEDGES = [
    r"\bprobably\b",
    r"\bi think\b",
    r"\bi believe\b",
    r"\bi guess\b",
    r"\bfrom memory\b",
    r"\bmost likely\b",
    r"\bif i remember\b",
    r"\bnot (?:entirely )?sure\b",
]


def main() -> None:
    event = json.load(sys.stdin)
    text = event.get("final_text") or ""
    if event.get("tool_calls", 0) > 0 or event.get("blocked"):
        print(json.dumps({"decision": "allow"}))
        return
    found = [h for h in HEDGES if re.search(h, text, re.IGNORECASE)]
    if not found:
        print(json.dumps({"decision": "allow"}))
        return
    print(
        json.dumps(
            {
                "decision": "deny",
                "reason": (
                    "your answer hedges (probably / I think / from memory) and "
                    "no command ran this turn. Run one check (read the file, run "
                    "the test, query the thing) and answer from what it shows; "
                    "if you cannot check, say plainly that you did not verify."
                ),
            }
        )
    )


if __name__ == "__main__":
    main()
