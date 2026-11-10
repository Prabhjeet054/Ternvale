#!/usr/bin/env python3
"""Skip vs fail for the compatibility matrix (no hypervisor)."""

from __future__ import annotations

import json
import unittest
from pathlib import Path

from checks import score_guest
from images import skip_reason

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def alpine() -> dict:
    matrix = json.loads((HERE / "matrix.json").read_text(encoding="utf-8"))
    return next(g for g in matrix["guests"] if g["id"] == "alpine-virt")


def windows() -> dict:
    matrix = json.loads((HERE / "matrix.json").read_text(encoding="utf-8"))
    return next(g for g in matrix["guests"] if g["id"] == "windows11-arm")


class ChecksTests(unittest.TestCase):
    def features(self) -> list[str]:
        return json.loads((HERE / "matrix.json").read_text(encoding="utf-8"))["features"]

    def test_missing_image_skips_all_not_fail(self) -> None:
        guest = alpine()
        guest = dict(guest)
        guest["images"] = {"disk": "test-assets/compat/does-not-exist.img"}
        reason = skip_reason(ROOT, guest)
        self.assertIsNotNone(reason)
        row = score_guest(self.features(), guest, "", guest_skip=reason)
        self.assertEqual(row["overall"], "skip")
        for feat in row["features"].values():
            self.assertEqual(feat["status"], "skip")

    def test_windows_share_and_agent_are_skip(self) -> None:
        serial = "Getting Windows ready\nviostor\nNetKVM\nviogpu\nvioinput\nvirtio-snd\nShutdown\n"
        row = score_guest(self.features(), windows(), serial)
        self.assertEqual(row["features"]["share"]["status"], "skip")
        self.assertEqual(row["features"]["agent"]["status"], "skip")
        self.assertEqual(row["features"]["login"]["status"], "pass")
        self.assertNotEqual(row["features"]["share"]["status"], "fail")

    def test_broken_net_is_fail_not_skip(self) -> None:
        serial = "ternvale root ready\nvda ready\nPower down\n"
        row = score_guest(
            self.features(),
            alpine(),
            serial,
            nic_enabled=False,
        )
        self.assertEqual(row["features"]["net"]["status"], "fail")
        self.assertIn("disabled", row["features"]["net"]["reason"])
        self.assertEqual(row["features"]["login"]["status"], "pass")

    def test_compat_break_net(self) -> None:
        serial = "ternvale root ready\nvda ready\nPING 10.0.0.2\nPower down\n"
        row = score_guest(
            self.features(),
            alpine(),
            serial,
            break_features=frozenset({"net"}),
        )
        self.assertEqual(row["features"]["net"]["status"], "fail")
        self.assertEqual(row["features"]["login"]["status"], "pass")

    def test_empty_serial_supported_guest_fails_login(self) -> None:
        row = score_guest(self.features(), alpine(), "")
        self.assertEqual(row["overall"], "fail")
        self.assertEqual(row["features"]["login"]["status"], "fail")


if __name__ == "__main__":
    unittest.main()
