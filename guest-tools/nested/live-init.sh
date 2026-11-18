#!/bin/busybox sh
# L1 probe for scripts/nested-virt-live-check.sh. Always prints NESTED_PROBE_DONE.
set -eu
bb=/bin/busybox
echo "NESTED_PROBE_START"
echo "--- cmdline ---"
$bb cat /proc/cmdline
echo "--- cpu started ---"
$bb dmesg 2>/dev/null | $bb grep -E "CPU: All CPU" || echo "NO_CPU_STARTED_LINE"
echo "--- dmesg kvm|hyp ---"
if $bb dmesg 2>/dev/null | $bb grep -i -E "kvm|hyp"; then
  :
else
  echo "NO_KVM_HYP_DMESG"
fi
if [ -f /modules/order ]; then
  echo "--- insmod kvm deps ---"
  for m in $($bb cat /modules/order); do
    $bb insmod /modules/${m}.ko 2>&1 || echo "INSMOD_${m}_FAIL"
  done
  echo "--- dmesg kvm after insmod ---"
  $bb dmesg 2>/dev/null | $bb grep -i -E "kvm|hyp" || echo "NO_KVM_HYP_AFTER"
fi
echo "--- /dev/kvm ---"
if [ -e /dev/kvm ]; then
  $bb ls -l /dev/kvm
  echo "DEV_KVM=yes"
else
  echo "NO_DEV_KVM"
  echo "DEV_KVM=no"
fi
if [ -e /dev/kvm ] && [ -x /bin/lkvm ] && [ -f /l2/Image ] && [ -f /l2/initrd ]; then
  echo "L2_START"
  t0=$($bb date +%s)
  set +e
  /bin/lkvm run --name l2 --cpus 1 --mem 64 --kernel /l2/Image --initrd /l2/initrd \
    --console serial --irqchip gicv3 --transport mmio --params "console=ttyAMA0 rdinit=/init" \
    >/tmp/l2.out 2>/tmp/l2.err
  rc=$?
  set -e
  t1=$($bb date +%s)
  echo "L2_RC=${rc} L2_WALL_S=$((t1 - t0))"
  $bb cat /tmp/l2.out 2>/dev/null || true
  $bb cat /tmp/l2.err 2>/dev/null || true
  if $bb grep -q L2_BOOTED /tmp/l2.out /tmp/l2.err 2>/dev/null; then
    echo "L2_BOOTED_OK"
  else
    echo "L2_NO_BANNER"
  fi
  echo "L2_DONE"
else
  echo "L2_SKIPPED"
fi
echo "NESTED_PROBE_DONE"
$bb poweroff -f
