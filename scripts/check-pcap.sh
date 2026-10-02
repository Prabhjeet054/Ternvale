#!/bin/bash
# Check a virtio-net pcap from the `net` boot scenario with tshark display filters.
#
# macOS has no tshark by default, so this runs Alpine's tshark package in a
# linux/arm64 container with the capture mounted read-only. It prints every
# frame, then counts frames matching each filter below. The expectations match
# `ping -c 3 10.0.2.2` from 10.0.2.15 against the loopback gateway.
#
# Usage:
#   ./scripts/check-pcap.sh path/to/net.pcap
#
# Exit 0 only when every check passes; 7 when one fails.
set -euo pipefail

if [[ $# -ne 1 ]]; then
    printf 'usage: %s PCAP\n' "$0" >&2
    exit 2
fi
pcap=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
alpine_ver="${ALPINE_VER:-3.20}"

log() {
    printf 'check-pcap: %s\n' "$*" >&2
}

if [[ ! -s "$pcap" ]]; then
    log "missing or empty capture $pcap"
    exit 2
fi
if ! docker info >/dev/null 2>&1; then
    log "docker is not reachable; start Docker Desktop and retry"
    exit 2
fi

log "checking $pcap"
set +e
docker run --rm --platform linux/arm64 \
    -v "${pcap}:/cap/net.pcap:ro" \
    "alpine:${alpine_ver}" \
    sh -c '
apk add --no-cache tshark >/dev/null 2>&1 || exit 3
# -2: two passes, so a request can point forward to its reply (icmp.resp_in).
ts() { tshark -2 -r /cap/net.pcap -o ip.check_checksum:TRUE "$@" 2>/dev/null; }
echo "--- frames ---"
ts
echo "--- checks ---"
failed=0
check() {
  got=$(ts -Y "$3" | wc -l | tr -d " ")
  if [ "$got" "$1" "$2" ]; then verdict=ok; else verdict=FAIL; failed=1; fi
  printf "%-4s %3s (want %s %s)  %s\n" "$verdict" "$got" "$1" "$2" "$3"
}
check -ge 1 "arp.opcode == 1 && arp.src.proto_ipv4 == 10.0.2.15 && arp.dst.proto_ipv4 == 10.0.2.2"
check -ge 1 "arp.opcode == 2 && arp.src.proto_ipv4 == 10.0.2.2 && arp.src.hw_mac == 52:55:0a:00:02:02"
check -eq 3 "icmp.type == 8 && ip.src == 10.0.2.15 && ip.dst == 10.0.2.2"
check -eq 3 "icmp.type == 0 && ip.src == 10.0.2.2 && ip.dst == 10.0.2.15 && eth.src == 52:55:0a:00:02:02"
check -eq 3 "icmp.type == 8 && icmp.resp_in"
check -eq 0 "icmp.no_resp"
check -eq 6 "icmp.checksum.status == 1 && ip.checksum.status == 1"
check -eq 0 "ip.checksum.status == 0 || icmp.checksum.status == 0"
exit $failed
'
status=$?
set -e
[[ "$status" -eq 1 ]] && status=7
log "status=${status}"
exit "$status"
