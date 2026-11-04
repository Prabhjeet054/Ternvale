#!/bin/bash
# Build test-assets/virtio-user-nat/{Image,initramfs.cpio,id_ed25519} for Step 68 live.
# Reuses the vmnet initramfs (virtio_net, curl, iperf3, CA certs) and adds dropbear.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/virtio-user-nat}"
vmnet="${root}/test-assets/virtio-vmnet"
alpine_ver="${ALPINE_VER:-3.20}"
alpine="https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}"

log() { printf 'make-user-nat-initramfs: %s\n' "$*" >&2; }

if [[ ! -f "${vmnet}/Image" || ! -f "${vmnet}/initramfs.cpio" ]]; then
    log "building virtio-vmnet assets first"
    "${script_dir}/make-vmnet-initramfs.sh" "${vmnet}"
fi

mkdir -p "${out}"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

log "unpacking vmnet initramfs"
mkdir -p "${tmp}/root"
(cd "${tmp}/root" && cpio -id --quiet <"${vmnet}/initramfs.cpio")
cp -f "${vmnet}/Image" "${out}/Image"

log "fetching dropbear"
curl -fsSL --retry 3 -A Ternvale -o "${tmp}/idx.tgz" "${alpine}/main/aarch64/APKINDEX.tar.gz"
tar -xOf "${tmp}/idx.tgz" APKINDEX >"${tmp}/APKINDEX"
name=$(python3 - "${tmp}/APKINDEX" dropbear <<'PY'
import sys
text = open(sys.argv[1], encoding="utf-8", errors="replace").read().split("\n\n")
for block in text:
    fields = dict(line.split(":", 1) for line in block.splitlines() if ":" in line)
    if fields.get("P") == sys.argv[2]:
        print(fields["P"] + "-" + fields["V"] + ".apk")
        break
else:
    sys.exit("dropbear not found")
PY
)
curl -fL --retry 3 -A Ternvale -o "${tmp}/dropbear.apk" "${alpine}/main/aarch64/${name}"
tar -xOf "${tmp}/dropbear.apk" 2>/dev/null | tar -xC "${tmp}/root" 2>/dev/null \
    || tar -xC "${tmp}/root" -f "${tmp}/dropbear.apk" 2>/dev/null || true

mkdir -p "${tmp}/root/root/.ssh" "${tmp}/root/etc/dropbear" "${tmp}/root/etc"
printf 'root:x:0:0:root:/root:/bin/sh\n' >"${tmp}/root/etc/passwd"
printf 'root:x:0:\n' >"${tmp}/root/etc/group"
if [[ ! -f "${out}/id_ed25519" ]]; then
    ssh-keygen -t ed25519 -f "${out}/id_ed25519" -N "" -C "ternvale-user-nat" >/dev/null
fi
cp -f "${out}/id_ed25519.pub" "${tmp}/root/root/.ssh/authorized_keys"
cp -f "${out}/id_ed25519.pub" "${tmp}/root/id.pub"
chmod 700 "${tmp}/root/root" "${tmp}/root/root/.ssh"
chmod 600 "${tmp}/root/root/.ssh/authorized_keys"

cat >"${tmp}/root/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mkdir -p /proc /sys /dev /tmp /etc /usr/bin /usr/sbin /sbin /root/.ssh
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev
/bin/busybox mkdir -p /dev/pts
/bin/busybox mount -t devpts devpts /dev/pts 2>/dev/null || true
/bin/busybox --install -s
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
ulimit -n 1024 2>/dev/null || true
for m in $(cat /modules/order); do
  insmod /modules/${m}.ko || echo "insmod ${m} failed"
done
i=0
while [ ! -e /sys/class/net/eth0 ] && [ "$i" -lt 100 ]; do
  sleep 0.1
  i=$((i + 1))
done
if [ ! -e /sys/class/net/eth0 ]; then
  echo "ternvale user-nat FAIL eth0 missing"
  exec setsid sh -c 'exec sh -i' </dev/console >/dev/console 2>&1
fi
ip link set eth0 up
if [ ! -f /etc/ssl/certs/ca-certificates.crt ]; then
  mkdir -p /etc/ssl/certs
  cat /usr/share/ca-certificates/mozilla/*.crt > /etc/ssl/certs/ca-certificates.crt 2>/dev/null || true
fi
export SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
export CURL_CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt
udhcpc -i eth0 -n -q -t 12 -T 2 || echo "ternvale user-nat FAIL dhcp"
ip addr show eth0
gw=$(ip route | awk '/default/{print $3; exit}')
echo "ternvale user-nat dhcp addr=$(ip -4 -o addr show eth0 | awk '{print $4}') gw=${gw}"
if ping -c 1 -W 3 10.0.2.2 >/tmp/ping-gw.out 2>&1; then
  echo "ternvale user-nat ping gw ok"
else
  echo "ternvale user-nat FAIL ping gw"
  cat /tmp/ping-gw.out
fi
if ping -c 3 -W 3 1.1.1.1 >/tmp/ping.out 2>&1; then
  echo "ternvale user-nat ping ok"
  cat /tmp/ping.out
else
  echo "ternvale user-nat FAIL ping 1.1.1.1"
  cat /tmp/ping.out
fi
if nslookup example.com >/tmp/dns 2>&1 || busybox nslookup example.com >/tmp/dns 2>&1; then
  echo "ternvale user-nat dns ok"
  cat /tmp/dns
else
  echo "ternvale user-nat FAIL dns"
  cat /tmp/dns
fi
if command -v curl >/dev/null 2>&1; then
  if curl -fsS -o /tmp/curl.out -D /tmp/curl.hdr --max-time 20 https://example.com; then
    echo "ternvale user-nat curl ok"
    head -n 5 /tmp/curl.hdr
  else
    echo "ternvale user-nat FAIL curl"
  fi
else
  echo "ternvale user-nat FAIL curl missing"
fi
export HOME=/root
mkdir -p /etc/dropbear /root/.ssh /.ssh
printf 'root:*:0:0:99999:7:::\n' > /etc/shadow
if [ -f /id.pub ]; then
  cat /id.pub > /root/.ssh/authorized_keys
  cat /id.pub > /.ssh/authorized_keys
fi
chown -R 0:0 /root /.ssh /etc/passwd /etc/group /etc/shadow 2>/dev/null || true
chmod 700 /root /root/.ssh /.ssh
chmod 600 /root/.ssh/authorized_keys /.ssh/authorized_keys /etc/shadow 2>/dev/null || true
if command -v dropbearkey >/dev/null 2>&1; then
  dropbearkey -t ed25519 -f /etc/dropbear/dropbear_ed25519_host_key >/dev/null 2>&1 || true
fi
if command -v dropbear >/dev/null 2>&1; then
  dropbear -R -s -p 22 -E || echo "ternvale user-nat FAIL dropbear"
  echo "ternvale user-nat sshd keys=$(wc -c < /root/.ssh/authorized_keys)"
  echo "ternvale user-nat sshd ready"
else
  echo "ternvale user-nat FAIL dropbear missing"
fi
sleep 12
echo "ternvale user-nat burst start"
n=0
while [ "$n" -lt 500 ]; do
  nc -w 5 10.0.2.2 19999 </dev/null >/dev/null 2>&1 &
  n=$((n + 1))
done
wait
echo "ternvale user-nat burst done"
if command -v iperf3 >/dev/null 2>&1; then
  iperf3 -c 10.0.2.2 -t 5 -p 5201 || echo "ternvale user-nat FAIL iperf3"
else
  echo "ternvale user-nat FAIL iperf3 missing"
fi
echo "ternvale user-nat ready"
while true; do
  sleep 5
done
INIT
chmod 755 "${tmp}/root/init"

(
    cd "${tmp}/root"
    find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc
) >"${out}/initramfs.cpio"

log "wrote ${out}/Image, ${out}/initramfs.cpio, ${out}/id_ed25519"
