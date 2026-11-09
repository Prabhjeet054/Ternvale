#!/bin/bash
# Inventory every `unsafe` in workspace crates. Fails if a block lacks
# `// SAFETY:` or if `unsafe` appears outside ternvale-hv and docs/UNSAFE.md.
#
#   ./scripts/audit-unsafe.sh
#   AUDIT_UNSAFE_GEIGER=1 ./scripts/audit-unsafe.sh   # fail if cargo-geiger missing
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

python3 - "$root" <<'PY'
import os
import re
import sys

root = sys.argv[1]
docs = os.path.join(root, "docs", "UNSAFE.md")
with open(docs, encoding="utf-8") as f:
    text = f.read()
allow = []
in_list = False
for line in text.splitlines():
    if line.strip() == "ALLOWLIST:":
        in_list = True
        continue
    if in_list:
        if not line.strip():
            break
        allow.append(line.strip())
allow_set = set(allow)
if "ternvale-hv" not in allow_set:
    print("audit-unsafe: docs/UNSAFE.md allowlist must include ternvale-hv", file=sys.stderr)
    sys.exit(1)

skip_dirs = {
    "target",
    ".fuzz-toolchain",
    ".git",
    "guest-tests",
    "app",
}

unsafe_re = re.compile(r"\bunsafe\b")
comment_line = re.compile(r"^\s*//")
block_comment_start = re.compile(r"/\*")
attr_unsafe = re.compile(r"#\[unsafe")

def crate_of(path: str) -> str | None:
    rel = os.path.relpath(path, root)
    parts = rel.split(os.sep)
    if not parts or not parts[0].startswith("ternvale-"):
        return None
    return parts[0]


def strip_strings_and_comments(src: str) -> list[tuple[int, str]]:
    """Return (lineno, code) with strings and comments blanked (keep length)."""
    out_lines = []
    i = 0
    n = len(src)
    line_no = 1
    buf = []
    in_block = False
    in_line = False
    in_str = None  # '"', "'", 'r' raw-ish we treat " and b" and c"
    raw = False

    def flush():
        nonlocal buf, line_no
        out_lines.append((line_no, "".join(buf)))
        buf = []
        line_no += 1

    while i < n:
        ch = src[i]
        nxt = src[i + 1] if i + 1 < n else ""
        if in_line:
            if ch == "\n":
                in_line = False
                flush()
            else:
                buf.append(" ")
            i += 1
            continue
        if in_block:
            if ch == "\n":
                flush()
            elif ch == "*" and nxt == "/":
                buf.append("  ")
                in_block = False
                i += 2
                continue
            else:
                buf.append(" ")
            i += 1
            continue
        if in_str is not None:
            if ch == "\n":
                flush()
                i += 1
                continue
            if ch == "\\" and not raw:
                buf.append("  ")
                i += 2
                continue
            if ch == in_str:
                buf.append(" ")
                in_str = None
                raw = False
                i += 1
                continue
            buf.append(" ")
            i += 1
            continue
        if ch == "/" and nxt == "/":
            in_line = True
            buf.append(" ")
            i += 1
            continue
        if ch == "/" and nxt == "*":
            in_block = True
            buf.append(" ")
            i += 1
            continue
        if ch in "\"'":
            # skip lifetime 'a and byte/c strings prefixes already emitted
            if ch == "'" and i + 1 < n and src[i + 1] != "\\":
                # likely lifetime or char
                if i + 2 < n and src[i + 2] == "'" :
                    buf.append("   ")
                    i += 3
                    continue
                if src[i + 1].isalpha() or src[i + 1] == "_":
                    buf.append(ch)
                    i += 1
                    continue
            in_str = ch
            raw = (i > 0 and src[i - 1] in "brc")
            buf.append(" ")
            i += 1
            continue
        if ch == "\n":
            flush()
            i += 1
            continue
        buf.append(ch)
        i += 1
    if buf:
        out_lines.append((line_no, "".join(buf)))
    return out_lines


def preceding_has_safety(orig_lines: list[str], idx: int) -> bool:
    start = max(0, idx - 10)
    window = "\n".join(orig_lines[start : idx + 1])
    return bool(re.search(r"(?i)SAFETY:", window))


def classify(code_line: str) -> str | None:
    m = unsafe_re.search(code_line)
    if not m:
        return None
    rest = code_line[m.end() :].lstrip()
    if rest.startswith("extern"):
        return "extern"
    if attr_unsafe.search(code_line):
        return "attr"
    if rest.startswith("impl"):
        return "impl"
    if rest.startswith("fn") or rest.startswith("const fn"):
        return "fn"
    if rest.startswith("trait"):
        return "trait"
    return "block"


missing = []
forbidden = []
inventory = []  # (crate, rel, line, kind)

for dirpath, dirnames, filenames in os.walk(root):
    dirnames[:] = [
        d
        for d in dirnames
        if d not in skip_dirs and not d.startswith(".")
    ]
    for name in filenames:
        if not name.endswith(".rs"):
            continue
        path = os.path.join(dirpath, name)
        crate = crate_of(path)
        if crate is None:
            continue
        with open(path, encoding="utf-8") as f:
            src = f.read()
        orig = src.splitlines()
        for lineno, code in strip_strings_and_comments(src):
            kind = classify(code)
            if kind is None or kind in ("extern", "attr"):
                continue
            rel = os.path.relpath(path, root)
            inventory.append((crate, rel, lineno, kind))
            if crate not in allow_set:
                forbidden.append((crate, rel, lineno, kind))
            idx = lineno - 1
            if not preceding_has_safety(orig, idx):
                missing.append((rel, lineno, kind, orig[idx].strip()[:80]))

print(f"audit-unsafe: {len(inventory)} unsafe sites in workspace crates")
by_crate = {}
for crate, *_ in inventory:
    by_crate[crate] = by_crate.get(crate, 0) + 1
for crate in sorted(by_crate):
    print(f"  {crate}: {by_crate[crate]}")

failed = 0
if missing:
    failed = 1
    print("audit-unsafe: missing // SAFETY: comment:", file=sys.stderr)
    for rel, lineno, kind, snippet in missing:
        print(f"  {rel}:{lineno}: {kind}: {snippet}", file=sys.stderr)
if forbidden:
    failed = 1
    print("audit-unsafe: unsafe outside allowlist (docs/UNSAFE.md):", file=sys.stderr)
    for crate, rel, lineno, kind in forbidden:
        print(f"  {crate} {rel}:{lineno} {kind}", file=sys.stderr)
sys.exit(failed)
PY

status=$?

if command -v cargo-geiger >/dev/null 2>&1 || cargo geiger --help >/dev/null 2>&1; then
    echo "audit-unsafe: cargo geiger --workspace --quiet"
    cargo geiger --workspace --quiet || true
else
    echo "audit-unsafe: cargo-geiger not installed; grep inventory is the CI gate"
    if [[ "${AUDIT_UNSAFE_GEIGER:-0}" == "1" ]]; then
        echo "audit-unsafe: AUDIT_UNSAFE_GEIGER=1 requires cargo-geiger" >&2
        exit 1
    fi
fi

exit "$status"
