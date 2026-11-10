#!/usr/bin/env python3
"""The Hypervisor workflow must not schedule self-hosted jobs for fork PRs."""

from __future__ import annotations

import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
YAML = ROOT / ".github" / "workflows" / "hypervisor.yml"


class ForkGuardTests(unittest.TestCase):
    def test_self_hosted_job_skips_forks(self) -> None:
        text = YAML.read_text(encoding="utf-8")
        self.assertIn("runs-on: [self-hosted, macOS, ARM64, ternvale]", text)
        self.assertIn(
            "github.event.pull_request.head.repo.full_name == github.repository",
            text,
        )
        self.assertIn(
            "github.event.pull_request.head.repo.full_name != github.repository",
            text,
        )
        self.assertNotIn("pull_request_target", text)
        self.assertIn("skip fork PRs", text)


if __name__ == "__main__":
    unittest.main()
