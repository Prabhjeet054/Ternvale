#!/bin/sh
# Guest-side mount + binfmt for Ternvale's VZ Linux Rosetta share.
# Magic/mask/flags: Apple "Running Intel Binaries in Linux VMs".
# Host tag is `rosetta`. Do not run this on the Mac host.
set -eu
MOUNT="${ROSETTA_MOUNT:-/mnt/rosetta}"
TAG="${ROSETTA_TAG:-rosetta}"
mkdir -p "$MOUNT"
mount -t virtiofs "$TAG" "$MOUNT"
test -x "$MOUNT/rosetta"
# Apple: on macOS 13 use --preserve no
PRESERVE="${ROSETTA_PRESERVE:-yes}"
/usr/sbin/update-binfmts --install rosetta "$MOUNT/rosetta" \
    --magic "\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x3e\x00" \
    --mask "\xff\xff\xff\xff\xff\xfe\xfe\x00\xff\xff\xff\xff\xff\xff\xff\xff\xfe\xff\xff\xff" \
    --credentials yes --preserve "$PRESERVE" --fix-binary yes
