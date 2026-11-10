#!/usr/bin/env python3
"""Score one guest's serial (and optional host) log against its matrix row.

Unsupported features are skip, never fail. A supported feature with no
evidence is fail (unless the whole guest was skipped for a missing image).
"""

from __future__ import annotations

import re
from typing import Any

STATUSES = ("pass", "fail", "skip")


def compile_pattern(expr: str | None) -> re.Pattern[str] | None:
    if not expr:
        return None
    return re.compile(expr, re.IGNORECASE | re.MULTILINE)


def feature_result(
    feature: str,
    guest: dict[str, Any],
    serial: str,
    *,
    guest_skip: str | None = None,
    nic_enabled: bool = True,
    break_features: frozenset[str] | None = None,
    apply_live_features: bool = False,
) -> dict[str, str]:
    """Return {status, reason} for one feature."""
    supported: list[str] = list(guest.get("supported") or [])
    reasons: dict[str, str] = dict(guest.get("unsupported_reason") or {})
    broken = break_features or frozenset()

    if guest_skip:
        return {"status": "skip", "reason": guest_skip}

    live_only = guest.get("live_features") if apply_live_features else None
    if live_only is not None and feature not in live_only:
        return {
            "status": "skip",
            "reason": reasons.get(feature)
            or f"live scenario does not exercise {feature}",
        }

    if feature not in supported:
        return {
            "status": "skip",
            "reason": reasons.get(feature, f"{guest.get('os', 'guest')} does not support {feature}"),
        }

    # Harness bug: nic omitted while net is supported → still fail, not skip.
    if feature == "net" and not nic_enabled:
        return {"status": "fail", "reason": "virtio-net disabled in VM config"}

    if feature in broken:
        return {"status": "fail", "reason": f"COMPAT_BREAK={feature}"}

    patterns: dict[str, str] = dict(guest.get("patterns") or {})
    cre = compile_pattern(patterns.get(feature))
    if cre is None:
        return {"status": "fail", "reason": f"no pattern configured for {feature}"}
    if cre.search(serial or ""):
        return {"status": "pass", "reason": "serial matched"}
    return {"status": "fail", "reason": f"serial did not match /{patterns.get(feature)}/"}


def score_guest(
    features: list[str],
    guest: dict[str, Any],
    serial: str,
    *,
    guest_skip: str | None = None,
    nic_enabled: bool = True,
    break_features: frozenset[str] | None = None,
    boot_ms: int | None = None,
    apply_live_features: bool = False,
) -> dict[str, Any]:
    results = {
        name: feature_result(
            name,
            guest,
            serial,
            guest_skip=guest_skip,
            nic_enabled=nic_enabled,
            break_features=break_features,
            apply_live_features=apply_live_features,
        )
        for name in features
    }
    statuses = {row["status"] for row in results.values()}
    if guest_skip:
        overall = "skip"
    elif "fail" in statuses:
        overall = "fail"
    elif statuses == {"skip"}:
        overall = "skip"
    else:
        overall = "pass"
    return {
        "id": guest.get("id"),
        "name": guest.get("name"),
        "os": guest.get("os"),
        "arch": guest.get("arch"),
        "kernel": guest.get("kernel"),
        "boot_ms": boot_ms,
        "overall": overall,
        "features": results,
        "skip_reason": guest_skip,
    }
