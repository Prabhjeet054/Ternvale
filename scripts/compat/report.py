#!/usr/bin/env python3
"""Write compat JSON and docs/COMPAT.md from scored guest rows."""

from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

FEATURES_DEFAULT = [
    "login",
    "disk",
    "net",
    "display",
    "input",
    "audio",
    "agent",
    "share",
    "shutdown",
]


def cell(status: str) -> str:
    return {"pass": "pass", "fail": "**fail**", "skip": "skip"}.get(status, status)


def boot_cell(boot_ms: int | None) -> str:
    if boot_ms is None:
        return "—"
    return f"{boot_ms / 1000.0:.1f}"


def build_document(
    guests: list[dict[str, Any]],
    *,
    features: list[str] | None = None,
    ternvale_version: str,
    date: str,
    host: str,
) -> dict[str, Any]:
    feats = features or FEATURES_DEFAULT
    return {
        "schema": 1,
        "ternvale_version": ternvale_version,
        "date": date,
        "host": host,
        "features": feats,
        "guests": guests,
    }


def render_markdown(doc: dict[str, Any]) -> str:
    feats: list[str] = list(doc.get("features") or FEATURES_DEFAULT)
    header = (
        "| Guest | Kernel | Boot (s) | "
        + " | ".join(feats)
        + " |\n|---|---|---|"
        + "|".join(["---"] * len(feats))
        + "|\n"
    )
    rows: list[str] = []
    for guest in doc.get("guests") or []:
        name = guest.get("name") or guest.get("id")
        kernel = guest.get("kernel") or "—"
        featmap = guest.get("features") or {}
        cells = " | ".join(cell((featmap.get(f) or {}).get("status", "skip")) for f in feats)
        rows.append(
            f"| {name} (`{guest.get('id')}`) | {kernel} | {boot_cell(guest.get('boot_ms'))} | {cells} |"
        )
    notes: list[str] = []
    for guest in doc.get("guests") or []:
        if guest.get("skip_reason"):
            notes.append(f"- `{guest.get('id')}` skipped: {guest['skip_reason']}")
        featmap = guest.get("features") or {}
        for name, row in featmap.items():
            if row.get("status") == "fail":
                issue = row.get("issue") or ""
                extra = f" ({issue})" if issue else ""
                notes.append(
                    f"- `{guest.get('id')}` **{name}**: {row.get('reason', 'fail')}{extra}"
                )
            elif row.get("status") == "skip" and not guest.get("skip_reason"):
                notes.append(f"- `{guest.get('id')}` {name} skip: {row.get('reason', '')}")
    note_block = "\n".join(notes) if notes else "_No skips or failures._"
    return (
        f"# Ternvale guest compatibility\n\n"
        f"Generated: {doc.get('date')}\n\n"
        f"Ternvale: `{doc.get('ternvale_version')}` on `{doc.get('host')}`.\n\n"
        f"Cells are pass / fail / skip. Skip means the guest does not support the "
        f"feature (or the image was not present); it is not a failure.\n\n"
        f"{header}"
        + "\n".join(rows)
        + "\n\n## Notes\n\n"
        + note_block
        + "\n\n## Review\n\n"
        + "Every **fail** must have a reason in Notes (or a tracking issue). "
        + "Skip is not a failure: missing images, unsupported guest features, "
        + "or the host killing the boot harness before serial (AMFI SIGKILL).\n"
    )


def write_outputs(doc: dict[str, Any], json_path: Path, markdown_path: Path) -> None:
    json_path.parent.mkdir(parents=True, exist_ok=True)
    markdown_path.parent.mkdir(parents=True, exist_ok=True)
    json_path.write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")
    markdown_path.write_text(render_markdown(doc), encoding="utf-8")


def iso_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
