#!/usr/bin/env python3
"""Per-crate line and function coverage from `cargo llvm-cov report --json --summary-only`.

    cargo llvm-cov report --json --summary-only | python3 scripts/coverage_summary.py

Prints a Markdown table, also appended to $GITHUB_STEP_SUMMARY when that is set.
"""

import json
import os
import re
import sys


def main() -> None:
    report = json.load(sys.stdin)
    crates: dict[str, dict[str, int]] = {}
    for data in report["data"]:
        for f in data["files"]:
            m = re.search(r"[/\\]crates[/\\]([^/\\]+)[/\\]", f["filename"])
            if not m:
                continue
            c = crates.setdefault(m.group(1), {"lines": 0, "covered": 0, "fns": 0, "fns_covered": 0})
            s = f["summary"]
            c["lines"] += s["lines"]["count"]
            c["covered"] += s["lines"]["covered"]
            c["fns"] += s["functions"]["count"]
            c["fns_covered"] += s["functions"]["covered"]

    def pct(covered: int, count: int) -> str:
        return f"{100 * covered / count:.1f}%" if count else "-"

    rows = ["| Crate | Lines | Line coverage | Function coverage |", "|---|---:|---:|---:|"]
    total = {"lines": 0, "covered": 0, "fns": 0, "fns_covered": 0}
    for name in sorted(crates):
        c = crates[name]
        for k in total:
            total[k] += c[k]
        rows.append(f"| {name} | {c['lines']} | {pct(c['covered'], c['lines'])} | {pct(c['fns_covered'], c['fns'])} |")
    rows.append(
        f"| **all** | {total['lines']} | **{pct(total['covered'], total['lines'])}** | {pct(total['fns_covered'], total['fns'])} |"
    )
    text = "### Rust coverage\n\n" + "\n".join(rows) + "\n"
    print(text)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as out:
            out.write(text)


if __name__ == "__main__":
    main()
