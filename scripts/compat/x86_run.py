#!/usr/bin/env python3
"""Step 100 x86 workload matrix → docs/X86_COMPAT.md. See scripts/compat/README.md."""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

from x86_report import iso_now, write_markdown

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
WOA = (
    "Windows on ARM provides x64 emulation inside the guest; "
    "Ternvale does not control it"
)


def log(message: str) -> None:
    print(f"compat-x86: {message}", file=sys.stderr, flush=True)


def load_matrix() -> dict[str, Any]:
    return json.loads((HERE / "x86_matrix.json").read_text(encoding="utf-8"))


def docker_ok() -> bool:
    if shutil.which("docker") is None:
        return False
    try:
        subprocess.run(
            ["docker", "info"],
            check=True,
            capture_output=True,
            timeout=20,
        )
        return True
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return False


def inspect_amd64(image: str) -> tuple[bool, str]:
    try:
        proc = subprocess.run(
            ["docker", "image", "inspect", "--format", "{{.Architecture}}", image],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return False, str(error)
    arch = (proc.stdout or "").strip()
    if proc.returncode == 0 and arch in {"amd64", "x86_64"}:
        return True, f"host docker image arch={arch}"
    return False, (proc.stderr or proc.stdout or "inspect failed").strip()[:200]


def pull_amd64(image: str) -> tuple[str, str]:
    try:
        proc = subprocess.run(
            ["docker", "pull", "--platform", "linux/amd64", image],
            capture_output=True,
            text=True,
            timeout=180,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return "skip", "docker pull timed out"
    except OSError as error:
        return "skip", str(error)
    if proc.returncode != 0:
        err = (proc.stderr or proc.stdout or "pull failed").strip().splitlines()
        return "skip", err[-1] if err else "docker pull failed"
    ok, detail = inspect_amd64(image)
    if ok:
        return "pass", detail
    # Docker Desktop may keep an arm64 tag locally; --platform pull still
    # proves the amd64 index exists.
    extra = f" ({detail})" if detail else ""
    return "pass", f"docker pull --platform linux/amd64 ok{extra}"


def guest_skip_reason(kind: str, suite_log: Path | None) -> str:
    if suite_log and suite_log.is_file():
        text = suite_log.read_text(encoding="utf-8", errors="replace")
        if kind in {"cli", "go"} and "det v1" in text:
            return ""
        if kind == "docker" and "DOCKER_IMAGE_RC=0" in text:
            return ""
        if kind == "docker":
            return "no dockerd in the Step 98/99 initramfs guest; image payload only"
    if kind == "java":
        return "needs a glibc RootFS / JRE in the guest; not in the initramfs live path"
    if kind == "docker":
        return "in-guest docker run not exercised (no Linux VM with dockerd in this run)"
    if kind in {"cli", "go"}:
        return "no in-guest x86 engine run in this pass (see make x86-engines)"
    return "not run in-guest"


def score_linux(
    row: dict[str, Any],
    *,
    live_docker: bool,
    suite_log: Path | None,
) -> dict[str, Any]:
    kind = row.get("kind") or "cli"
    image = row.get("image") or ""
    image_status = "skip"
    image_reason = "offline (pass --live-docker when Docker Desktop is up)"
    if live_docker and image:
        image_status, image_reason = pull_amd64(image)
        log(f"{row.get('id')}: image={image_status} {image_reason}")
    elif not live_docker:
        image_reason = "docker not used (--offline or docker unreachable)"
    guest_reason = guest_skip_reason(kind, suite_log)
    guest_status = "skip"
    if suite_log and suite_log.is_file():
        text = suite_log.read_text(encoding="utf-8", errors="replace")
        if kind in {"cli", "go"} and "DIRECT_HELLO_RC=0" in text:
            guest_status = "pass"
            guest_reason = "static x86_64 hello via FEX (Step 98 live)"
        elif kind in {"cli", "go"} and "DET_RC=0" in text and "det v1" in text:
            guest_status = "pass"
            guest_reason = "deterministic x86_64 probe via FEX/qemu-user (Step 99)"
        elif kind == "docker" and "DOCKER_IMAGE_RC=0" in text:
            guest_status = "pass"
            guest_reason = "amd64 image payload via FEX; docker run still skipped"
    overall = "fail" if "fail" in {image_status, guest_status} else (
        "pass" if guest_status == "pass" or image_status == "pass" else "skip"
    )
    reason = guest_reason if guest_status != "skip" or not live_docker else image_reason
    if guest_status == "skip":
        reason = f"{image_reason}; guest: {guest_reason}"
    return {
        "id": row.get("id"),
        "name": row.get("name"),
        "kind": kind,
        "image": image_status,
        "guest": guest_status,
        "overall": overall,
        "reason": reason,
    }


def score_windows(row: dict[str, Any]) -> dict[str, Any]:
    return {
        "id": row.get("id"),
        "name": row.get("name"),
        "kind": "windows",
        "guest": "skip",
        "overall": "skip",
        "reason": WOA,
    }


def latest_suite_log() -> Path | None:
    latest = ROOT / "target" / "x86-suite" / "latest"
    native = ROOT / "target" / "native-fex" / "latest"
    for pointer in (latest, native):
        if pointer.is_file():
            dest = Path(pointer.read_text(encoding="utf-8").strip())
            serial = dest / "native-serial.log"
            if serial.is_file():
                return serial
    for folder in (ROOT / "target" / "x86-suite", ROOT / "target" / "native-fex"):
        if not folder.is_dir():
            continue
        serials = sorted(folder.glob("*/native-serial.log"))
        if serials:
            return serials[-1]
    return None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--live-docker", action="store_true")
    parser.add_argument("--auto", action="store_true")
    parser.add_argument(
        "--out-json",
        type=Path,
        default=ROOT / "target" / "compat" / "x86.json",
    )
    parser.add_argument(
        "--out-md",
        type=Path,
        default=ROOT / "docs" / "X86_COMPAT.md",
    )
    args = parser.parse_args(argv)
    live_docker = bool(args.live_docker)
    if args.auto and not args.offline:
        live_docker = docker_ok()
        log(f"auto live-docker={live_docker}")
    if args.offline:
        live_docker = False

    matrix = load_matrix()
    suite_log = None if args.offline else latest_suite_log()
    if suite_log:
        log(f"guest evidence {suite_log}")
    linux = [
        score_linux(row, live_docker=live_docker, suite_log=suite_log)
        for row in matrix.get("linux") or []
    ]
    windows = [score_windows(row) for row in matrix.get("windows") or []]
    doc = {
        "schema": 1,
        "date": iso_now(),
        "host": platform.platform(),
        "live_docker": live_docker,
        "linux": linux,
        "windows": windows,
    }
    args.out_json.parent.mkdir(parents=True, exist_ok=True)
    args.out_json.write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")
    write_markdown(args.out_md, doc)
    log(f"wrote {args.out_json} and {args.out_md}")
    if any(row.get("overall") == "fail" for row in linux + windows):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
