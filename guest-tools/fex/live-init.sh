#!/bin/sh
# PID 1 for Step 98 native FEX live disk. Prints markers for the host harness.
set -eu
export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
export HOME=/root

log() { printf '%s\n' "$*" >/dev/console 2>/dev/null || printf '%s\n' "$*"; }

mkdir -p /proc /sys /dev /tmp /proc/sys/fs/binfmt_misc
mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sys /sys 2>/dev/null || true
mount -t devtmpfs dev /dev 2>/dev/null || true
mkdir -p /tmp /root
export HOME=/root
export TMPDIR=/tmp
log "FEX_LIVE_START"
for f in /lib/modules/tv/binfmt_misc.ko /modules/binfmt_misc.ko; do
  if [ -f "$f" ]; then
    /bin/busybox insmod "$f" 2>/dev/null || insmod "$f" 2>/dev/null || true
  fi
done
mount -t binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc 2>/dev/null || true

FEX_BIN=""
for c in /usr/bin/FEXInterpreter /usr/bin/FEX /usr/local/bin/FEXInterpreter; do
  if [ -x "$c" ]; then
    FEX_BIN=$c
    break
  fi
done
if [ -z "$FEX_BIN" ]; then
  log "FEX_INTERPRETER_MISSING"
else
  log "FEX_INTERPRETER_OK path=$FEX_BIN"
fi
if [ -n "${FEX_ROOTFS:-}" ] && [ -d "$FEX_ROOTFS" ]; then
  log "FEX_ROOTFS_OK path=$FEX_ROOTFS"
  export FEX_ROOTFS
else
  if [ -d /opt/fex/RootFS ]; then
    export FEX_ROOTFS=/opt/fex/RootFS
    log "FEX_ROOTFS_OK path=$FEX_ROOTFS"
  else
    log "FEX_ROOTFS_ABSENT"
  fi
fi

if [ -n "$FEX_BIN" ] && [ -w /proc/sys/fs/binfmt_misc/register ]; then
  FEX_INTERPRETER="$FEX_BIN" /usr/local/lib/ternvale/fex/register-binfmt.sh || true
  printf '%s' ":FEX-x86_64:M::\\x7fELF\\x02\\x01\\x01\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x02\\x00\\x3e\\x00:\\xff\\xff\\xff\\xff\\xff\\xfe\\xfe\\x00\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xff\\xfe\\xff\\xff\\xff:${FEX_BIN}:PCF" \
    >/proc/sys/fs/binfmt_misc/register && log "BINFMT_REG_OK" || log "BINFMT_REG_FAIL"
fi
if [ -e /proc/sys/fs/binfmt_misc/FEX-x86_64 ] || [ -e /proc/sys/fs/binfmt_misc/FEXInterpreter ]; then
  log "BINFMT_ENTRY_OK"
else
  log "BINFMT_ENTRY_MISSING"
fi

run() {
  name=$1
  shift
  set +e
  "$@"
  rc=$?
  set -e
  log "${name}_RC=$rc"
}

LOADER=/lib/ld-linux-aarch64.so.1
if [ ! -x "$LOADER" ]; then
  LOADER=/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1
fi
fex_run() {
  if [ -x "$LOADER" ] && [ -x /usr/bin/FEX ]; then
    "$LOADER" /usr/bin/FEX "$@"
  else
    "$FEX_BIN" "$@"
  fi
}
if [ -x "$LOADER" ] && [ -x /usr/bin/FEXServer ]; then
  "$LOADER" /usr/bin/FEXServer >/dev/console 2>&1 &
  sleep 1
  log "FEXSERVER_STARTED"
fi
if [ -n "$FEX_BIN" ] && [ -x /x86/hello ]; then
  log "FEX_HELP"
  fex_run --help >/dev/null 2>&1 && log "FEX_HELP_OK" || log "FEX_HELP_FAIL"
  run DIRECT_HELLO fex_run /x86/hello
fi
if [ -x /x86/hello ]; then
  run HELLO /x86/hello
fi
if [ -x /x86/mt ]; then
  run DIRECT_MT fex_run /x86/mt
  run MT /x86/mt
fi
if [ -x /x86/docker-hello ]; then
  run DOCKER_IMAGE fex_run /x86/docker-hello
else
  log "DOCKER_IMAGE_SKIP"
fi
if command -v docker >/dev/null 2>&1; then
  run DOCKER docker run --rm --platform linux/amd64 hello-world
else
  log "DOCKER_SKIP reason=no-dockerd"
fi
if [ -x /x86/bench-arm ]; then
  run ARM_BENCH /x86/bench-arm
fi
if [ -x /x86/bench-x86 ]; then
  run FEX_BENCH fex_run /x86/bench-x86
fi

log "FEX_LIVE_DONE"
sleep 8
poweroff -f 2>/dev/null || halt -f 2>/dev/null || exit 0
