#!/usr/bin/env python3
"""Attestation layout + secret-scan tests (no WDK, no certificates)."""

import pathlib
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import attestation_layout  # noqa: E402
import infverif_dch  # noqa: E402
import secrets_grep  # noqa: E402
import stamp  # noqa: E402


class PackageTests(unittest.TestCase):
    def test_source_infs_infverif_analog_clean(self) -> None:
        for rel in ("drivers/tvhello/tvhello.inf", "drivers/tvgpu-dod/tvgpu-dod.inf"):
            path = stamp.ROOT / rel
            findings = infverif_dch.check_inf(path)
            self.assertEqual(
                [f.message for f in findings],
                [],
                rel,
            )

    def test_attestation_fixture_is_valid_and_rejects_pfx(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp) / "stage"
            attestation_layout.write_fixture(root)
            self.assertEqual(attestation_layout.validate_tree(root), [])
            (root / "tvhello" / "leak.pfx").write_bytes(b"nope")
            errors = attestation_layout.validate_tree(root)
            self.assertTrue(any("secret" in e for e in errors), errors)

    def test_attestation_rejects_long_folder_name(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            name = "n" * 40
            folder = root / name
            folder.mkdir()
            for ext in (".sys", ".inf", ".pdb", ".cat"):
                (folder / f"x{ext}").write_bytes(b"x")
            errors = attestation_layout.validate_tree(root)
            self.assertTrue(any("longer" in e for e in errors), errors)

    def test_repo_has_no_private_key_material(self) -> None:
        hits = secrets_grep.scan(stamp.ROOT)
        self.assertEqual(hits, [])


if __name__ == "__main__":
    unittest.main()
