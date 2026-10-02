#!/bin/bash
# Install the guest agent into the rootfs from ./scripts/make-rootfs.sh:
#   /usr/bin/ternvale-agent            static aarch64 musl build (build-guest-agent.sh)
#   /usr/sbin/ternvale-agent-start     loads vsock, starts the agent in the background
#   /lib/modules/ternvale-vsock/       vsock modules matching the rootfs kernel Image
#
# The rootfs /sbin/init (written by make-rootfs.sh) runs ternvale-agent-start
# when the kernel cmdline has ternvale.agent=<1|trace|debug|info|warn>; the
# value is also the agent's log level. Run ternvale-agent-start by hand in the
# guest to restart a killed agent.
#
# The vsock modules come from Alpine's current linux-virt package, so its
# version must match the kernel in OUT_DIR/Image; the script refuses otherwise
# (rerun make-rootfs.sh to refresh both).
#
# Needs Docker (linux/arm64, privileged for the loop mount). Safe to rerun:
# every installed file is overwritten.
#
# Usage:
#   ./scripts/install-guest-agent.sh [test-assets/virtio-root]
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${script_dir}/.." && pwd)
out="${1:-${root}/test-assets/virtio-root}"
out=$(cd "${out}" && pwd)
alpine_ver="${ALPINE_VER:-3.20}"
alpine="https://dl-cdn.alpinelinux.org/alpine/v${alpine_ver}"

log() {
    printf 'install-guest-agent: %s\n' "$*" >&2
}

for tool in curl python3 gzip tar docker; do
    command -v "${tool}" >/dev/null 2>&1 || { log "missing required tool: ${tool}"; exit 1; }
done
for file in rootfs.ext4 Image; do
    [[ -f "${out}/${file}" ]] || { log "missing ${out}/${file} (run ./scripts/make-rootfs.sh first)"; exit 1; }
done
if ! docker info >/dev/null 2>&1; then
    log "docker is not reachable; start Docker Desktop and retry"
    exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT
overlay="${tmp}/overlay"
mkdir -p "${overlay}/usr/bin" "${overlay}/usr/sbin" "${overlay}/lib/modules/ternvale-vsock"

kver=$(python3 - "${out}/Image" <<'PY'
import re, sys
m = re.search(rb"Linux version (\S+)", open(sys.argv[1], "rb").read())
print(m.group(1).decode() if m else "")
PY
)
[[ -n "${kver}" ]] || { log "no 'Linux version' string in ${out}/Image"; exit 1; }
log "rootfs kernel ${kver}"

"${script_dir}/build-guest-agent.sh" "${overlay}/usr/bin/ternvale-agent"

curl -fsSL --retry 3 -A Ternvale -o "${tmp}/apkindex.tar.gz" \
    "${alpine}/main/aarch64/APKINDEX.tar.gz"
tar -xOf "${tmp}/apkindex.tar.gz" APKINDEX >"${tmp}/APKINDEX"
virt_apk=$(python3 - "${tmp}/APKINDEX" <<'PY'
import sys
for block in open(sys.argv[1], encoding="utf-8", errors="replace").read().split("\n\n"):
    fields = dict(line.split(":", 1) for line in block.splitlines() if ":" in line)
    if fields.get("P") == "linux-virt":
        print("linux-virt-" + fields["V"] + ".apk")
        break
else:
    sys.exit("linux-virt package not found")
PY
)
log "downloading ${virt_apk}"
curl -fsSL --retry 3 -A Ternvale -o "${tmp}/linux-virt.apk" "${alpine}/main/aarch64/${virt_apk}"

mod_dir=$(tar -tf "${tmp}/linux-virt.apk" | grep -m1 '/modules.dep$' | sed 's|/modules.dep$||')
[[ -n "${mod_dir}" ]] || { log "modules.dep not found in ${virt_apk}"; exit 1; }
if [[ "$(basename "${mod_dir}")" != "${kver}" ]]; then
    log "${virt_apk} has modules for $(basename "${mod_dir}") but the Image is ${kver}; rerun ./scripts/make-rootfs.sh"
    exit 1
fi
tar -xOf "${tmp}/linux-virt.apk" "${mod_dir}/modules.dep" >"${tmp}/modules.dep"
python3 - "${tmp}/modules.dep" vmw_vsock_virtio_transport >"${tmp}/order" <<'PY'
import sys
deps = {}
for line in open(sys.argv[1]):
    if ":" in line:
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
        sys.exit(f"{want} is not a module in this kernel (built in?)")
    visit(by_name[want])
print("\n".join(order))
PY

mods="${overlay}/lib/modules/ternvale-vsock"
: >"${mods}/order"
while read -r rel; do
    name=$(basename "${rel}")
    base="${name%%.ko*}"
    case "${name}" in
        *.ko.gz) tar -xOf "${tmp}/linux-virt.apk" "${mod_dir}/${rel}" | gzip -dc >"${mods}/${base}.ko" ;;
        *.ko) tar -xOf "${tmp}/linux-virt.apk" "${mod_dir}/${rel}" >"${mods}/${base}.ko" ;;
        *) log "unsupported module compression: ${name}"; exit 1 ;;
    esac
    printf '%s\n' "${base}" >>"${mods}/order"
    log "module ${base}"
done <"${tmp}/order"

cat >"${overlay}/usr/sbin/ternvale-agent-start" <<'START'
#!/bin/sh
# Start ternvale-agent in the background with its log on the console. Loads
# the vsock modules first when /dev/vsock is missing. Log level comes from
# ternvale.agent=<trace|debug|info|warn> on the kernel cmdline (else info).
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
mods=/lib/modules/ternvale-vsock
if [ ! -e /dev/vsock ]; then
  for m in $(cat "${mods}/order"); do
    [ -d "/sys/module/${m}" ] || insmod "${mods}/${m}.ko" || echo "insmod ${m} failed"
  done
  i=0
  while [ ! -e /dev/vsock ] && [ "$i" -lt 50 ]; do
    sleep 0.1
    i=$((i + 1))
  done
fi
[ -e /dev/vsock ] || echo "ternvale agent: /dev/vsock is missing"
level=$(sed -n 's/.*ternvale\.agent=\([a-z0-9]*\).*/\1/p' /proc/cmdline)
case "${level}" in trace|debug|info|warn) ;; *) level=info ;; esac
TERNVALE_AGENT_LOG=${level} setsid /usr/bin/ternvale-agent </dev/null >/dev/console 2>&1 &
echo "ternvale agent started pid=$! log=${level}"
START
chmod 755 "${overlay}/usr/sbin/ternvale-agent-start" "${overlay}/usr/bin/ternvale-agent"

log "installing into ${out}/rootfs.ext4"
docker run --rm --privileged --platform linux/arm64 \
    -v "${out}:/out" \
    -v "${overlay}:/overlay:ro" \
    "alpine:${alpine_ver}" \
    sh -c '
set -euo pipefail
apk add --no-cache e2fsprogs >/dev/null
mkdir -p /mnt
mount -o loop /out/rootfs.ext4 /mnt
if ! grep -q ternvale-agent-start /mnt/sbin/init; then
  umount /mnt
  echo "rootfs /sbin/init has no agent hook; rerun ./scripts/make-rootfs.sh" >&2
  exit 1
fi
rm -rf /mnt/lib/modules/ternvale-vsock
install -D -o 0 -g 0 -m 755 /overlay/usr/bin/ternvale-agent /mnt/usr/bin/ternvale-agent
install -D -o 0 -g 0 -m 755 /overlay/usr/sbin/ternvale-agent-start /mnt/usr/sbin/ternvale-agent-start
mkdir -p /mnt/lib/modules/ternvale-vsock
for f in /overlay/lib/modules/ternvale-vsock/*; do
  install -o 0 -g 0 -m 644 "$f" /mnt/lib/modules/ternvale-vsock/
done
ls -l /mnt/usr/bin/ternvale-agent /mnt/usr/sbin/ternvale-agent-start /mnt/lib/modules/ternvale-vsock
sync
umount /mnt
e2fsck -fy /out/rootfs.ext4 >/dev/null || [ $? -eq 1 ]
'
log "done"
