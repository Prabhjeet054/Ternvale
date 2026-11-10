#!/usr/bin/env python3
import json
import tempfile
import time
import unittest
from http.client import HTTPConnection
from pathlib import Path
from threading import Thread

from server import make_server, purge


class CrashIngestTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.httpd = make_server("127.0.0.1", 0, self.dir, 90, allow_get=True)
        self.port = self.httpd.server_address[1]
        self.thread = Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.httpd.shutdown()
        self.tmp.cleanup()

    def conn(self) -> HTTPConnection:
        return HTTPConnection("127.0.0.1", self.port, timeout=2)

    def test_health_and_post_roundtrip(self):
        c = self.conn()
        c.request("GET", "/health")
        health = json.loads(c.getresponse().read())
        self.assertTrue(health["ok"])
        self.assertEqual(health["retention_days"], 90)
        body = json.dumps({"app_version": "0.1.0", "files": [{"name": "a.log", "bytes": 1}]})
        c = self.conn()
        c.request("POST", "/v1/crash", body=body, headers={"Content-Type": "application/json"})
        resp = c.getresponse()
        self.assertEqual(resp.status, 201)
        created = json.loads(resp.read())
        ident = created["id"]
        stored = list(self.dir.glob("crash-*.json"))
        self.assertEqual(len(stored), 1)
        record = json.loads(stored[0].read_text())
        self.assertEqual(record["magic"], "TVCR")
        self.assertEqual(record["version"], 1)
        self.assertEqual(record["payload"]["app_version"], "0.1.0")
        c = self.conn()
        c.request("GET", f"/v1/crash/{ident}")
        got = json.loads(c.getresponse().read())
        self.assertEqual(got["id"], ident)

    def test_reject_huge_and_bad_json(self):
        c = self.conn()
        c.request("POST", "/v1/crash", body=b"{", headers={"Content-Type": "application/json"})
        self.assertEqual(c.getresponse().status, 400)
        c.close()
        c = self.conn()
        c.request(
            "POST",
            "/v1/crash",
            body=b"",
            headers={"Content-Type": "application/json", "Content-Length": "0"},
        )
        self.assertEqual(c.getresponse().status, 413)
        c.close()
        import socket

        s = socket.create_connection(("127.0.0.1", self.port), timeout=2)
        s.sendall(
            b"POST /v1/crash HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 3000000\r\n\r\n"
        )
        data = s.recv(4096)
        s.close()
        self.assertIn(b"413", data.split(b"\r\n", 1)[0])

    def test_purge_old_files(self):
        old = self.dir / "crash-old.json"
        old.write_text("{}")
        past = time.time() - 91 * 86400
        # utime may fail on some FS; still assert purge logic with injected now.
        n = purge(self.dir, 90, now=past + 1)
        # file mtime is ~now, so with now=past+1 it looks new... use now far future
        n = purge(self.dir, 90, now=time.time() + 200 * 86400)
        self.assertEqual(n, 1)
        self.assertFalse(old.exists())


if __name__ == "__main__":
    unittest.main()
