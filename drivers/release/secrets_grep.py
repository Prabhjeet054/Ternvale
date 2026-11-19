#!/usr/bin/env python3
"""Scan the repo for certificate private-key material. Exit 1 if any hit.

Ignores this file's own pattern strings, Sparkle *test* PEM builders that
emit a key at runtime (not a committed PFX), and binary git objects.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]

# Real on-disk secrets, not documentation of the words "BEGIN PRIVATE KEY".
HIT = re.compile(
    rb"-----BEGIN (?:RSA |EC |OPENSSH |ENCRYPTED )?PRIVATE KEY-----"
    rb"|Bag Attributes"
    rb"|Proc-Type:\s*4,ENCRYPTED"
)
SKIP_DIR = {
    ".git",
    "target",
    "dist",
    ".build",
    "node_modules",
    "attestation-stage",
    "__pycache__",
    "test-assets",
}
SKIP_SUFFIX = {".png", ".jpg", ".pdf", ".iso", ".sys", ".pdb", ".cat", ".elf", ".o", ".pyc", ".cpio"}
# Sparkle unit tests build a throwaway PEM in memory; this scanner's own regex.
ALLOW_FILES = {
    "scripts/updates/generate_appcast.py",
    "scripts/updates/test_generate_appcast.py",
    "drivers/release/secrets_grep.py",
}


def tracked_files(root: pathlib.Path) -> list[pathlib.Path] | None:
    git = root / ".git"
    if not git.exists():
        return None
    import subprocess

    result = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z"],
        check=False,
        capture_output=True,
    )
    if result.returncode != 0:
        return None
    out: list[pathlib.Path] = []
    for rel in result.stdout.split(b"\0"):
        if not rel:
            continue
        out.append(root / rel.decode("utf-8", errors="replace"))
    return out


def scan(root: pathlib.Path) -> list[str]:
    hits: list[str] = []
    files = tracked_files(root)
    paths = files if files is not None else [p for p in root.rglob("*") if p.is_file()]
    for path in paths:
        if not path.is_file():
            continue
        if any(part in SKIP_DIR for part in path.parts):
            continue
        rel = path.relative_to(root).as_posix()
        if rel in ALLOW_FILES:
            continue
        if path.suffix.lower() in SKIP_SUFFIX:
            continue
        if path.name == "id_ed25519" or path.suffix.lower() in {".pfx", ".pvk"}:
            hits.append(rel)
            continue
        try:
            data = path.read_bytes()
        except OSError:
            continue
        if HIT.search(data):
            hits.append(rel)
    return hits


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=pathlib.Path, default=ROOT)
    args = parser.parse_args(argv)
    hits = scan(args.root)
    if hits:
        print("secret material:")
        for hit in hits:
            print(f"  {hit}")
        return 1
    print("ok: no certificate private key material in the tree")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
