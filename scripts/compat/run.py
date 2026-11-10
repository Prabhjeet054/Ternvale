#!/usr/bin/env python3
"""Unattended guest compatibility matrix. See scripts/compat/README.md."""

from __future__ import annotations

import argparse
import json
import os
import platform
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

from checks import score_guest
from images import skip_reason
from report import build_document, iso_now, write_outputs

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def log(message: str) -> None:
    print(f"compat: {message}", file=sys.stderr, flush=True)


def load_matrix() -> dict[str, Any]:
    path = HERE / "matrix.json"
    return json.loads(path.read_text(encoding="utf-8"))


def ternvale_version() -> str:
    cargo = ROOT / "Cargo.toml"
    for line in cargo.read_text(encoding="utf-8").splitlines():
        if line.strip().startswith("version"):
            return line.split("=", 1)[1].strip().strip('"')
    return "0.1.0"


def hv_ok() -> bool:
    try:
        out = subprocess.check_output(["sysctl", "-n", "kern.hv_support"], text=True)
        return out.strip() == "1"
    except (OSError, subprocess.CalledProcessError):
        return False


def break_features(raw: str | None) -> frozenset[str]:
    if not raw:
        return frozenset()
    return frozenset(part.strip() for part in raw.split(",") if part.strip())


def read_serial(path: Path | None) -> str:
    if path is None or not path.is_file():
        return ""
    return path.read_text(encoding="utf-8", errors="replace")


def run_live(
    guest: dict[str, Any], work: Path, timeout: int
) -> tuple[str, int | None, int | None]:
    """Boot via scripts/boot-test.sh for Alpine virt; other guests stay skip-live."""
    scenario = {
        "alpine-virt": os.environ.get("TERNVALE_COMPAT_ALPINE_SCENARIO", "initrd"),
    }.get(guest.get("id") or "")
    if not scenario:
        return "", None, None
    env = os.environ.copy()
    env["TERNVALE_BOOT_SCENARIO"] = scenario
    log(f"{guest.get('id')}: live boot-test scenario={scenario}")
    started = time.monotonic()
    proc = subprocess.run(
        [str(ROOT / "scripts" / "boot-test.sh")],
        cwd=ROOT,
        env=env,
        timeout=timeout,
        capture_output=True,
        text=True,
    )
    elapsed_ms = int((time.monotonic() - started) * 1000)
    serial_dir = ROOT / "target" / "boot-logs"
    serial = ""
    if serial_dir.is_dir():
        logs = sorted(serial_dir.rglob("*serial*"))
        if logs:
            serial = read_serial(logs[-1])
    combined = (proc.stdout or "") + "\n" + (proc.stderr or "") + "\n" + serial
    work.mkdir(parents=True, exist_ok=True)
    (work / f"{guest.get('id')}.boot.txt").write_text(combined, encoding="utf-8")
    if proc.returncode != 0 and "SYSTEM_OFF" not in combined:
        log(f"{guest.get('id')}: boot-test exit {proc.returncode}")
    return combined, elapsed_ms, proc.returncode


def host_kill_reason(combined: str, returncode: int | None) -> str | None:
    text = combined.lower()
    if "sigkill" in text or returncode == -9:
        return (
            "host SIGKILL (AMFI/codesign) before guest serial; "
            "see docs/CI.md and scripts/sign-and-run.sh"
        )
    return None


def score_one(
    features: list[str],
    guest: dict[str, Any],
    *,
    live: bool,
    timeout: int,
    broken: frozenset[str],
    work: Path,
    fixture: Path | None,
) -> dict[str, Any]:
    gid = guest.get("id") or "guest"
    reason = skip_reason(ROOT, guest)
    nic_enabled = os.environ.get("TERNVALE_COMPAT_NET", "1") != "0"
    guest_broken = broken
    apply_live = live and fixture is None

    serial = ""
    boot_ms: int | None = None
    if fixture is not None:
        serial_path = fixture / f"{gid}.serial.txt"
        if serial_path.is_file():
            serial = read_serial(serial_path)
            reason = None
        else:
            miss = skip_reason(ROOT, guest) or "no fixture serial"
            log(f"{gid}: skip ({miss})")
            return score_guest(features, guest, "", guest_skip=miss, nic_enabled=nic_enabled)
    elif reason:
        log(f"{gid}: skip ({reason})")
        return score_guest(features, guest, "", guest_skip=reason, nic_enabled=nic_enabled)
    elif live:
        try:
            serial, boot_ms, rc = run_live(guest, work, timeout)
            kill = host_kill_reason(serial, rc)
            if kill:
                log(f"{gid}: skip ({kill})")
                return score_guest(
                    features, guest, "", guest_skip=kill, nic_enabled=nic_enabled, boot_ms=boot_ms
                )
        except subprocess.TimeoutExpired:
            serial = ""
            log(f"{gid}: boot timed out")
            return score_guest(
                features,
                guest,
                "",
                nic_enabled=nic_enabled,
                break_features=guest_broken,
                boot_ms=timeout * 1000,
                apply_live_features=apply_live,
            )
    else:
        reason = "offline (pass --live on a host with kern.hv_support=1)"
        if skip_reason(ROOT, guest) is None:
            reason = "image present; " + reason
        log(f"{gid}: skip ({reason})")
        return score_guest(features, guest, "", guest_skip=reason, nic_enabled=nic_enabled)

    return score_guest(
        features,
        guest,
        serial,
        nic_enabled=nic_enabled,
        break_features=guest_broken,
        boot_ms=boot_ms,
        apply_live_features=apply_live,
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live", action="store_true", help="boot guests that have images")
    parser.add_argument(
        "--auto",
        action="store_true",
        help="--live when kern.hv_support=1, otherwise skip live boots",
    )
    parser.add_argument("--offline", action="store_true", help="never boot (CI unit path)")
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--break", dest="broken", default=os.environ.get("COMPAT_BREAK", ""))
    parser.add_argument("--fixture-dir", type=Path, default=None)
    parser.add_argument(
        "--only",
        default="",
        help="comma-separated guest ids (re-run failed guests)",
    )
    parser.add_argument(
        "--merge-json",
        type=Path,
        default=None,
        help="previous compat.json; --only rows replace matching ids",
    )
    parser.add_argument(
        "--out-json",
        type=Path,
        default=ROOT / "target" / "compat" / "compat.json",
    )
    parser.add_argument(
        "--out-md",
        type=Path,
        default=ROOT / "docs" / "COMPAT.md",
    )
    parser.add_argument(
        "--verify-break-net",
        action="store_true",
        help="append a COMPAT_BREAK=net row (fail, not skip) using alpine fixtures",
    )
    args = parser.parse_args(argv)

    live = bool(args.live)
    if args.auto and not args.offline:
        live = hv_ok()
        log(f"auto live={live} kern.hv_support")
    if args.offline:
        live = False

    matrix = load_matrix()
    features = list(matrix.get("features") or [])
    broken = break_features(args.broken)
    work = ROOT / "target" / "compat"
    work.mkdir(parents=True, exist_ok=True)
    wanted = {part.strip() for part in args.only.split(",") if part.strip()}
    guests = list(matrix.get("guests") or [])
    if wanted:
        guests = [g for g in guests if g.get("id") in wanted]
        log(f"only {','.join(wanted)}")
    rows = [
        score_one(
            features,
            guest,
            live=live,
            timeout=args.timeout,
            broken=broken,
            work=work,
            fixture=args.fixture_dir,
        )
        for guest in guests
    ]
    if args.merge_json and args.merge_json.is_file():
        previous = json.loads(args.merge_json.read_text(encoding="utf-8"))
        by_id = {row.get("id"): row for row in rows}
        merged = []
        for old in previous.get("guests") or []:
            merged.append(by_id.pop(old.get("id"), old))
        merged.extend(by_id.values())
        rows = merged
    doc = build_document(
        rows,
        features=features,
        ternvale_version=ternvale_version(),
        date=iso_now(),
        host=platform.platform(),
    )
    md_also = work / "COMPAT.md"
    write_outputs(doc, args.out_json, args.out_md)
    if args.out_md.resolve() != md_also.resolve():
        write_outputs(doc, args.out_json, md_also)
    log(f"wrote {args.out_json} and {args.out_md}")
    if args.verify_break_net:
        alpine = next(g for g in matrix.get("guests") or [] if g.get("id") == "alpine-virt")
        demo = score_one(
            features,
            alpine,
            live=False,
            timeout=args.timeout,
            broken=frozenset({"net"}),
            work=work,
            fixture=HERE / "fixtures",
        )
        net = (demo.get("features") or {}).get("net") or {}
        login = (demo.get("features") or {}).get("login") or {}
        section = (
            "\n## Deliberate virtio-net disable\n\n"
            "Same Alpine virt fixture with `COMPAT_BREAK=net` / virtio-net disabled. "
            "Net must be **fail**, not skip.\n\n"
            f"| Guest | login | net |\n|---|---|---|\n"
            f"| Alpine Linux (`alpine-virt`) | {login.get('status')} | **{net.get('status')}** |\n\n"
            f"- net: {net.get('reason')} (status={net.get('status')})\n"
            f"- login still {login.get('status')}\n"
        )
        args.out_md.write_text(args.out_md.read_text(encoding="utf-8") + section, encoding="utf-8")
        md_also = work / "COMPAT.md"
        if md_also.is_file() and md_also.resolve() != args.out_md.resolve():
            md_also.write_text(md_also.read_text(encoding="utf-8") + section, encoding="utf-8")
        log(f"break-net demo net={net.get('status')} login={login.get('status')}")
        if net.get("status") != "fail":
            return 1
    if any(row.get("overall") == "fail" for row in rows):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
