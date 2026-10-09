"""Summarize criterion results as markdown tables, one per benchmark group.

Usage: python3 bench/summarize.py [target/criterion]

Reports the median time per iteration with its 95% confidence interval,
and each entry's ratio to oxtel's median in the same group (above 1 means
slower than oxtel).
"""

import json
import sys
from pathlib import Path


def fmt(ns: float) -> str:
    if ns >= 1_000_000:
        return f"{ns / 1_000_000:.2f} ms"
    if ns >= 1_000:
        return f"{ns / 1_000:.2f} µs"
    return f"{ns:.1f} ns"


def main(root: Path) -> None:
    groups: dict[str, list[tuple[str, dict]]] = {}
    for bench in sorted(root.glob("*/*/new/benchmark.json")):
        meta = json.loads(bench.read_text())
        est = json.loads((bench.parent / "estimates.json").read_text())["median"]
        groups.setdefault(meta["group_id"], []).append((meta["function_id"], est))
    for group, rows in groups.items():
        base = dict(rows).get("oxtel", {}).get("point_estimate")
        print(f"### {group}\n")
        print("| | Median | 95% CI | vs oxtel |")
        print("|---|---|---|---|")
        for name, est in rows:
            ci = est["confidence_interval"]
            ratio = f"{est['point_estimate'] / base:.2f}×" if base else ""
            print(
                f"| {name} | {fmt(est['point_estimate'])} "
                f"| {fmt(ci['lower_bound'])} – {fmt(ci['upper_bound'])} | {ratio} |"
            )
        print()


if __name__ == "__main__":
    main(Path(sys.argv[1] if len(sys.argv) > 1 else "target/criterion"))
