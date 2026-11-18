#!/usr/bin/env python3
"""x86 matrix scoring and markdown merge (no hypervisor)."""

from __future__ import annotations

import json
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

import x86_report
import x86_run


class X86Matrix(unittest.TestCase):
    def test_offline_skips_and_keeps_woa_note(self) -> None:
        with TemporaryDirectory() as tmp:
            out_json = Path(tmp) / "x86.json"
            out_md = Path(tmp) / "X86_COMPAT.md"
            out_md.write_text("# x86 compatibility (Step 98 live)\n\nkept\n", encoding="utf-8")
            code = x86_run.main(
                [
                    "--offline",
                    "--out-json",
                    str(out_json),
                    "--out-md",
                    str(out_md),
                ]
            )
            self.assertEqual(code, 0)
            doc = json.loads(out_json.read_text(encoding="utf-8"))
            ids = {row["id"] for row in doc["linux"]}
            self.assertIn("docker-nginx", ids)
            self.assertIn("go-hello", ids)
            self.assertIn("java-hello", ids)
            self.assertTrue(all(row["overall"] == "skip" for row in doc["linux"]))
            self.assertEqual(len(doc["windows"]), 3)
            self.assertTrue(all(row["overall"] == "skip" for row in doc["windows"]))
            self.assertIn("Windows on ARM", doc["windows"][0]["reason"])
            md = out_md.read_text(encoding="utf-8")
            self.assertIn("Step 98 live", md)
            self.assertIn("Step 100: x86 workload matrix", md)
            self.assertIn("nginx", md)
            self.assertIn("7-Zip", md)

    def test_merge_replaces_old_step_100(self) -> None:
        prev = "# x86 compatibility\n\n## Step 100: x86 workload matrix\n\nold\n"
        section = x86_report.render_section(
            {
                "date": "2026-11-18T00:00:00Z",
                "host": "test",
                "linux": [
                    {
                        "id": "docker-nginx",
                        "name": "nginx",
                        "image": "pass",
                        "guest": "skip",
                        "overall": "pass",
                        "reason": "pulled",
                    }
                ],
                "windows": [],
            }
        )
        merged = x86_report.merge_markdown(prev, section)
        self.assertEqual(merged.count("Step 100:"), 1)
        self.assertIn("pulled", merged)
        self.assertNotIn("\nold\n", merged)

    def test_guest_pass_from_suite_log(self) -> None:
        with TemporaryDirectory() as tmp:
            serial = Path(tmp) / "native-serial.log"
            serial.write_text("det v1 iters=1 sum=1\nDET_RC=0\nDIRECT_HELLO_RC=0\n", encoding="utf-8")
            row = {"id": "cli-busybox", "name": "busybox", "kind": "cli", "image": "busybox:1.36"}
            scored = x86_run.score_linux(row, live_docker=False, suite_log=serial)
            self.assertEqual(scored["guest"], "pass")


if __name__ == "__main__":
    unittest.main()
