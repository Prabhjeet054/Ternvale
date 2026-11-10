#!/usr/bin/env python3
"""Fail if THIRD_PARTY_NOTICES.md is missing a crate or bundled component."""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NOTICES = ROOT / "THIRD_PARTY_NOTICES.md"
CRATE = re.compile(r"([A-Za-z0-9_-]+)@([0-9][^,\s]*)")
BUNDLED = ("Sparkle", "EDK2", "swtpm", "virtio-win", "WinFsp", "GPLv3")


def deny_list() -> str:
    proc = subprocess.run(
        ["cargo", "deny", "list"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        raise SystemExit(proc.returncode)
    return proc.stdout


def crates_from_deny(text: str) -> set[str]:
    found: set[str] = set()
    for line in text.splitlines():
        if line.startswith("Unlicensed"):
            continue
        found.update(m.group(0) for m in CRATE.finditer(line))
    return found


def main() -> int:
    if not NOTICES.is_file():
        print("check-notices: missing THIRD_PARTY_NOTICES.md", file=sys.stderr)
        return 1
    body = NOTICES.read_text(encoding="utf-8")
    expected = crates_from_deny(deny_list())
    missing = sorted(c for c in expected if c not in body)
    if missing:
        print("check-notices: crates missing from notices:", file=sys.stderr)
        for crate in missing:
            print(f"  {crate}", file=sys.stderr)
        return 1
    absent = [name for name in BUNDLED if name not in body]
    if absent:
        print(
            "check-notices: bundled component missing:",
            ", ".join(absent),
            file=sys.stderr,
        )
        return 1
    if "GPL" in body.split("## Optional", 1)[0] and "No crate in this graph is GPL" not in body:
        print("check-notices: unexpected GPL in crate section", file=sys.stderr)
        return 1
    print(
        f"check-notices: {len(expected)} crate versions + bundled {', '.join(BUNDLED)} ok"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
