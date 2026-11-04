#!/bin/bash
# Build test-assets/virtio-vmnet/{Image,initramfs.cpio} for the Step 66 live check.
#
# Same virtio_net modules as make-net-initramfs.sh, plus udhcpc, curl, CA certs,
# and iperf3 so the guest can DHCP, resolve DNS, hit https://example.com, and
# run iperf3 to the host.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/virtio-vmnet}"
alpine_ver="${ALPINE_VER:-3.20}"
alpine="https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}"

log() {
    printf 'make-vmnet-initramfs: %s\n' "$*" >&2
}

for tool in curl python3 gzip cpio tar; do
    command -v "${tool}" >/dev/null 2>&1 || {
        log "missing required tool: ${tool}"
        exit 1
    }
done

mkdir -p "${out}"
tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

fetch_index() {
    local repo=$1 dest=$2
    curl -fsSL --retry 3 -A Ternvale -o "${tmp}/idx.tgz" "${alpine}/${repo}/aarch64/APKINDEX.tar.gz"
    tar -xOf "${tmp}/idx.tgz" APKINDEX >"${dest}"
}

fetch_index main "${tmp}/APKINDEX.main"
fetch_index community "${tmp}/APKINDEX.community"
cat "${tmp}/APKINDEX.main" "${tmp}/APKINDEX.community" >"${tmp}/APKINDEX"

python3 - "${tmp}/APKINDEX" "${tmp}/apks" curl ca-certificates iperf3 libcurl openssl \
    nghttp2-libs brotli-libs zstd-libs libidn2 libpsl musl busybox c-ares zlib \
    libunistring <<'PY'
import pathlib, sys
index = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace").split("\n\n")
pkgs = {}
for block in index:
    fields = dict(line.split(":", 1) for line in block.splitlines() if ":" in line)
    name = fields.get("P")
    if not name:
        continue
    pkgs[name] = (fields.get("V", ""), [d.split(">")[0].split("=")[0].split("<")[0] for d in fields.get("D", "").split() if d], fields.get("C", "main"))
want = list(sys.argv[3:])
seen = set()
order = []
def visit(name):
    name = name.split(":")[-1]
    if name in seen or name not in pkgs:
        return
    seen.add(name)
    _, deps, _ = pkgs[name]
    for dep in deps:
        if dep.startswith("so:"):
            continue
        visit(dep)
    order.append(name)
for name in want:
    visit(name)
pathlib.Path(sys.argv[2]).write_text("\n".join(order) + "\n")
print("\n".join(order))
PY

apk_file() {
    python3 - "${tmp}/APKINDEX" "$1" <<'PY'
import sys
text = open(sys.argv[1], encoding="utf-8", errors="replace").read().split("\n\n")
for block in text:
    fields = dict(line.split(":", 1) for line in block.splitlines() if ":" in line)
    if fields.get("P") == sys.argv[2]:
        print(fields["P"] + "-" + fields["V"] + ".apk")
        print("community" if "iperf3" == sys.argv[2] or fields.get("P") == "iperf3" else "main")
        break
else:
    sys.exit(f"{sys.argv[2]} package not found")
PY
}

mkdir -p "${tmp}/root"
while read -r pkg; do
    [ -z "${pkg}" ] && continue
    info="$(apk_file "${pkg}")"
    name=$(printf '%s\n' "${info}" | sed -n '1p')
    repo=main
    log "downloading ${repo}/${name}"
    curl -fL --retry 3 -A Ternvale -o "${tmp}/${pkg}.apk" "${alpine}/${repo}/aarch64/${name}"
    tar -xOf "${tmp}/${pkg}.apk" 2>/dev/null | tar -xC "${tmp}/root" 2>/dev/null || \
        tar -xC "${tmp}/root" -f "${tmp}/${pkg}.apk" 2>/dev/null || true
done <"${tmp}/apks"

# linux-virt for kernel + modules (same as make-net-initramfs.sh)
name=$(python3 - "${tmp}/APKINDEX.main" linux-virt <<'PY'
import sys
text = open(sys.argv[1], encoding="utf-8", errors="replace").read().split("\n\n")
for block in text:
    fields = dict(line.split(":", 1) for line in block.splitlines() if ":" in line)
    if fields.get("P") == sys.argv[2]:
        print(fields["P"] + "-" + fields["V"] + ".apk")
        break
PY
)
log "downloading ${name}"
curl -fL --retry 3 -A Ternvale -o "${tmp}/linux-virt.apk" "${alpine}/main/aarch64/${name}"
tar -xOf "${tmp}/linux-virt.apk" boot/vmlinuz-virt >"${tmp}/vmlinuz"
python3 - "${tmp}/vmlinuz" "${out}/Image" <<'PY'
import gzip, pathlib, struct, sys
blob = pathlib.Path(sys.argv[1]).read_bytes()
if blob[56:60] == b"ARM\x64":
    image = blob
elif blob[4:8] == b"zimg":
    off, size = struct.unpack_from("<II", blob, 8)
    comp = blob[24:32].split(b"\x00", 1)[0]
    if comp != b"gzip":
        sys.exit(f"unsupported zboot compression {comp!r}")
    image = gzip.decompress(blob[off:off + size])
else:
    sys.exit("vmlinuz-virt is neither a raw arm64 Image nor an EFI zboot image")
if image[56:60] != b"ARM\x64":
    sys.exit("unpacked file is not an arm64 Image")
pathlib.Path(sys.argv[2]).write_bytes(image)
PY

tar -tf "${tmp}/linux-virt.apk" >"${tmp}/files"
mod_dir=$(python3 - "${tmp}/files" <<'PY'
import sys
for line in open(sys.argv[1]):
    line = line.strip()
    if line.endswith("/modules.dep"):
        print(line.rsplit("/", 1)[0])
        break
else:
    sys.exit("modules.dep not found")
PY
)
tar -xOf "${tmp}/linux-virt.apk" "${mod_dir}/modules.dep" >"${tmp}/modules.dep"
python3 - "${tmp}/modules.dep" virtio_mmio virtio_net af_packet >"${tmp}/order" <<'PY'
import sys
deps = {}
for line in open(sys.argv[1]):
    if ":" not in line:
        continue
    mod, rest = line.split(":", 1)
    deps[mod.strip()] = rest.split()
by_name = {p.rsplit("/", 1)[1].split(".ko")[0].replace("-", "_"): p for p in deps}
order = []
def visit(path):
    for dep in deps.get(path, []):
        visit(dep)
    if path not in order:
        order.append(path)
for want in sys.argv[2:]:
    if want not in by_name:
        print(f"skip {want} (built-in or absent)", file=sys.stderr)
        continue
    visit(by_name[want])
print("\n".join(order))
PY

mkdir -p "${tmp}/root/modules" "${tmp}/root/bin" "${tmp}/root/sbin"
: >"${tmp}/root/modules/order"
while read -r rel; do
    name=$(basename "${rel}")
    base="${name%%.ko*}"
    member="${mod_dir}/${rel}"
    case "${name}" in
        *.ko.gz) tar -xOf "${tmp}/linux-virt.apk" "${member}" | gzip -dc >"${tmp}/root/modules/${base}.ko" ;;
        *.ko) tar -xOf "${tmp}/linux-virt.apk" "${member}" >"${tmp}/root/modules/${base}.ko" ;;
        *) log "unsupported module: ${name}"; exit 1 ;;
    esac
    printf '%s\n' "${base}" >>"${tmp}/root/modules/order"
done <"${tmp}/order"

# Busybox may already be in /bin from the apk unpack.
if [[ ! -x "${tmp}/root/bin/busybox" ]]; then
    tar -xOf "${tmp}/busybox.apk" bin/busybox >"${tmp}/root/bin/busybox"
    chmod 755 "${tmp}/root/bin/busybox"
fi

cat >"${tmp}/root/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mkdir -p /proc /sys /dev /tmp /etc /usr/bin /usr/sbin /sbin
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sys /sys
/bin/busybox mount -t devtmpfs dev /dev
/bin/busybox --install -s
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
for m in $(cat /modules/order); do
  insmod /modules/${m}.ko || echo "insmod ${m} failed"
done
i=0
while [ ! -e /sys/class/net/eth0 ] && [ "$i" -lt 100 ]; do
  sleep 0.1
  i=$((i + 1))
done
if [ ! -e /sys/class/net/eth0 ]; then
  echo "ternvale vmnet FAIL eth0 missing"
  exec setsid sh -c 'exec sh -i' </dev/console >/dev/console 2>&1
fi
ip link set eth0 up
if [ ! -f /etc/ssl/certs/ca-certificates.crt ]; then
  mkdir -p /etc/ssl/certs
  cat /usr/share/ca-certificates/mozilla/*.crt > /etc/ssl/certs/ca-certificates.crt 2>/dev/null || true
fi
export SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
export CURL_CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt
c=0
while [ "$(cat /sys/class/net/eth0/carrier 2>/dev/null)" != 1 ] && [ "$c" -lt 50 ]; do
  sleep 0.2
  c=$((c + 1))
done
echo "ternvale vmnet link $(cat /sys/class/net/eth0/operstate) carrier=$(cat /sys/class/net/eth0/carrier 2>/dev/null || echo x) mac=$(cat /sys/class/net/eth0/address)"
ipcfg=$(sed -n 's/.*ternvale.ip=\([^ ]*\).*/\1/p' /proc/cmdline)
peer=$(sed -n 's/.*ternvale.peer=\([^ ]*\).*/\1/p' /proc/cmdline)
if [ -n "$ipcfg" ]; then
  ip addr add "$ipcfg" dev eth0 || echo "ternvale vmnet FAIL static addr"
  ip route add 192.168.100.0/24 dev eth0 2>/dev/null || true
  echo "ternvale vmnet static addr=$ipcfg peer=$peer"
  if [ -n "$peer" ]; then
    p=0
    while [ "$p" -lt 90 ]; do
      if ping -c 1 -W 1 "$peer" >/tmp/ping.out 2>&1; then
        echo "ternvale vmnet ping ok"
        cat /tmp/ping.out
        break
      fi
      p=$((p + 1))
      sleep 0.2
    done
    if [ "$p" -ge 90 ]; then
      echo "ternvale vmnet FAIL ping"
      cat /tmp/ping.out 2>/dev/null || true
      ip neigh show
      ip addr show eth0
    fi
  fi
  if command -v curl >/dev/null 2>&1 && curl -fsS -o /tmp/curl.out --max-time 8 https://example.com; then
    echo "ternvale vmnet FAIL internet reachable"
  else
    echo "ternvale vmnet internet blocked"
  fi
  echo "ternvale vmnet ready"
  while true; do
    car=$(cat /sys/class/net/eth0/carrier 2>/dev/null || echo x)
    echo "ternvale vmnet carrier=${car} $(ip link show eth0 | tr '\n' ' ')"
    sleep 1
  done
fi
udhcpc -i eth0 -n -q -t 12 -T 2 || echo "ternvale vmnet FAIL dhcp"
ip addr show eth0
gw=$(ip route | awk '/default/{print $3; exit}')
echo "ternvale vmnet dhcp addr=$(ip -4 -o addr show eth0 | awk '{print $4}') gw=${gw}"
if nslookup example.com >/tmp/dns 2>&1 || busybox nslookup example.com >/tmp/dns 2>&1; then
  echo "ternvale vmnet dns ok"
  cat /tmp/dns
else
  echo "ternvale vmnet FAIL dns"
  cat /tmp/dns
fi
if command -v curl >/dev/null 2>&1; then
  if curl -fsS -o /tmp/curl.out -D /tmp/curl.hdr --max-time 20 https://example.com; then
    echo "ternvale vmnet curl ok"
    head -n 5 /tmp/curl.hdr
  else
    echo "ternvale vmnet FAIL curl"
  fi
else
  echo "ternvale vmnet FAIL curl missing"
fi
if command -v iperf3 >/dev/null 2>&1 && [ -n "${gw}" ]; then
  iperf3 -c "${gw}" -t 5 -p 5201 || echo "ternvale vmnet FAIL iperf3"
else
  echo "ternvale vmnet FAIL iperf3 missing gw=${gw}"
fi
echo "ternvale vmnet ready"
while true; do
  car=$(cat /sys/class/net/eth0/carrier 2>/dev/null || echo x)
  echo "ternvale vmnet carrier=${car} $(ip link show eth0 | tr '\n' ' ')"
  sleep 1
done
INIT
chmod 755 "${tmp}/root/init"

(
    cd "${tmp}/root"
    find . -mindepth 1 -print | sed 's|^\./||' | sort | cpio -o -H newc
) >"${out}/initramfs.cpio"

log "wrote ${out}/Image and ${out}/initramfs.cpio"
