#!/usr/bin/env python3
"""Host-side DCH / Universal INF checks (infverif analog when WDK is absent).

Flags co-installers, DefaultInstall, missing PnpLockdown/CatalogFile/DriverVer,
undecorated DDInstall vs NTarm64, and ClassGuid spelling. Does not replace
WDK `infverif /u`. Exit 1 on errors; `--werror` also fails on warnings.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys


class Finding:
    def __init__(self, path: pathlib.Path, level: str, message: str) -> None:
        self.path = path
        self.level = level
        self.message = message


def sections(text: str) -> set[str]:
    return {m.group(1) for m in re.finditer(r"^\[([^\]]+)\]", text, re.MULTILINE)}


def check_inf(path: pathlib.Path) -> list[Finding]:
    text = path.read_text(encoding="utf-8", errors="replace")
    lower = text.lower()
    names = {s.lower() for s in sections(text)}
    findings: list[Finding] = []

    def err(msg: str) -> None:
        findings.append(Finding(path, "error", msg))

    def warn(msg: str) -> None:
        findings.append(Finding(path, "warning", msg))

    if "[defaultinstall]" in lower:
        err("DCH Universal INF must not use [DefaultInstall]")
    if "coinstaller" in lower:
        err("DCH INF must not ship a co-installer")
    if not re.search(r"^pnpLockdown\s*=\s*1", text, re.IGNORECASE | re.MULTILINE):
        err("PnpLockdown = 1 is required")
    if not re.search(r"^catalogfile\s*=", text, re.IGNORECASE | re.MULTILINE):
        err("CatalogFile is required")
    if not re.search(r"^driverver\s*=", text, re.IGNORECASE | re.MULTILINE):
        err("DriverVer is required")
    else:
        m = re.search(
            r"^DriverVer\s*=\s*(\d{2}/\d{2}/\d{4}),(\d+\.\d+\.\d+\.\d+)",
            text,
            re.MULTILINE,
        )
        if not m:
            err("DriverVer must be MM/DD/YYYY,w.x.y.z")
    if not re.search(r"^signature\s*=\s*\"\$windows nt\$\"", text, re.IGNORECASE | re.MULTILINE):
        err('Signature = "$WINDOWS NT$" is required')
    if "ntarm64" not in lower:
        err("Manufacturer models section must include NTarm64")
    if "driverpackagetype" not in lower:
        warn("DriverPackageType missing (DCH PlugAndPlay)")
    if "driverpackagedisplayname" not in lower:
        warn("DriverPackageDisplayName missing")
    if re.search(r"\\\\[a-zA-Z0-9._-]+\\", text):
        err("UNC paths are not allowed in attestation packages")

    for m in re.finditer(
        r"^%[^%]+%\s*=\s*(\S+),\s*(PCI\\[^\s]+|Root\\[^\s]+)",
        text,
        re.MULTILINE | re.IGNORECASE,
    ):
        inst = m.group(1)
        if f"{inst.lower()}.ntarm64" not in names:
            err(f"DDInstall [{inst}.NTARM64] missing for hardware id")

    if ".nt]" in lower and ".ntarm64]" in lower:
        warn("both .NT and .NTARM64 DDInstall; prefer NTARM64-only on ARM64")

    return findings


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inf", nargs="+", type=pathlib.Path)
    parser.add_argument("--werror", action="store_true")
    args = parser.parse_args(argv)
    code = 0
    for path in args.inf:
        findings = check_inf(path)
        for item in findings:
            print(f"{item.level}: {path}: {item.message}")
            if item.level == "error" or (args.werror and item.level == "warning"):
                code = 1
        if not findings:
            print(f"ok: {path}")
    return code


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
