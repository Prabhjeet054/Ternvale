#!/usr/bin/env python3
"""Diff two Step 62 bench-results JSON files. Exit 1 on regressions.

Higher-is-better metrics regress when they fall by more than --threshold
percent. Lower-is-better metrics (latencies) regress when they rise.
"""

from __future__ import annotations

import argparse
import json
import sys
from typing import Any, Iterable

LOWER_BETTER = frozenset(
    {
        "boot_to_login_ms",
        "mmio_dispatch_ns",
    }
)

SKIP_SUFFIXES = ("_threads", "_error")
SKIP_KEYS = frozenset({"nproc", "error", "skipped", "notes"})


def is_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def walk(prefix: str, node: Any) -> Iterable[tuple[str, float]]:
    if is_number(node):
        yield prefix, float(node)
        return
    if isinstance(node, dict):
        for key, child in node.items():
            path = f"{prefix}.{key}" if prefix else key
            yield from walk(path, child)


def leaf_name(path: str) -> str:
    return path.rsplit(".", 1)[-1]


def skip_path(path: str) -> bool:
    name = leaf_name(path)
    if name in SKIP_KEYS:
        return True
    return any(name.endswith(suffix) for suffix in SKIP_SUFFIXES)


def lower_better(path: str) -> bool:
    name = leaf_name(path)
    return name in LOWER_BETTER or name.endswith("_ns")


def load(path: str) -> dict[str, Any]:
    with open(path, encoding="utf-8") as handle:
        data = json.load(handle)
    if not isinstance(data, dict):
        raise SystemExit(f"{path}: expected a JSON object")
    return data


def compare(old: dict[str, Any], new: dict[str, Any], threshold: float) -> list[str]:
    old_map = {path: value for path, value in walk("", old) if not skip_path(path)}
    new_map = {path: value for path, value in walk("", new) if not skip_path(path)}
    failures: list[str] = []
    for path in sorted(set(old_map) & set(new_map)):
        before = old_map[path]
        after = new_map[path]
        if before == 0:
            if after == 0:
                delta_pct = 0.0
            else:
                delta_pct = float("inf") if after > 0 else float("-inf")
        else:
            delta_pct = (after - before) * 100.0 / before
        worse = delta_pct > threshold if lower_better(path) else delta_pct < -threshold
        mark = "REGRESS" if worse else "ok"
        print(
            f"{mark:7} {path}: {before:g} -> {after:g} ({delta_pct:+.2f}%)"
        )
        if worse:
            failures.append(
                f"{path} changed {delta_pct:+.2f}% (threshold {threshold:g}%)"
            )
    missing = sorted(set(old_map) - set(new_map))
    for path in missing:
        print(f"skip    {path}: present in baseline only")
    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("old", help="baseline JSON")
    parser.add_argument("new", help="candidate JSON")
    parser.add_argument(
        "--threshold",
        type=float,
        default=10.0,
        help="regression percent (default 10)",
    )
    args = parser.parse_args(argv)
    failures = compare(load(args.old), load(args.new), args.threshold)
    if failures:
        print("compare: regressions:", file=sys.stderr)
        for line in failures:
            print(f"  {line}", file=sys.stderr)
        return 1
    print("compare: no regressions over threshold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
