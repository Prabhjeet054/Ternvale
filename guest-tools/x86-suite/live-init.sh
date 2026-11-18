#!/bin/sh
# Native-guest runner for Step 99: FEX then qemu-user-static on the same
# x86_64 musl binaries. QEMU is a separate aarch64 program (GPL, not in git).
set -eu
export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
export HOME=/root
log() { printf '%s\n' "$*" >/dev/console 2>/dev/null || printf '%s\n' "$*"; }

mkdir -p /proc /sys /dev /tmp
mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sys /sys 2>/dev/null || true
mount -t devtmpfs dev /dev 2>/dev/null || true
ITERS="${LITMUS_ITERS:-2000000}"
if [ -r /proc/cmdline ]; then
  for _tok in $(cat /proc/cmdline); do
    case "$_tok" in
    LITMUS_ITERS=*) ITERS=${_tok#LITMUS_ITERS=} ;;
    esac
  done
fi
log "SUITE_START iters=$ITERS"

LOADER=/lib/ld-linux-aarch64.so.1
if [ ! -x "$LOADER" ]; then
  LOADER=/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1
fi

fex_run() {
  if [ -x "$LOADER" ] && [ -x /usr/bin/FEX ]; then
    "$LOADER" /usr/bin/FEX "$@"
  elif [ -x /usr/bin/FEX ]; then
    /usr/bin/FEX "$@"
  else
    return 127
  fi
}

if [ -x "$LOADER" ] && [ -x /usr/bin/FEXServer ]; then
  "$LOADER" /usr/bin/FEXServer >/dev/console 2>&1 &
  sleep 1
  log "FEXSERVER_STARTED"
fi

run_probes() {
  eng=$1
  log "ENGINE=$eng"
  set +e
  case "$eng" in
  fex)
    fex_run /x86/hello
    log "HELLO_RC=$?"
    fex_run /x86/det
    log "DET_RC=$?"
    fex_run /x86/litmus mp "$ITERS"
    log "LITMUS_MP_RC=$?"
    fex_run /x86/litmus sb "$ITERS"
    log "LITMUS_SB_RC=$?"
    ;;
  qemu-user)
    "$QEMU" /x86/hello
    log "HELLO_RC=$?"
    "$QEMU" /x86/det
    log "DET_RC=$?"
    "$QEMU" /x86/litmus mp "$ITERS"
    log "LITMUS_MP_RC=$?"
    "$QEMU" /x86/litmus sb "$ITERS"
    log "LITMUS_SB_RC=$?"
    ;;
  esac
  set -e
  log "ENGINE_DONE=$eng"
}

if [ -x /usr/bin/FEX ]; then
  run_probes fex
else
  log "ENGINE=fex"
  log "ENGINE_SKIP reason=no-fex"
fi

QEMU=/usr/bin/qemu-x86_64-static
if [ ! -x "$QEMU" ]; then
  QEMU=/usr/bin/qemu-x86_64
fi
if [ -x "$QEMU" ]; then
  run_probes qemu-user
else
  log "ENGINE=qemu-user"
  log "ENGINE_SKIP reason=no-qemu-user-static"
fi

log "SUITE_DONE"
sleep 4
poweroff -f 2>/dev/null || halt -f 2>/dev/null || exit 0
