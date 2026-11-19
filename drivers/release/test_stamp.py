#!/usr/bin/env python3
"""Unit tests for INF stamping and DCH checks (no WDK)."""

import pathlib
import sys
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import infverif_dch  # noqa: E402
import stamp  # noqa: E402


SAMPLE = """\
[Version]
Signature   = "$WINDOWS NT$"
Class       = System
ClassGuid   = {4D36E97D-E325-11CE-BFC1-08002BE10318}
Provider    = %ManufacturerName%
CatalogFile = tvhello.cat
PnpLockdown = 1
DriverVer   = 01/01/2020,1.0.0.0
DriverPackageType = PlugAndPlay
DriverPackageDisplayName = %PackageName%

[Manufacturer]
%ManufacturerName% = Standard,NTarm64
"""


class StampTests(unittest.TestCase):
    def test_workspace_version(self) -> None:
        ver = stamp.workspace_version()
        self.assertRegex(ver, r"^\d+\.\d+\.\d+")

    def test_inf_version_maps_semver(self) -> None:
        self.assertEqual(stamp.inf_version("0.1.0"), "10.0.1.0")
        self.assertEqual(stamp.inf_version("1.2.3-beta"), "10.1.2.3")

    def test_stamp_replaces_driverver(self) -> None:
        out = stamp.stamp_inf(SAMPLE, "0.1.0", "11/19/2026")
        self.assertIn("DriverVer   = 11/19/2026,10.0.1.0", out)
        self.assertNotIn("01/01/2020", out)

    def test_dch_sample_is_ok(self) -> None:
        path = HERE / "_sample.inf"
        path.write_text(SAMPLE, encoding="utf-8")
        try:
            findings = infverif_dch.check_inf(path)
            errors = [f.message for f in findings if f.level == "error"]
            self.assertEqual(errors, [])
        finally:
            path.unlink(missing_ok=True)

    def test_dch_rejects_coinstaller(self) -> None:
        path = HERE / "_bad.inf"
        path.write_text(SAMPLE + "\n[Foo.CoInstallers]\nCopyFiles=x\n", encoding="utf-8")
        try:
            findings = infverif_dch.check_inf(path)
            self.assertTrue(any("co-installer" in f.message for f in findings))
        finally:
            path.unlink(missing_ok=True)

    def test_source_infs_pass_dch(self) -> None:
        root = stamp.ROOT
        for rel in ("drivers/tvhello/tvhello.inf", "drivers/tvgpu-dod/tvgpu-dod.inf"):
            findings = infverif_dch.check_inf(root / rel)
            errors = [f.message for f in findings if f.level == "error"]
            self.assertEqual(errors, [], rel)


if __name__ == "__main__":
    unittest.main()
