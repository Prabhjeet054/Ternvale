# Ternvale macOS app

SwiftUI shell for the VM library under `~/Library/Application Support/Ternvale/vms/`.

## Features

- Lists VMs by reading each `config.toml`
- **New VM** scaffolds `~/Library/Application Support/Ternvale/vms/<name>/`
- **Start** launches the bundled `ternvale` helper (`ternvale run <config>`) as a subprocess; UI polls until **Running**
- **Stop / Pause / Resume** use the Step 28 Unix control socket (`~/Library/Application Support/Ternvale/run/<name>.sock`)
- **Host log** panel tails `~/Library/Logs/Ternvale/ternvale-<name>-*.log`
- **Crashed** state when the helper dies without a clean stop; reads Step 30 sidecars (`.summary.txt`, `.run.json`) for a short error summary

Display output is not implemented here; use the Step 44 dev viewer (`ternvale run --display window`) from the CLI when you need a window.

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
