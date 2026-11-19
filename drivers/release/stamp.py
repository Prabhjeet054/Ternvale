#!/usr/bin/env python3
"""Stamp DriverVer in INF files from the workspace package version.

Never embeds certificates. DriverVer is DATE,10.<major>.<minor>.<patch>
(Windows INF version tuple). Date is UTC today unless TERNVALE_DRIVER_DATE
is set (MM/DD/YYYY).
"""

from __future__ import annotations

import argparse
import datetime as dt
import os
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]


def workspace_version(root: pathlib.Path | None = None) -> str:
    cargo = (root or ROOT) / "Cargo.toml"
    text = cargo.read_text(encoding="utf-8")
    in_workspace = False
    for line in text.splitlines():
        if line.strip() == "[workspace.package]":
            in_workspace = True
            continue
        if in_workspace and line.startswith("["):
            break
        if in_workspace:
            m = re.match(r'version\s*=\s*"([^"]+)"', line)
            if m:
                return m.group(1)
    raise SystemExit("workspace.package version not found in Cargo.toml")


def inf_version(semver: str) -> str:
    parts = semver.split("+", 1)[0].split("-", 1)[0].split(".")
    nums = [int(p) if p.isdigit() else 0 for p in parts[:3]]
    while len(nums) < 3:
        nums.append(0)
    major, minor, patch = nums
    return f"10.{major}.{minor}.{patch}"


def driver_date() -> str:
    env = os.environ.get("TERNVALE_DRIVER_DATE", "").strip()
    if env:
        return env
    return dt.datetime.now(dt.timezone.utc).strftime("%m/%d/%Y")


def stamp_inf(text: str, semver: str, date: str | None = None) -> str:
    ver = inf_version(semver)
    when = date or driver_date()
    stamped = f"DriverVer   = {when},{ver}"
    if re.search(r"^DriverVer\s*=", text, re.MULTILINE):
        return re.sub(r"^DriverVer\s*=.*$", stamped, text, count=1, flags=re.MULTILINE)
    return text.replace("[Version]", f"[Version]\n{stamped}", 1)


def drivers_version(semver: str) -> str:
    return f"tvgpu-{semver}"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inf", nargs="+", type=pathlib.Path)
    parser.add_argument("--version", default="")
    parser.add_argument("--in-place", action="store_true")
    args = parser.parse_args(argv)
    semver = args.version or workspace_version()
    date = driver_date()
    for path in args.inf:
        text = path.read_text(encoding="utf-8")
        out = stamp_inf(text, semver, date)
        if args.in_place:
            path.write_text(out, encoding="utf-8")
        else:
            sys.stdout.write(out)
            if not out.endswith("\n"):
                sys.stdout.write("\n")
        print(
            f"stamped {path} DriverVer={date},{inf_version(semver)} drivers_version={drivers_version(semver)}",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
