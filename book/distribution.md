# Distribution and licenses

Ternvale is distributed **outside** the Mac App Store (Developer ID +
notarization). See `docs/DISTRIBUTION.md` for the comparison with the Store
(sandbox, hypervisor entitlements, vmnet, updates, payment).

Licenses are offline Ed25519 files (`TVLK`). The app holds only the public
key. A 14-day trial start is stored in the Keychain. After expiry (and a
grace period) you can still **open, export, and back up** VMs; creating or
starting new sessions is refused. License checks send **nothing** unless you
set `TERNVALE_LICENSE_ACTIVATE_URL` (`docs/PRIVACY.md`).

```sh
ternvale license show
ternvale license install ./LIC-001.tvlk
```
