# Troubleshooting

## The VM dies immediately (exit 9 / SIGKILL)

The helper was killed by **AMFI**. Usual causes:

1. Entitlements include `com.apple.vm.device-access` without a USB provisioning
   profile. The cargo runner defaults to `entitlements/ternvale-hv-only.entitlements`.
2. Codesign identity is another person’s Apple Development cert in the Keychain.
   `sign-and-run.sh` uses `scripts/lib-codesign.sh`: Developer ID, then
   `TERNVALE_TEAM_ID`, then `TERNVALE_CODESIGN_EMAIL` (default
   `prabhjeetsingh054@gmail.com`).
3. Ad-hoc `codesign -s -`.

```sh
./scripts/sign-and-run.sh target/release/ternvale run config.toml
```

`codesign --verify --deep --strict` can still pass on an Apple Development
build while `spctl -a -vvv --type execute` **rejects** it. Gatekeeper needs a
**Developer ID Application** certificate (Apple Developer Program), then
`notarytool` + staple. This Keychain has only Apple Development identities
until you create that cert. `./scripts/require-developer-id.sh` checks.
`ternvale release-verify --app Ternvale.app --expect-gatekeeper` after staple.

## “Control socket went stale”

The VMM process exited. Open `~/Library/Logs/Ternvale/ternvale-<vm>-*.log` and
the `.summary.txt` sidecar. The app offers an opt-in crash report preview.

## New Linux VM will not Start

- Kernel still `/path/to/Image` — set a real ARM64 kernel or `firmware`.
- `disk0.img` path exists in TOML but the file does not —
  `ternvale create-disk … 8G`.
- App template is 1 CPU / 256 MiB — raise them in `config.toml`.

## `ternvale create --windows` fails on a clean Mac

Need `--unattended`, a real ISO, a password env/file, **and** firmware
(`--firmware` or `TERNVALE_FIRMWARE`). Build `ternvale tools build-iso --fetch`
for virtio drivers. Interactive Setup is not implemented.

## No network

- `loopback` cannot reach the internet.
- `user` should ping `10.0.2.2`.
- `vmnet` needs the privileged helper. Bridged mode needs an allowed Ethernet
  interface.

## Display is black

UEFI guests need firmware (`QEMU_EFI.fd`). Direct-kernel guests need a console
on `ttyAMA0` or virtio-gpu. `ternvale run --display window` is a **developer**
viewer (`make viewer`), not the shipping app window.

## Windows Setup cannot see the disk

Load `viostor` from the virtio-win / tools ISO at the “load driver” prompt.

## Shared folder rejected at validate

At most one `[[shares]]`. `host_path` must be an existing directory. You must
have `[vsock]` with `agent = true`. There is no share UI in the app.

## Sparkle does not install an update

In-tree `SUPublicEDKey` is all zeros until `generate_keys`. Automatic install
stays off unless **Download and install updates automatically** is enabled.
The zip must still be Developer ID notarized.

## More logs

```sh
TERNVALE_LOG=debug ternvale run config.toml
```

Targets include `ternvale::hv`, `ternvale::vcpu`, `ternvale::virtio::blk`,
`ternvale::net`, `ternvale::virtiofs`. Never log secrets (keys, TPM state).
