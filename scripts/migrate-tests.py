#!/usr/bin/env python3
"""Move P0 integration tests into one binary, filed by feature.

Each old test source belongs to exactly one feature module. Sources are copied
unchanged except for replacing the old local `mod support;` with an alias to
the feature's one support module.
"""
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parent.parent
TESTS = ROOT / "tests"
IT = TESTS / "it"

OWNER = {
    "e2e": "loop_", "e2e_steer6": "loop_", "review_steer6": "loop_", "review_steer6b": "loop_",
    "e2e_m3": "resume", "review_m3": "resume", "m8fix": "resume", "review_m8": "resume",
    "e2e_bashdesc": "tools", "e2e_bashdesc_adversarial": "tools",
    "review_m4": "tools", "review_m4b": "tools", "e2e_m4": "bg", "review_m1": "verbs",
    "e2e_verbs": "verbs", "steer7fix4": "verbs", "e2e_m7": "children", "review_m11": "children",
    "e2e_m6": "mcp", "review_m6": "mcp", "review_m6b": "mcp", "e2e_m10": "channel",
    "review_m10": "channel", "review_m10fix": "channel", "e2e_m8": "gates",
    "e2e_m5b": "hint", "review_m5b": "hint", "steer12": "hint", "review_steer12": "hint",
    "e2e_m9": "jobs", "review_m9": "jobs", "review_m9fix": "jobs", "e2e_m11": "notify",
    "e2e_m5": "sources", "review_m5a": "sources", "e2e_tui2": "attachments",
    "review_tui2": "attachments", "e2e_tui": "tui", "review_tui2b": "tui", "review_tui3": "tui",
    "review_m0": "providers", "review_steer1": "providers", "e2e_steer2": "providers",
    "review_steer2": "providers", "e2e_steer4": "providers", "e2e_steer5": "providers",
    "review_steer5": "providers", "e2e_steer8": "providers", "review_steer8": "providers",
    "e2e_steer11": "cli", "review_m2": "cli", "e2e_steer7": "platform",
    "review_steer7": "platform", "review_steer7b": "platform", "review_steer7c": "platform",
    "review_steer7d": "platform", "e2e_steer7sig": "platform", "review_steer4": "platform",
    "buildinfra": "buildinfra", "buildinfra_review": "buildinfra",
}
RENAME = {"buildinfra": "buildinfra_tests", "buildinfra_review": "buildinfra_review_tests"}
PROBE = "buildinfra_probe"
FEATURES = sorted(set(OWNER.values()))


def main():
    IT.mkdir(exist_ok=True)
    support = IT / "support"
    support.mkdir(exist_ok=True)
    (support / "mod.rs").write_bytes((TESTS / "support" / "mod.rs").read_bytes())
    for child in (TESTS / "support").iterdir():
        if child.name != "mod.rs":
            (support / child.name).write_bytes(child.read_bytes())

    for original, feature in OWNER.items():
        stem = RENAME.get(original, original)
        text = (TESTS / f"{original}.rs").read_text()
        text = re.sub(r'(?m)^#\[path = "[^"]*support/mod\.rs"\]\s*\n', '', text)
        text = re.sub(r'(?m)^mod support;\s*\n', '', text)
        if stem.startswith("buildinfra"):
            alias = 'use crate::support;\n\n'
        else:
            alias = 'use super::support;\n\n'
        lines = text.splitlines(True)
        cursor = 0
        while cursor < len(lines) and lines[cursor].startswith('//!'):
            cursor += 1
        if cursor < len(lines) and lines[cursor].strip() == '':
            cursor += 1
        lines.insert(cursor, alias)
        (IT / f"{stem}.rs").write_text(''.join(lines))

    main_lines = [
        "//! The rewrite's one integration-test binary, one source file per feature.",
        "//!",
        "//! Every migrated source keeps its old file name as a nested module, so",
        "//! `docs/rewrite/test-map.md` maps each old test to its new path exactly.",
        "",
    ]
    if "buildinfra" not in FEATURES:
        main_lines += ['#[path = "support/mod.rs"]', "mod support;", ""]
    else:
        main_lines += ['#[path = "support/mod.rs"]', "mod support;", ""]
    for feature in FEATURES:
        main_lines += [f"mod {feature};", ""]
    (IT / "main.rs").write_text("\n".join(main_lines))

    for feature in FEATURES:
        lines = [f"//! Integration tests filed by feature: `{feature}`.", ""]
        lines += ['#[path = "support/mod.rs"]', "mod support;", ""]
        for original, owner in sorted(OWNER.items()):
            if owner != feature:
                continue
            stem = RENAME.get(original, original)
            lines += [f'#[path = "{stem}.rs"]', f"mod {stem};", ""]
        (IT / f"{feature}.rs").write_text("\n".join(lines))

    probe = TESTS / f"{PROBE}.rs"
    text = probe.read_text()
    text = re.sub(r'(?m)^#\[path = "[^"]*support/mod\.rs"\]\s*\n', '', text)
    text = re.sub(r'(?m)^mod support;\s*\n', '', text)
    lines = text.splitlines(True)
    cursor = 0
    while cursor < len(lines) and lines[cursor].startswith('//!'):
        cursor += 1
    if cursor < len(lines) and lines[cursor].strip() == '':
        cursor += 1
    lines.insert(cursor, '#[path = "it/support/mod.rs"]\nmod support;\n\n')
    probe.write_text(''.join(lines))

    for original in OWNER:
        (TESTS / f"{original}.rs").unlink()
    for child in list((TESTS / "support").iterdir()):
        child.unlink()
    (TESTS / "support").rmdir()


if __name__ == "__main__":
    main()
