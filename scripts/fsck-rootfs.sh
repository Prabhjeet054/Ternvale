#!/bin/bash
# Read-only ext4 check of a Ternvale rootfs image, plus an optional file readback.
#
# macOS has no e2fsprogs, so this runs `fsck.ext4 -fn` (forced, no changes) and
# `debugfs` inside a linux/arm64 Alpine container. The image is mounted read-only.
#
# Usage:
#   ./scripts/fsck-rootfs.sh path/to/rootfs.ext4 [guest-path [expected-content]]
#
# Exit 0 only when fsck reports no errors, the journal needs no recovery, and
# (when given) the file at guest-path holds expected-content.
set -euo pipefail

if [[ $# -lt 1 ]]; then
    printf 'usage: %s IMAGE [GUEST_PATH [EXPECTED]]\n' "$0" >&2
    exit 2
fi

image=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
guest_path="${2:-}"
expected="${3:-}"
alpine_ver="${ALPINE_VER:-3.20}"

log() {
    printf 'fsck-rootfs: %s\n' "$*" >&2
}

if [[ ! -f "$image" ]]; then
    log "missing image $image"
    exit 2
fi
if ! docker info >/dev/null 2>&1; then
    log "docker is not reachable; start Docker Desktop and retry"
    exit 2
fi

log "checking $image"
start=$(python3 -c 'import time; print(int(time.time() * 1000))')
set +e
docker run --rm --platform linux/arm64 \
    -v "${image}:/img/rootfs.ext4:ro" \
    -e "GUEST_PATH=${guest_path}" \
    -e "EXPECTED=${expected}" \
    "alpine:${alpine_ver}" \
    sh -c '
apk add --no-cache e2fsprogs e2fsprogs-extra >/dev/null || exit 3
if dumpe2fs -h /img/rootfs.ext4 2>/dev/null | grep "^Filesystem features:" | grep -q needs_recovery; then
  echo "journal needs recovery"
  exit 4
fi
fsck.ext4 -fn /img/rootfs.ext4
rc=$?
echo "fsck_exit=$rc"
[ "$rc" -eq 0 ] || exit 5
if [ -n "$GUEST_PATH" ]; then
  got=$(debugfs -R "cat $GUEST_PATH" /img/rootfs.ext4 2>/dev/null)
  echo "readback $GUEST_PATH=$got"
  [ "$got" = "$EXPECTED" ] || exit 6
fi
'
status=$?
set -e
end=$(python3 -c 'import time; print(int(time.time() * 1000))')
log "status=${status} fsck_ms=$((end - start))"
exit "$status"
