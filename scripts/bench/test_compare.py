#!/usr/bin/env python3
"""Unit tests for scripts/bench/compare.py (no hypervisor)."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

import compare


def sample(eps: float, boot_ms: float, idle: float) -> dict:
    return {
        "hardware": {"model": "test"},
        "runs": {
            "ternvale": {
                "boot_to_login_ms": boot_ms,
                "sysbench_cpu_1_eps": eps,
                "sysbench_cpu_n_threads": 4,
                "host_idle_cpu_pct": idle,
                "iperf3_guest_to_host_mbps": None,
            },
            "qemu": {"sysbench_cpu_1_eps": eps * 1.1},
        },
    }


class CompareTests(unittest.TestCase):
    def fail_list(self, old: dict, new: dict, threshold: float = 10.0) -> list[str]:
        return compare.compare(old, new, threshold)

    def test_cpu_drop_over_threshold_is_regression(self) -> None:
        old, new = sample(1000, 2000, 90), sample(850, 2000, 90)
        failures = self.fail_list(old, new)
        self.assertTrue(any("sysbench_cpu_1_eps" in item for item in failures))

    def test_cpu_drop_under_threshold_passes(self) -> None:
        old, new = sample(1000, 2000, 90), sample(920, 2000, 90)
        self.assertEqual(self.fail_list(old, new, threshold=10), [])

    def test_boot_time_increase_is_regression(self) -> None:
        old, new = sample(1000, 2000, 90), sample(1000, 2500, 90)
        failures = self.fail_list(old, new)
        self.assertTrue(any("boot_to_login_ms" in item for item in failures))

    def test_boot_time_decrease_is_not_regression(self) -> None:
        old, new = sample(1000, 2000, 90), sample(1000, 1500, 90)
        self.assertEqual(self.fail_list(old, new), [])

    def test_fio_clat_ns_rise_is_regression(self) -> None:
        old = {"runs": {"qemu": {"fio_randwrite_clat_ns": 1000}}}
        new = {"runs": {"qemu": {"fio_randwrite_clat_ns": 2000}}}
        failures = self.fail_list(old, new)
        self.assertTrue(any("clat_ns" in item for item in failures))

    def test_mmio_dispatch_ns_rise_is_regression(self) -> None:
        old = {"runs": {"ternvale": {"mmio_dispatch_ns": 800}}}
        new = {"runs": {"ternvale": {"mmio_dispatch_ns": 1_000_000}}}
        failures = self.fail_list(old, new)
        self.assertTrue(any("mmio_dispatch_ns" in item for item in failures))

    def test_null_and_thread_count_are_skipped(self) -> None:
        old, new = sample(1000, 2000, 90), sample(1000, 2000, 90)
        new["runs"]["ternvale"]["sysbench_cpu_n_threads"] = 8
        self.assertEqual(self.fail_list(old, new), [])

    def test_cli_exits_nonzero_on_regression(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            old_path = Path(tmp) / "old.json"
            new_path = Path(tmp) / "new.json"
            old_path.write_text(json.dumps(sample(1000, 2000, 90)))
            new_path.write_text(json.dumps(sample(500, 2000, 90)))
            self.assertEqual(
                compare.main([str(old_path), str(new_path), "--threshold", "10"]),
                1,
            )
            self.assertEqual(
                compare.main([str(old_path), str(old_path)]),
                0,
            )


if __name__ == "__main__":
    unittest.main()
