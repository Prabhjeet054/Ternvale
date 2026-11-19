# Driver packaging (`drivers/release`)

Stamps INF `DriverVer` from the Cargo workspace version (`0.1.0` →
`10.0.1.0`), checks DCH Universal INF rules, builds catalogs, test-signs for
development, and prepares a Partner Center attestation CAB.

Certificates, PFX files, and passwords are **never** stored in this tree
(see `drivers/.gitignore`).

## Development (test-signing)

On Windows 11 ARM64 + WDK:

```powershell
$env:TERNVALE_TESTSIGN_PFX = "C:\secure\not-in-git.pfx"   # optional
.\drivers\release\package.ps1
```

Guests need `bcdedit /set testsigning on`. Production / Secure Boot guests
need the attestation package instead (`ATTESTATION.md`).

On macOS (no WDK):

```sh
python3 drivers/release/infverif_dch.py --werror drivers/tvhello/tvhello.inf drivers/tvgpu-dod/tvgpu-dod.inf
python3 -m unittest discover -s drivers/release -p 'test_*.py' -v
./scripts/driver-package-live-check.sh
```

## Tools ISO

`ternvale tools build-iso` copies `drivers/release/dist` (if present) to
`ternvale-drivers/` on the ISO and records `drivers_version` (`tvgpu-<semver>`)
in `MANIFEST.json`. Guests whose Hello string does not include that stamp are
flagged **tools outdated**.
