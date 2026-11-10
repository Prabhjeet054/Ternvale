#!/usr/bin/env python3
"""COMPAT.md rendering (no hypervisor)."""

from __future__ import annotations

import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from report import build_document, render_markdown, write_outputs


class ReportTests(unittest.TestCase):
    def test_table_has_version_date_and_dash_boot(self) -> None:
        doc = build_document(
            [
                {
                    "id": "alpine-virt",
                    "name": "Alpine Linux",
                    "kernel": "virt",
                    "boot_ms": None,
                    "overall": "skip",
                    "features": {
                        "login": {"status": "skip", "reason": "no image"},
                        "net": {"status": "fail", "reason": "virtio-net disabled"},
                    },
                    "skip_reason": None,
                }
            ],
            features=["login", "net"],
            ternvale_version="0.1.0",
            date="2026-11-10T00:00:00Z",
            host="aarch64-apple-darwin",
        )
        md = render_markdown(doc)
        self.assertIn("Ternvale: `0.1.0`", md)
        self.assertIn("2026-11-10", md)
        self.assertIn("| — |", md)
        self.assertIn("**fail**", md)
        self.assertIn("alpine-virt", md)

    def test_write_outputs(self) -> None:
        doc = build_document(
            [],
            features=["login"],
            ternvale_version="0.1.0",
            date="2026-11-10T00:00:00Z",
            host="test",
        )
        with TemporaryDirectory() as tmp:
            base = Path(tmp)
            write_outputs(doc, base / "c.json", base / "COMPAT.md")
            self.assertTrue((base / "c.json").is_file())
            self.assertIn("compatibility", (base / "COMPAT.md").read_text(encoding="utf-8").lower())


if __name__ == "__main__":
    unittest.main()
