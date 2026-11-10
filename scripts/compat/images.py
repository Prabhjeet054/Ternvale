#!/usr/bin/env python3
"""Resolve matrix image paths; missing files skip the guest (not fail)."""

from __future__ import annotations

import glob
from pathlib import Path
from typing import Any


def resolve_one(root: Path, spec: str) -> Path | None:
    path = Path(spec)
    if not path.is_absolute():
        path = root / spec
    if "*" in spec or "?" in spec:
        matches = sorted(glob.glob(str(path)))
        return Path(matches[0]) if matches else None
    return path if path.is_file() else None


def missing_images(root: Path, guest: dict[str, Any]) -> list[str]:
    images: dict[str, str] = dict(guest.get("images") or {})
    # Windows can use a preinstalled disk *or* an unattended ISO.
    if guest.get("os") == "windows":
        disk = resolve_one(root, images["disk"]) if "disk" in images else None
        iso = resolve_one(root, images["iso"]) if "iso" in images else None
        if disk is None and iso is None:
            return ["disk or iso"]
        return []
    missing: list[str] = []
    for key, spec in images.items():
        if resolve_one(root, spec) is None:
            missing.append(key)
    return missing


def skip_reason(root: Path, guest: dict[str, Any]) -> str | None:
    miss = missing_images(root, guest)
    if not miss:
        return None
    return "no image (" + ", ".join(miss) + ") under test-assets/compat or env overlay"
