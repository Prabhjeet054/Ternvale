#!/bin/busybox sh
# VZ Linux Rosetta runner for the Step 99 x86 suite.
echo SUITE_START
ITERS="${LITMUS_ITERS:-2000000}"
if [ -r /proc/cmdline ]; then
  for _tok in $(cat /proc/cmdline); do
    case "$_tok" in
    LITMUS_ITERS=*) ITERS=${_tok#LITMUS_ITERS=} ;;
    esac
  done
fi
echo "SUITE_ITERS=$ITERS"
echo ENGINE=rosetta
/bin/busybox mkdir -p /mnt/rosetta /proc/sys/fs/binfmt_misc
/bin/busybox mount -t virtiofs rosetta /mnt/rosetta || echo ROSETTA_MOUNT_FAIL
RUNTIME=/mnt/rosetta/rosetta
if [ -d "$RUNTIME" ] && [ -e "$RUNTIME/rosetta" ]; then
  RUNTIME="$RUNTIME/rosetta"
fi
/bin/busybox insmod /modules/binfmt_misc.ko 2>/dev/null || true
/bin/busybox mount -t binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc 2>/dev/null || true
if [ ! -f "$RUNTIME" ]; then
  echo ENGINE_SKIP reason=no-rosetta-runtime
  echo SUITE_DONE
  exit 0
fi
set +e
"$RUNTIME" /x86/hello
echo HELLO_RC=$?
"$RUNTIME" /x86/det
echo DET_RC=$?
"$RUNTIME" /x86/litmus mp "$ITERS"
echo LITMUS_MP_RC=$?
"$RUNTIME" /x86/litmus sb "$ITERS"
echo LITMUS_SB_RC=$?
set -e
echo ENGINE_DONE=rosetta
echo SUITE_DONE
