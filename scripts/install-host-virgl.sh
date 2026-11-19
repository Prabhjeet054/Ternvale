#!/usr/bin/env bash
# Install host Venus pieces: Homebrew MoltenVK plus virglrenderer built with -Dvenus=true.
# Does not vendor binaries into git (see THIRD_PARTY_NOTICES).
set -euo pipefail

log() { printf 'install-host-virgl: %s\n' "$*" >&2; }

script_dir="$(cd "$(dirname "$0")" && pwd)"
prefix="${TERNVALE_HOST_GFX:-$HOME/Library/Application Support/Ternvale/host-gfx}"
src="${TERNVALE_VIRGL_SRC:-$prefix/src/virglrenderer-1.3.0}"
ver="1.3.0"
url="https://gitlab.freedesktop.org/virgl/virglrenderer/-/archive/${ver}/virglrenderer-${ver}.tar.bz2"

if ! command -v brew >/dev/null 2>&1; then
    log "Homebrew is required for MoltenVK / meson"
    exit 1
fi

log "brew install molten-vk vulkan-headers vulkan-loader meson ninja pkgconf libepoxy epoll-shim"
brew install molten-vk vulkan-headers vulkan-loader meson ninja pkgconf libepoxy epoll-shim >/dev/null
venv="$prefix/venv"
if [[ ! -x "$venv/bin/python3" ]]; then
    python3 -m venv "$venv"
fi
"$venv/bin/pip" install --quiet pyyaml
export PATH="$venv/bin:$PATH"
export PYTHONPATH="$venv/lib/$("$venv/bin/python3" -c 'import sys; print(f"python{sys.version_info.major}.{sys.version_info.minor}")')/site-packages${PYTHONPATH:+:$PYTHONPATH}"

icd="$(brew --prefix molten-vk)/share/vulkan/icd.d/MoltenVK_icd.json"
if [[ ! -f "$icd" ]]; then
    icd="$(brew --prefix molten-vk)/etc/vulkan/icd.d/MoltenVK_icd.json"
fi
[[ -f "$icd" ]] || { log "MoltenVK ICD JSON missing"; exit 1; }

mkdir -p "$prefix/src" "$prefix/lib"
archive="$prefix/src/virglrenderer-${ver}.tar.bz2"
if [[ ! -d "$src" ]]; then
    if [[ ! -f "$archive" ]]; then
        log "download $url"
        curl -fsSL "$url" -o "$archive"
    fi
    tar -xjf "$archive" -C "$prefix/src"
fi
macos_nanosleep="$script_dir/patches/virglrenderer-1.3.0-macos-clock_nanosleep.patch"
if [[ -f "$macos_nanosleep" ]]; then
    patch -p1 -N -d "$src" <"$macos_nanosleep" >/dev/null || true
fi
python3 - "$src/meson.build" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
t = p.read_text()
t = t.replace(
    "   if host_machine.system() in ['freebsd', 'openbsd']",
    "   if host_machine.system() in ['freebsd', 'openbsd', 'darwin']",
    1,
)
p.write_text(t)
print("meson.build darwin epoll-shim applied", file=sys.stderr)
PY

export PKG_CONFIG_PATH="$(brew --prefix)/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
build="$src/build-venus"
if [[ ! -f "$prefix/lib/libvirglrenderer.1.dylib" ]]; then
    log "meson -Dvenus=true prefix=$prefix"
    compat="$script_dir/patches/virgl-macos-socket-compat.h"
    incdir="/tmp/ternvale-virgl-inc-$$"
    mkdir -p "$incdir"
    cp -R "$src/src/mesa/compat/c11/." "$incdir/"
    export CFLAGS="-include ${compat} -I${incdir}"
    meson setup "$build" "$src" \
        --prefix="$prefix" \
        -Dvenus=true \
        -Drender-server-worker=thread \
        -Dtests=false
    meson compile -C "$build"
    meson install -C "$build"
fi

dylib="$prefix/lib/libvirglrenderer.1.dylib"
[[ -f "$dylib" ]] || dylib="$(brew --prefix virglrenderer)/lib/libvirglrenderer.1.dylib"

cat >"$prefix/env.sh" <<EOF
export TERNVALE_VIRGL_RENDERER="$dylib"
export VK_DRIVER_FILES="$icd"
export VK_ICD_FILENAMES="$icd"
EOF
log "wrote $prefix/env.sh renderer=$dylib icd=$icd"
# shellcheck source=lib-codesign.sh
source "$script_dir/lib-codesign.sh"
identity=$(ternvale_pick_codesign_identity)
if [[ -f "$dylib" ]]; then
    log "codesign $dylib identity=$identity"
    if [[ "$identity" != "-" ]]; then
        codesign --sign "$identity" --force --timestamp --options runtime "$dylib" || true
    else
        codesign --sign - --force "$dylib" || true
    fi
fi
# shellcheck disable=SC1091
source "$prefix/env.sh"
printf '%s\n' "$dylib"
