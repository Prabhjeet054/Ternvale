#!/usr/bin/env python3
import tempfile
import unittest
from pathlib import Path

from generate_appcast import main, render


class GenerateAppcastTests(unittest.TestCase):
    def test_beta_channel_element(self):
        xml = render(
            url="https://updates.ternvale.dev/Ternvale.zip",
            version="0.2.0-beta.1",
            build="2",
            length=10,
            signature="SIG",
            channel="beta",
            notes="n",
        )
        self.assertIn("<sparkle:channel>beta</sparkle:channel>", xml)
        self.assertIn('sparkle:edSignature="SIG"', xml)

    def test_stable_omits_channel(self):
        xml = render(
            url="https://updates.ternvale.dev/Ternvale.zip",
            version="0.2.0",
            build="2",
            length=10,
            signature="SIG",
            channel=None,
            notes="n",
        )
        self.assertNotIn("sparkle:channel", xml)

    def test_writes_file_without_key(self):
        with tempfile.TemporaryDirectory() as tmp:
            z = Path(tmp) / "Ternvale.zip"
            z.write_bytes(b"zip-bytes")
            out = Path(tmp) / "appcast.xml"
            rc = main(
                [
                    "--zip",
                    str(z),
                    "--version",
                    "0.2.0",
                    "--build",
                    "2",
                    "--out",
                    str(out),
                ]
            )
            self.assertEqual(rc, 0)
            text = out.read_text(encoding="utf-8")
            self.assertIn("sparkle:edSignature=", text)
            self.assertIn("Ternvale.zip", text)


if __name__ == "__main__":
    unittest.main()
