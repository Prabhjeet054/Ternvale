#!/usr/bin/env python3
"""Validate a Partner Center attestation staging tree (no makecab required).

Microsoft Learn: each driver folder < 40 chars, no UNC, same architecture,
.sys + .inf + .pdb + .cat. Secrets (.pfx/.pvk/.key) are rejected.
"""

from __future__ import annotations

import argparse
import pathlib
import sys

MAX_FOLDER = 39
REQUIRED = {".sys", ".inf", ".pdb", ".cat"}
FORBIDDEN_SUFFIX = {".pfx", ".pvk", ".key"}


def validate_tree(root: pathlib.Path) -> list[str]:
    errors: list[str] = []
    if not root.is_dir():
        return [f"{root} is not a directory"]
    folders = [p for p in root.iterdir() if p.is_dir()]
    if not folders:
        errors.append("no per-driver folders")
    for folder in folders:
        if len(folder.name) > MAX_FOLDER:
            errors.append(f"{folder.name}: name longer than {MAX_FOLDER}")
        if any(ch in folder.name for ch in r'\/:*?"<>|') or " " in folder.name:
            errors.append(f"{folder.name}: special characters")
        suffixes = {p.suffix.lower() for p in folder.iterdir() if p.is_file()}
        missing = REQUIRED - suffixes
        if missing:
            errors.append(f"{folder.name}: missing {sorted(missing)}")
        for path in folder.rglob("*"):
            if path.is_file() and path.suffix.lower() in FORBIDDEN_SUFFIX:
                errors.append(f"secret not allowed: {path}")
            if path.is_file() and str(path).startswith("\\\\"):
                errors.append(f"UNC path: {path}")
    return errors


def write_fixture(dest: pathlib.Path) -> None:
    dest.mkdir(parents=True, exist_ok=True)
    for name in ("tvhello", "tvgpu-dod"):
        folder = dest / name
        folder.mkdir(exist_ok=True)
        (folder / f"{name}.sys").write_bytes(b"MZ-stub")
        (folder / f"{name}.inf").write_text("; stub\n", encoding="utf-8")
        (folder / f"{name}.pdb").write_bytes(b"pdb")
        (folder / f"{name}.cat").write_bytes(b"cat")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=pathlib.Path)
    parser.add_argument("--write-fixture", action="store_true")
    args = parser.parse_args(argv)
    if args.write_fixture:
        write_fixture(args.root)
    errors = validate_tree(args.root)
    for item in errors:
        print(f"error: {item}")
    if not errors:
        print(f"ok: attestation layout {args.root}")
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
