#!/usr/bin/env python3
"""run.py offline / fixture / COMPAT_BREAK (no hypervisor)."""

from __future__ import annotations

import json
import os
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

import run as compat_run

HERE = Path(__file__).resolve().parent


class RunTests(unittest.TestCase):
    def test_offline_exit_zero_when_only_skips(self) -> None:
        with TemporaryDirectory() as tmp:
            out_json = Path(tmp) / "compat.json"
            out_md = Path(tmp) / "COMPAT.md"
            code = compat_run.main(
                ["--offline", "--out-json", str(out_json), "--out-md", str(out_md)]
            )
            self.assertEqual(code, 0)
            doc = json.loads(out_json.read_text(encoding="utf-8"))
            self.assertEqual(doc["schema"], 1)
            self.assertTrue(doc["guests"])
            self.assertTrue(all(g["overall"] == "skip" for g in doc["guests"]))
            self.assertIn("Ternvale:", out_md.read_text(encoding="utf-8"))

    def test_broken_net_on_fixture_is_fail_exit_1(self) -> None:
        fixtures = HERE / "fixtures"
        with TemporaryDirectory() as tmp:
            out_json = Path(tmp) / "compat.json"
            out_md = Path(tmp) / "COMPAT.md"
            code = compat_run.main(
                [
                    "--offline",
                    "--fixture-dir",
                    str(fixtures),
                    "--break",
                    "net",
                    "--out-json",
                    str(out_json),
                    "--out-md",
                    str(out_md),
                ]
            )
            self.assertEqual(code, 1)
            doc = json.loads(out_json.read_text(encoding="utf-8"))
            alpine = next(g for g in doc["guests"] if g["id"] == "alpine-virt")
            self.assertEqual(alpine["features"]["net"]["status"], "fail")
            self.assertEqual(alpine["features"]["login"]["status"], "pass")
            self.assertNotEqual(alpine["features"]["net"]["status"], "skip")

    def test_merge_only_updates_named_guest(self) -> None:
        with TemporaryDirectory() as tmp:
            first = Path(tmp) / "compat.json"
            md = Path(tmp) / "COMPAT.md"
            self.assertEqual(
                compat_run.main(
                    ["--offline", "--out-json", str(first), "--out-md", str(md)]
                ),
                0,
            )
            code = compat_run.main(
                [
                    "--offline",
                    "--fixture-dir",
                    str(HERE / "fixtures"),
                    "--only",
                    "alpine-virt",
                    "--break",
                    "net",
                    "--merge-json",
                    str(first),
                    "--out-json",
                    str(first),
                    "--out-md",
                    str(md),
                ]
            )
            self.assertEqual(code, 1)
            doc = json.loads(first.read_text(encoding="utf-8"))
            alpine = next(g for g in doc["guests"] if g["id"] == "alpine-virt")
            debian = next(g for g in doc["guests"] if g["id"] == "debian-aarch64")
            self.assertEqual(alpine["features"]["net"]["status"], "fail")
            self.assertEqual(debian["overall"], "skip")

    def test_env_net_disabled_is_fail(self) -> None:
        os.environ["TERNVALE_COMPAT_NET"] = "0"
        self.addCleanup(lambda: os.environ.pop("TERNVALE_COMPAT_NET", None))
        fixtures = HERE / "fixtures"
        with TemporaryDirectory() as tmp:
            out_json = Path(tmp) / "compat.json"
            code = compat_run.main(
                [
                    "--offline",
                    "--fixture-dir",
                    str(fixtures),
                    "--out-json",
                    str(out_json),
                    "--out-md",
                    str(Path(tmp) / "COMPAT.md"),
                ]
            )
            self.assertEqual(code, 1)
            doc = json.loads(out_json.read_text(encoding="utf-8"))
            alpine = next(g for g in doc["guests"] if g["id"] == "alpine-virt")
            self.assertEqual(alpine["features"]["net"]["status"], "fail")


if __name__ == "__main__":
    unittest.main()
