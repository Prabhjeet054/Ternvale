# Ternvale benchmark harness (Step 62)

`make bench` boots the same Alpine arm64 guest under Ternvale and under
`qemu-system-aarch64 -accel hvf`, runs a fixed workload, and writes
`bench-results/<timestamp>.json`.

## Guest image

Documented recipe (Alpine 3.20 aarch64, linux-virt kernel):

```sh
./scripts/bench/make-image.sh
```

Produces `test-assets/bench/{Image,initramfs.cpio,rootfs.ext4}`:

| Package | Role |
| --- | --- |
| `sysbench` (community) | CPU events/sec, 1 and `nproc` threads |
| `fio` | sequential and random read/write on virtio-blk |
| `iperf3` | guest↔host throughput |
| `stream` | compiled from `scripts/bench/stream.c` (Copy/Scale/Add/Triad, 10⁷ doubles) |
| `python3` | parse fio/iperf JSON in the guest |

Needs Docker Desktop (`linux/arm64`, privileged loop mounts), `curl`, `python3`,
`gzip`, and `cpio`. Rebuild after changing `scripts/bench/guest-bench.sh`.

The guest script discards a short sysbench/fio/STREAM warm-up, then reports the
**median of 3** fio and STREAM samples. Sequential fio and STREAM moved more
than 5% run-to-run on QEMU/HVF (cache fill and frequency scaling); the median
keeps `compare.py`'s 10% gate from firing on noise.

Default disk is 1024 MiB (`SIZE_MIB`). Default VM is 4 vCPUs and 2048 MiB
(`BENCH_CPUS`, `BENCH_RAM_MIB`).

## Networking

QEMU uses user-mode SLIRP (`10.0.2.2` is the host) plus
`hostfwd=tcp:127.0.0.1:15201-:5201` for host→guest iperf3.

Ternvale's virtio-net `loopback` backend answers ARP/ICMP only (see
`docs/NET.md`). TCP iperf3 is recorded as unavailable until a user-mode or
vmnet backend exists. The NIC is still attached so boot cost matches QEMU.

## Compare

```sh
python3 scripts/bench/compare.py bench-results/a.json bench-results/b.json
python3 scripts/bench/compare.py --threshold 5 old.json new.json
python3 scripts/bench/test_compare.py
```

Higher-is-better metrics (throughput, events/sec, STREAM, host idle %) regress
when they drop by more than the threshold. Lower-is-better (`boot_to_login_ms`)
regress when they rise. Missing/`null` metrics are skipped.

Idle host CPU is sampled with `top -l 2 -s 60` while the guest runs `sleep 60`
(`BENCH_IDLE_SECS`, default 60).
