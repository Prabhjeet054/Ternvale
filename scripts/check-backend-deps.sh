#!/usr/bin/env bash
# Step 91: CLI, control API, and the app-bridge client must not depend on
# ternvale-vz or on Hypervisor.framework internals (ternvale-hv::ffi).
#
#   ./scripts/check-backend-deps.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

python3 - "$root" <<'PY'
import json
import os
import subprocess
import sys

root = sys.argv[1]
# Control-plane crates: talk to VMs through VmBackend / the socket, not VZ/HV FFI.
api_surface = ("ternvale-api", "ternvale-cli", "ternvale-client")
# doctor/peer/power on the CLI use public ternvale-hv wrappers only.
hv_direct_forbidden = ("ternvale-api", "ternvale-client")


def log(msg: str) -> None:
    print(f"check-backend-deps: {msg}", file=sys.stderr)


def fail(msg: str) -> None:
    log(msg)
    sys.exit(1)


meta = subprocess.check_output(
    ["cargo", "metadata", "--format-version", "1", "--offline", "--no-deps"],
    cwd=root,
    text=True,
)
data = json.loads(meta)
by_name = {pkg["name"]: pkg for pkg in data["packages"]}

missing = [name for name in api_surface if name not in by_name]
if missing:
    fail(f"workspace missing packages: {missing}")

errors = []
for name in api_surface:
    dep_names = {dep["name"] for dep in by_name[name].get("dependencies", [])}
    if "ternvale-vz" in dep_names:
        errors.append(f"{name} has a direct dependency on ternvale-vz")
    if name in hv_direct_forbidden and "ternvale-hv" in dep_names:
        errors.append(f"{name} has a direct dependency on ternvale-hv")
    else:
        log(f"{name}: no forbidden crate dependency")

# App / Swift must not link Hypervisor.framework or a VZ crate.
app_root = os.path.join(root, "app")
banned_needles = (
    "ternvale-vz",
    "ternvale-hv",
    "ternvale_hv",
    "Hypervisor.framework",
)
if os.path.isdir(app_root):
    for dirpath, dirnames, filenames in os.walk(app_root):
        dirnames[:] = [d for d in dirnames if d not in {".git", "DerivedData", "build"}]
        for filename in filenames:
            if not filename.endswith(
                (".swift", ".m", ".h", ".pbxproj", ".toml", ".rs")
            ):
                continue
            path = os.path.join(dirpath, filename)
            try:
                text = open(path, encoding="utf-8", errors="replace").read()
            except OSError as err:
                errors.append(f"read {path}: {err}")
                continue
            for needle in banned_needles:
                if needle in text:
                    rel = os.path.relpath(path, root)
                    errors.append(f"app bridge {rel} mentions {needle}")

# CLI may depend on ternvale-hv public APIs; FFI is crate-private internals.
cli_src = os.path.join(root, "ternvale-cli")
for dirpath, dirnames, filenames in os.walk(cli_src):
    dirnames[:] = [d for d in dirnames if d != "target"]
    for filename in filenames:
        if not filename.endswith(".rs"):
            continue
        path = os.path.join(dirpath, filename)
        text = open(path, encoding="utf-8", errors="replace").read()
        if "ternvale_hv::ffi" in text or "use ternvale_hv::ffi" in text:
            rel = os.path.relpath(path, root)
            errors.append(f"{rel} uses ternvale-hv internals (ffi)")

if errors:
    for item in errors:
        log(item)
    sys.exit(1)

log("ok: api/cli/client/app do not depend on ternvale-vz or hv internals")
PY
