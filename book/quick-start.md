# Quick start

Requirements: **Apple Silicon**, **macOS 14+**, and a Ternvale binary signed with
`com.apple.security.hypervisor`. There is no x86 guest support.

## Install (app)

1. Download a **Developer ID, notarized** `Ternvale.zip` from a release you trust.
   An Apple Development or ad-hoc signature will **not** pass Gatekeeper
   (`spctl -a -vvv --type execute` is rejected until notarize + staple).
2. Open the zip, drag **Ternvale.app** to `/Applications`.
3. First launch: System Settings may ask you to allow the hypervisor helper.
4. The CLI is inside the bundle (it is not on `PATH` by default):

```sh
export PATH="/Applications/Ternvale.app/Contents/Helpers:$PATH"
ternvale doctor
```

From source (developers): `cargo build --workspace`, or
`./scripts/release.sh --skip-notarize`. That dry-run still needs a codesign
identity; Gatekeeper stays red until Developer ID + `notarytool` + staple.
Check a bundle with `ternvale release-verify --app /path/to/Ternvale.app`
(add `--expect-gatekeeper` after notarization).

## First Linux VM (app)

The sidebar button is **New VM** (sheet title **New VM**), not “Create VM”.

1. Click **New VM**. Leave **Windows 11 (unattended)** off.
2. Enter a name (`alpine`). Optionally set **Kernel path** to a real ARM64
   `Image`. If you leave it empty, the config writes `kernel = "/path/to/Image"`
   and **Start will fail** until you edit `config.toml`.
3. The app does **not** let you pick a disk image. It writes
   `~/Library/Application Support/Ternvale/vms/<name>/disks/disk0.img` in the
   TOML; that file does not exist yet. Create it:

```sh
ternvale create-disk "$HOME/Library/Application Support/Ternvale/vms/alpine/disks/disk0.img" 8G
```

4. Point `kernel` / `initrd` / `cmdline` (or `firmware`) at files you supply.
   Ternvale does not download a distro.
5. Select the VM → **Start**. Serial is in the log panel; host logs are under
   `~/Library/Logs/Ternvale/`.

Default app template is **1 vCPU / 256 MiB**. Raise `cpus` and `ram_mib` in
`config.toml` for a real distro.

## First Linux VM (CLI)

```sh
mkdir -p ~/VMs
ternvale create-disk ~/VMs/alpine.img 8G
# Write alpine.toml (see Linux VMs). You must provide kernel or firmware.
ternvale validate alpine.toml
ternvale run alpine.toml
```

Library VMs live under `~/Library/Application Support/Ternvale/vms/`.

## Updates

**Ternvale → Check for Updates…**, or **Ternvale → Settings**, or
**Help → Crash & Update Preferences…**. Channel is **Stable** / **Beta**.
**Download and install updates automatically** is off until you enable it.
Sparkle needs a real `SUPublicEDKey` (not the all-zero placeholder in-tree)
plus a notarized zip.

## Crash reports

Off by default (**Allow sending crash reports**). After a crash you get a
preview of every file and field. Guest serial and guest disks are omitted.
Canonical policy: `docs/PRIVACY.md` in the source tree.
