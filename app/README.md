# Ternvale macOS app

SwiftUI shell for the VM library under `~/Library/Application Support/Ternvale/vms/`.

## Features

- Lists VMs by reading each `config.toml`
- **New VM** scaffolds `~/Library/Application Support/Ternvale/vms/<name>/`
- **Start** launches the bundled `ternvale` helper with `run --display window` and `TERNVALE_EMBEDDED_DISPLAY=1` (presenter socket only; no dev viewer subprocess)
- **Metal display** in the VM detail pane: TVDP client on `~/Library/Application Support/Ternvale/run/<name>.display.sock`, keyboard/mouse via virtio-input
- **Resize** sends `SET_SIZE` on the display socket; Linux guests with `virtio_gpu` + modeset typically pick up `VIRTIO_GPU_EVENT_DISPLAY` and re-scanout. **Windows** guests do not yet have a dynamic-resolution driver in-tree—resize may not change the guest framebuffer until a driver is added.
- **Stop / Pause / Resume** use the Step 28 Unix control socket (`~/Library/Application Support/Ternvale/run/<name>.sock`)
- **Host log** panel tails `~/Library/Logs/Ternvale/ternvale-<name>-*.log`
- **Crashed** state when the helper dies without a clean stop; reads Step 30 sidecars (`.summary.txt`, `.run.json`) for a short error summary

The Step 44 **dev viewer** (`ternvale run --display window` without `TERNVALE_EMBEDDED_DISPLAY`) remains for CLI/headless testing.

## Build

```sh
cargo build -p ternvale-cli
cd app/Ternvale
xcodebuild -scheme Ternvale -configuration Debug -destination 'platform=macOS' build
open ~/Library/Developer/Xcode/DerivedData/*/Build/Products/Debug/Ternvale.app
```

The **Embed ternvale helper** build phase copies and ad-hoc signs `target/debug/ternvale` into `Contents/Helpers/ternvale`.

Override the helper path when running from Xcode without embedding:

```sh
export TERNVALE_HELPER_PATH=/path/to/ternvale
```

## Library layout

Same as the CLI: `~/Library/Application Support/Ternvale/vms/<name>/config.toml`.
