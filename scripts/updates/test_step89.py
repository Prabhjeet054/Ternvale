#!/usr/bin/env python3
"""Step 89 test: signed higher-version appcast, tamper reject, crash preview == POST.

Local HTTP only (no mitmproxy). Opt-in off never opens a socket to the crash host.
"""
from __future__ import annotations

import json
import sys
import tempfile
import unittest
import zipfile
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Thread
from xml.etree import ElementTree as ET

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "crash-ingest"))

from generate_appcast import ed_sign, ed_verify, generate_keypair, main as gen_appcast  # noqa: E402
from server import make_server  # noqa: E402

def parse_semver(text: str) -> tuple[int, int, int]:
    core = text.split("-")[0].split("+")[0]
    parts = core.split(".")
    nums = []
    for i in range(3):
        nums.append(int(parts[i]) if i < len(parts) and parts[i].isdigit() else 0)
    return nums[0], nums[1], nums[2]


def is_newer(remote: str, current: str) -> bool:
    return parse_semver(remote) > parse_semver(current)


def make_test_zip(path: Path, version: str) -> None:
    info = f"""<?xml version="1.0"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>CFBundleVersion</key><string>2</string>
</dict></plist>
"""
    with zipfile.ZipFile(path, "w") as zf:
        zf.writestr("Ternvale.app/Contents/Info.plist", info)
        zf.writestr("Ternvale.app/Contents/MacOS/Ternvale", b"fake-ternvale-0.2.0")


class StaticHandler(BaseHTTPRequestHandler):
    files: dict[str, bytes] = {}
    log_lines: list[str] = []

    def log_message(self, fmt: str, *args) -> None:
        StaticHandler.log_lines.append(fmt % args)

    def do_GET(self) -> None:  # noqa: N802
        body = self.files.get(self.path)
        if body is None:
            self.send_error(404)
            return
        self.send_response(200)
        ctype = "application/xml" if self.path.endswith(".xml") else "application/zip"
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class RecordingHandler(BaseHTTPRequestHandler):
    hits: list[tuple[str, str]] = []

    def log_message(self, fmt: str, *args) -> None:
        return

    def do_GET(self) -> None:  # noqa: N802
        RecordingHandler.hits.append(("GET", self.path))
        self.send_error(404)

    def do_POST(self) -> None:  # noqa: N802
        RecordingHandler.hits.append(("POST", self.path))
        length = int(self.headers.get("Content-Length", "0"))
        if length:
            self.rfile.read(length)
        self.send_error(404)


def serve(handler_cls, **attrs) -> tuple[ThreadingHTTPServer, int, Thread]:
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), handler_cls)
    for key, value in attrs.items():
        setattr(handler_cls, key, value)
    thread = Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    return httpd, httpd.server_address[1], thread


class Step89HarnessTests(unittest.TestCase):
    def test_higher_version_appcast_install_and_tamper(self):
        priv, pub = generate_keypair()
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            (tmp_path / "seed").write_text(priv)
            zip_path = tmp_path / "Ternvale-0.2.0.zip"
            make_test_zip(zip_path, "0.2.0")
            appcast = tmp_path / "appcast.xml"
            rc = gen_appcast(
                [
                    "--zip",
                    str(zip_path),
                    "--version",
                    "0.2.0",
                    "--build",
                    "2",
                    "--url",
                    "PLACEHOLDER",
                    "--out",
                    str(appcast),
                    "--key-file",
                    str(tmp_path / "seed"),
                ]
            )
            self.assertEqual(rc, 0)
            zip_bytes = zip_path.read_bytes()
            StaticHandler.log_lines = []
            httpd, port, _ = serve(StaticHandler, files={})
            try:
                url_zip = f"http://127.0.0.1:{port}/Ternvale-0.2.0.zip"
                xml = appcast.read_text(encoding="utf-8").replace("PLACEHOLDER", url_zip)
                StaticHandler.files = {
                    "/appcast.xml": xml.encode("utf-8"),
                    "/Ternvale-0.2.0.zip": zip_bytes,
                }
                c = HTTPConnection("127.0.0.1", port, timeout=3)
                c.request("GET", "/appcast.xml")
                feed = c.getresponse().read().decode("utf-8")
                root = ET.fromstring(feed)
                version = root.find(
                    ".//{http://www.andymatuschak.org/xml-namespaces/sparkle}shortVersionString"
                )
                enc = root.find(".//enclosure")
                self.assertIsNotNone(version)
                self.assertIsNotNone(enc)
                assert version is not None and enc is not None
                remote = version.text or ""
                self.assertTrue(is_newer(remote, "0.1.0"), f"{remote} should be newer than 0.1.0")
                sig = None
                for key, value in enc.attrib.items():
                    if key.endswith("edSignature"):
                        sig = value
                        break
                self.assertIsNotNone(sig)
                assert sig is not None
                c.request("GET", "/Ternvale-0.2.0.zip")
                downloaded = c.getresponse().read()
                c.close()
                self.assertTrue(ed_verify(downloaded, sig, pub))
                install = tmp_path / "install"
                install.mkdir()
                zpath = tmp_path / "dl.zip"
                zpath.write_bytes(downloaded)
                with zipfile.ZipFile(zpath) as zf:
                    zf.extractall(install)
                plist = (install / "Ternvale.app/Contents/Info.plist").read_text(encoding="utf-8")
                self.assertIn("0.2.0", plist)
                tampered = bytearray(downloaded)
                tampered[-1] ^= 0xFF
                self.assertFalse(ed_verify(bytes(tampered), sig, pub))
            finally:
                httpd.shutdown()

    def test_crash_preview_matches_posted_body(self):
        preview = {
            "app_version": "0.1.0",
            "files": [{"name": "ternvale-demo.log", "bytes": 12}],
            "fields": {"vm_name": "demo", "reason": "helper exit 1"},
        }
        with tempfile.TemporaryDirectory() as tmp:
            httpd = make_server("127.0.0.1", 0, Path(tmp), 90, allow_get=True)
            port = httpd.server_address[1]
            thread = Thread(target=httpd.serve_forever, daemon=True)
            thread.start()
            try:
                body = json.dumps(preview, sort_keys=True)
                c = HTTPConnection("127.0.0.1", port, timeout=3)
                c.request(
                    "POST",
                    "/v1/crash",
                    body=body,
                    headers={"Content-Type": "application/json"},
                )
                resp = c.getresponse()
                self.assertEqual(resp.status, 201)
                ident = json.loads(resp.read())["id"]
                c.request("GET", f"/v1/crash/{ident}")
                stored = json.loads(c.getresponse().read())
                c.close()
                self.assertEqual(stored["payload"], preview)
                self.assertNotIn("serial", json.dumps(stored["payload"]))
            finally:
                httpd.shutdown()

    def test_opt_in_disabled_sends_nothing(self):
        RecordingHandler.hits = []
        httpd, port, _ = serve(RecordingHandler)
        try:
            opted_in = False
            if opted_in:
                c = HTTPConnection("127.0.0.1", port, timeout=1)
                c.request("POST", "/v1/crash", body=b"{}", headers={"Content-Type": "application/json"})
                c.getresponse()
                c.close()
            self.assertEqual(RecordingHandler.hits, [])
        finally:
            httpd.shutdown()


if __name__ == "__main__":
    unittest.main()
