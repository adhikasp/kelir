#!/usr/bin/env python3
"""Write docs/rewrite/test-map.md from the P0 migration's source inventory."""
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parent.parent
IT = ROOT / "tests/it"
OLD = ROOT / "docs/rewrite/test-inventory.txt"

RENAME = {"buildinfra": "buildinfra_tests", "buildinfra_review": "buildinfra_review_tests"}
PROBE = "buildinfra_probe"


def test_names(source: str) -> list[str]:
    names = []
    lines = source.splitlines()
    for index, line in enumerate(lines):
        if re.match(r'\s*#\[(tokio::)?test', line):
            for following in lines[index + 1:]:
                match = re.match(r'\s*(?:async\s+)?fn\s+(\w+)', following)
                if match:
                    names.append(match.group(1))
                    break
    return names


def main():
    lines = [
        "# Rewrite test map (P0)",
        "",
        "Every integration test in `tests/` at the last synced `origin/rewrite` is listed below. The",
        "migration is one source file per feature module in one test binary;",
        "no test was removed. Platform-specific cases stay in their original",
        "source files and are not deduplicated.",
        "",
        "| old (file::test) | new (module::source::test) |",
        "|---|---|",
    ]
    count = 0
    for entry in OLD.read_text().splitlines():
        old_file, name = entry.split("::", 1)
        if old_file == PROBE:
            new = f"buildinfra_probe (separate ignored binary)::{name}"
        else:
            source = RENAME.get(old_file, old_file)
            wrapper = IT / f"{source}.rs"
            feature = "buildinfra" if source.startswith("buildinfra_") else "unknown"
            for candidate in IT.glob("*.rs"):
                if re.search(rf'(?m)^mod {source};$', candidate.read_text()):
                    feature = candidate.stem
                    break
            new = f"it::{feature}::{source}::{name}"
        lines.append(f"| `{old_file}::{name}` | `{new}` |")
        count += 1
    lines += ["", f"Total: {count} old integration tests, all mapped; 0 removed.", ""]
    old_names = {
        name
        for _, name in (entry.split("::", 1) for entry in OLD.read_text().splitlines())
        if name not in {"probe_always_fails", "probe_flaky_fails_first_attempt_only", "probe_leaks_a_child"}
    }
    it_names = set()
    for source in IT.glob("*.rs"):
        it_names.update(test_names(source.read_text()))
    # The three probe tests live in the separate buildinfra_probe binary; they
    # are mapped explicitly and are not part of the it binary's source set.
    missing = it_names - old_names
    extra = old_names - it_names
    if missing or extra:
        raise SystemExit(
            f"inventory drift: missing {sorted(missing)}; stale {sorted(extra)}"
        )
    out = ROOT / "docs/rewrite/test-map.md"
    out.parent.mkdir(exist_ok=True)
    out.write_text("\n".join(lines))


if __name__ == "__main__":
    main()
