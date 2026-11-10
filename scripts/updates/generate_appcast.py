#!/usr/bin/env python3
"""Write a Sparkle 2 RSS appcast with sparkle:edSignature on each enclosure.

Signing uses PyNaCl when SPARKLE_ED_PRIVATE_KEY (32-byte seed, base64) is set,
or --key-file. Without a key the XML is still emitted so CI can lint structure;
Sparkle will refuse to install unsigned items.

The zip must already be Developer ID signed and notarized (scripts/release.sh).
EdDSA is an extra check on top of Gatekeeper, not a replacement.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from xml.sax.saxutils import escape

# PKCS#8 Ed25519 private prefix (RFC 8410) + 32-byte seed.
_ED25519_PKCS8_PREFIX = bytes.fromhex("302e020100300506032b657004220420")
_ED25519_SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")


def _seed(key_b64: str) -> bytes:
    raw = base64.b64decode(key_b64)
    if len(raw) not in (32, 64):
        raise SystemExit("generate-appcast: Ed25519 seed must be 32 or 64 bytes")
    return raw[:32]


def _pem_private(seed: bytes) -> bytes:
    der = _ED25519_PKCS8_PREFIX + seed
    b64 = base64.encodebytes(der).decode("ascii")
    return f"-----BEGIN PRIVATE KEY-----\n{b64}-----END PRIVATE KEY-----\n".encode("ascii")


def _pem_public(pub: bytes) -> bytes:
    der = _ED25519_SPKI_PREFIX + pub
    b64 = base64.encodebytes(der).decode("ascii")
    return f"-----BEGIN PUBLIC KEY-----\n{b64}-----END PUBLIC KEY-----\n".encode("ascii")


def _openssl(*args: str, stdin: bytes | None = None) -> bytes:
    proc = subprocess.run(
        ["openssl", *args],
        input=stdin,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        raise SystemExit(f"generate-appcast: openssl {' '.join(args)}: {proc.stderr.decode()}")
    return proc.stdout


def ed_sign(data: bytes, key_b64: str) -> str:
    try:
        from nacl.signing import SigningKey

        sig = SigningKey(_seed(key_b64)).sign(data).signature
        return base64.b64encode(sig).decode("ascii")
    except ImportError:
        pass
    seed = _seed(key_b64)
    with tempfile.TemporaryDirectory() as tmp:
        pem = Path(tmp) / "ed.pem"
        src = Path(tmp) / "blob"
        sigp = Path(tmp) / "sig"
        pem.write_bytes(_pem_private(seed))
        src.write_bytes(data)
        _openssl("pkeyutl", "-sign", "-inkey", str(pem), "-rawin", "-in", str(src), "-out", str(sigp))
        return base64.b64encode(sigp.read_bytes()).decode("ascii")


def ed_verify(data: bytes, signature_b64: str, public_b64: str) -> bool:
    try:
        from nacl.exceptions import BadSignatureError
        from nacl.signing import VerifyKey

        VerifyKey(base64.b64decode(public_b64)).verify(data, base64.b64decode(signature_b64))
        return True
    except ImportError:
        pass
    except Exception:
        return False
    pub = base64.b64decode(public_b64)
    sig = base64.b64decode(signature_b64)
    with tempfile.TemporaryDirectory() as tmp:
        pem = Path(tmp) / "pub.pem"
        src = Path(tmp) / "blob"
        sigp = Path(tmp) / "sig"
        pem.write_bytes(_pem_public(pub))
        src.write_bytes(data)
        sigp.write_bytes(sig)
        proc = subprocess.run(
            [
                "openssl",
                "pkeyutl",
                "-verify",
                "-pubin",
                "-inkey",
                str(pem),
                "-rawin",
                "-in",
                str(src),
                "-sigfile",
                str(sigp),
            ],
            capture_output=True,
        )
        return proc.returncode == 0


def generate_keypair() -> tuple[str, str]:
    """Return (private_seed_b64, public_key_b64) for Sparkle `SUPublicEDKey`."""
    try:
        from nacl.signing import SigningKey

        key = SigningKey.generate()
        return (
            base64.b64encode(key.encode()).decode("ascii"),
            base64.b64encode(key.verify_key.encode()).decode("ascii"),
        )
    except ImportError:
        der = _openssl("genpkey", "-algorithm", "ED25519", "-outform", "DER")
        seed = der[-32:]
        pub_der = _openssl("pkey", "-pubout", "-outform", "DER", stdin=_pem_private(seed))
        pub = pub_der[-32:]
        return base64.b64encode(seed).decode("ascii"), base64.b64encode(pub).decode("ascii")


def render(
    *,
    url: str,
    version: str,
    build: str,
    length: int,
    signature: str,
    channel: str | None,
    notes: str,
) -> str:
    channel_xml = (
        f"      <sparkle:channel>{escape(channel)}</sparkle:channel>\n" if channel else ""
    )
    return f"""<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>Ternvale</title>
    <item>
      <title>Ternvale {escape(version)}</title>
      <sparkle:version>{escape(build)}</sparkle:version>
      <sparkle:shortVersionString>{escape(version)}</sparkle:shortVersionString>
{channel_xml}      <description><![CDATA[{notes}]]></description>
      <enclosure url="{escape(url)}"
                 length="{length}"
                 type="application/octet-stream"
                 sparkle:edSignature="{escape(signature)}" />
    </item>
  </channel>
</rss>
"""


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--zip", type=Path, default=None)
    p.add_argument("--version", default="")
    p.add_argument("--build", default="")
    p.add_argument("--channel", choices=("stable", "beta"), default="stable")
    p.add_argument("--url", default="")
    p.add_argument("--out", type=Path, default=Path("appcast.xml"))
    p.add_argument("--key-file", type=Path, default=None)
    p.add_argument("--notes", default="See the GitHub release notes.")
    p.add_argument("--gen-keys", action="store_true", help="print seed+public base64 and exit")
    args = p.parse_args(argv)
    if args.gen_keys:
        priv, pub = generate_keypair()
        print(f"SPARKLE_ED_PRIVATE_KEY={priv}")
        print(f"SUPublicEDKey={pub}")
        return 0
    if args.zip is None or not args.version or not args.build:
        raise SystemExit("generate-appcast: --zip --version --build are required")

    blob = args.zip.read_bytes()
    key = os.environ.get("SPARKLE_ED_PRIVATE_KEY", "")
    if args.key_file is not None:
        key = args.key_file.read_text(encoding="utf-8").strip()
    if key:
        signature = ed_sign(blob, key)
    else:
        # Structural placeholder — Sparkle and Appcast.requireSignatures reject empty.
        signature = base64.b64encode(hashlib.sha256(blob).digest()).decode("ascii")
        print("generate-appcast: no SPARKLE_ED_PRIVATE_KEY; wrote digest placeholder (not installable)", file=sys.stderr)

    url = args.url or f"https://updates.ternvale.dev/{args.zip.name}"
    channel = None if args.channel == "stable" else "beta"
    xml = render(
        url=url,
        version=args.version,
        build=args.build,
        length=len(blob),
        signature=signature,
        channel=channel,
        notes=args.notes,
    )
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(xml, encoding="utf-8")
    print(args.out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
