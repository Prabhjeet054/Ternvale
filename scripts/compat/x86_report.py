"""Render the x86 workload section of docs/X86_COMPAT.md."""

from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path
from typing import Any

MARKER = "## Step 100: x86 workload matrix"


def iso_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def cell(status: str) -> str:
    return {"pass": "pass", "fail": "**fail**", "skip": "skip"}.get(status, status)


def render_table(rows: list[dict[str, Any]], columns: list[str]) -> str:
    header = "| Workload | " + " | ".join(columns) + " |\n|---|" + "|".join(["---"] * len(columns)) + "|"
    lines = [header]
    for row in rows:
        cells = " | ".join(cell(str(row.get(col, "skip"))) for col in columns)
        lines.append(f"| {row.get('name')} (`{row.get('id')}`) | {cells} |")
    return "\n".join(lines)


def render_notes(rows: list[dict[str, Any]]) -> str:
    notes: list[str] = []
    for row in rows:
        reason = row.get("reason") or ""
        status = row.get("overall") or row.get("guest") or "skip"
        if reason:
            notes.append(f"- `{row.get('id')}` **{status}**: {reason}")
    return "\n".join(notes) if notes else "_No extra notes._"


def render_section(doc: dict[str, Any]) -> str:
    linux = list(doc.get("linux") or [])
    windows = list(doc.get("windows") or [])
    cols = ["image", "guest"]
    return f"""
{MARKER}

Generated: {doc.get("date")} on `{doc.get("host")}`.

Linux amd64 workloads need a usermode engine (Rosetta / FEX / qemu-user). Host
`docker pull --platform linux/amd64` only proves the **image exists**; in-guest
`docker run` needs dockerd + binfmt in a full Linux VM.

Windows x64 apps run through **Windows on ARM’s own emulator**. Ternvale does
not install or control that path.

### Linux (amd64)

{render_table(linux, cols)}

### Windows 11 ARM (x64 via WoA)

{render_table(windows, ["guest"])}

### Notes

{render_notes(linux + windows)}
"""


def merge_markdown(existing: str, section: str) -> str:
    base = existing.strip()
    if MARKER in base:
        base = base.split(MARKER)[0].rstrip()
    if not base.startswith("#"):
        base = "# x86 compatibility\n\n" + base
    return base + "\n" + section + "\n"


def write_markdown(path: Path, doc: dict[str, Any]) -> None:
    prev = path.read_text(encoding="utf-8") if path.is_file() else "# x86 compatibility\n"
    path.write_text(merge_markdown(prev, render_section(doc)), encoding="utf-8")
