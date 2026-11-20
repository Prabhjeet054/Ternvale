#!/usr/bin/env python3
"""Host checks for the tvvk ICD skeleton (no Windows toolchain)."""

from __future__ import annotations

import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent


def main() -> int:
    json_path = HERE / "tvvk.json"
    data = json.loads(json_path.read_text(encoding="utf-8"))
    icd = data["ICD"]
    if icd.get("library_path") != ".\\tvvk.dll":
        print("bad library_path", icd, file=sys.stderr)
        return 1
    if icd.get("api_version") != "1.0.0":
        print("bad api_version", icd, file=sys.stderr)
        return 1
    header = (HERE / "encode.h").read_text(encoding="utf-8")
    if "TVVK_CS_NOP 1u" not in header and "TVVK_CS_NOP 1" not in header:
        print("encode.h missing TVVK_CS_NOP 1", file=sys.stderr)
        return 1
    inf = (HERE / "tvvk.inf").read_text(encoding="utf-8")
    if "Khronos\\Vulkan\\Drivers" not in inf:
        print("tvvk.inf missing Vulkan Drivers key", file=sys.stderr)
        return 1
    print("tvvk proto check ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
