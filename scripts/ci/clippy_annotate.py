#!/usr/bin/env python3
"""Turn `cargo clippy --message-format=json` into GitHub Actions annotations.

Reads JSON lines from a file (or stdin). Prints `::error file=…,line=…::`
for each compiler error so the PR Checks tab shows a clickable location.
Exit 1 if any error was seen (or if CLIPPY_EXIT is set to a non-zero int).
"""

from __future__ import annotations

import json
import os
import sys
from typing import Any, Iterable, TextIO


def encode_annotation(text: str) -> str:
    # GitHub workflow command encoding: % \r \n (colons stay in the message).
    return text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def errors_from_messages(lines: Iterable[str]) -> list[tuple[str, int, int, str]]:
    found: list[tuple[str, int, int, str]] = []
    for raw in lines:
        raw = raw.strip()
        if not raw:
            continue
        try:
            obj: Any = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if obj.get("reason") != "compiler-message":
            continue
        message = obj.get("message") or {}
        if message.get("level") != "error":
            continue
        rendered = (message.get("rendered") or message.get("message") or "").strip()
        spans = message.get("spans") or []
        primary = next((s for s in spans if s.get("is_primary")), None)
        span = primary or (spans[0] if spans else {})
        path = span.get("file_name") or "unknown"
        line = int(span.get("line_start") or 1)
        col = int(span.get("column_start") or 1)
        found.append((path, line, col, rendered or "clippy error"))
    return found


def emit(errors: list[tuple[str, int, int, str]], out: TextIO) -> None:
    for path, line, col, rendered in errors:
        body = encode_annotation(rendered)
        out.write(f"::error file={path},line={line},col={col}::{body}\n")


def main(argv: list[str] | None = None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    if argv and argv[0] not in ("-",):
        with open(argv[0], encoding="utf-8") as handle:
            errors = errors_from_messages(handle)
    else:
        errors = errors_from_messages(sys.stdin)
    emit(errors, sys.stderr)
    prior = os.environ.get("CLIPPY_EXIT", "")
    if prior.isdigit() and int(prior) != 0:
        return int(prior)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
