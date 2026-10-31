#!/bin/sh
# Guest-side Step 62 workload. Prints one `BENCH key=value` line per metric.
# Warm-up plus median-of-3 on fio/STREAM: those moved >5% run-to-run on QEMU/HVF
# (cache fill, frequency scaling). Sysbench gets a short discarded warm-up.
set -eu

nproc=$(nproc 2>/dev/null || echo 1)
echo "BENCH nproc=${nproc}"

bring_up_net() {
    if [ ! -e /sys/class/net/eth0 ]; then
        echo "BENCH net=no_eth0"
        return 1
    fi
    ip link set eth0 up 2>/dev/null || true
    ip addr add 10.0.2.15/24 dev eth0 2>/dev/null || true
    ip route add default via 10.0.2.2 2>/dev/null || true
    echo "BENCH net=eth0"
}

bring_up_net || true

python3 - "$nproc" <<'PY'
import json, statistics, subprocess, sys
from pathlib import Path

NPROC = sys.argv[1]
Path("/var/tmp").mkdir(parents=True, exist_ok=True)


def run(cmd, timeout=120):
    return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)


def median(values):
    return float(statistics.median(values))


def sysbench(threads, seconds):
    out = run(
        [
            "sysbench",
            "cpu",
            f"--threads={threads}",
            f"--time={seconds}",
            "run",
        ]
    ).stdout
    for line in out.splitlines():
        if "events per second:" in line:
            return float(line.split()[-1])
    return None


# Discarded warm-up so the timed run is not the first burst on a cold core.
sysbench(1, 2)
eps1 = sysbench(1, 10)
epsn = sysbench(NPROC, 10)
if eps1 is not None:
    print(f"BENCH sysbench_cpu_1_eps={eps1:.4f}")
if epsn is not None:
    print(f"BENCH sysbench_cpu_n_eps={epsn:.4f}")
print(f"BENCH sysbench_cpu_n_threads={NPROC}")


def fio_once(name, rw, bs, size):
    cmd = [
        "fio",
        "--name=" + name,
        "--rw=" + rw,
        "--bs=" + bs,
        "--size=" + size,
        "--filename=/var/tmp/fio.bin",
        "--direct=1",
        "--ioengine=psync",
        "--output-format=json",
        "--unlink=1",
    ]
    proc = run(cmd)
    if proc.returncode != 0:
        return None
    job = json.loads(proc.stdout)["jobs"][0]
    side = job["read"] if rw in ("read", "randread") else job["write"]
    return float(side.get("bw", 0)), float(side.get("iops", 0))


def fio_job(name, rw, bs):
    fio_once(name + "_warm", rw, bs, "16m")
    samples = []
    for _ in range(3):
        got = fio_once(name, rw, bs, "128m")
        if got is not None:
            samples.append(got)
    if not samples:
        print(f"BENCH fio_{name}_error=1")
        return
    print(f"BENCH fio_{name}_kbs={median([s[0] for s in samples]):.1f}")
    print(f"BENCH fio_{name}_iops={median([s[1] for s in samples]):.1f}")


fio_job("seqread", "read", "1m")
fio_job("seqwrite", "write", "1m")
fio_job("randread", "randread", "4k")
fio_job("randwrite", "randwrite", "4k")

iperf = run(["iperf3", "-c", "10.0.2.2", "-t", "10", "-J"], timeout=30)
mbps = None
if iperf.returncode == 0:
    try:
        bps = json.loads(iperf.stdout).get("end", {}).get("sum_sent", {}).get(
            "bits_per_second"
        )
        if bps is not None:
            mbps = float(bps) / 1e6
    except json.JSONDecodeError:
        mbps = None
if mbps is None:
    print("BENCH iperf3_guest_to_host_mbps=null")
else:
    print(f"BENCH iperf3_guest_to_host_mbps={mbps:.3f}")

subprocess.run(["iperf3", "-s", "-D", "-p", "5201"], capture_output=True)
print("BENCH iperf3_server=5201")


def stream_once():
    out = run(["stream"]).stdout
    got = {}
    for line in out.splitlines():
        for key, label in (
            ("copy", "Copy:"),
            ("scale", "Scale:"),
            ("add", "Add:"),
            ("triad", "Triad:"),
        ):
            if line.startswith(label):
                got[key] = float(line.split()[1])
    return got


run(["stream"])
stream_samples = [stream_once() for _ in range(3)]
stream_samples = [s for s in stream_samples if s]
if not stream_samples:
    for key in ("copy", "scale", "add", "triad"):
        print(f"BENCH stream_{key}_mbs=null")
else:
    for key in ("copy", "scale", "add", "triad"):
        vals = [s[key] for s in stream_samples if key in s]
        print(f"BENCH stream_{key}_mbs={median(vals):.2f}")
PY

idle=${BENCH_IDLE_SECS:-60}
echo "BENCH idle_start=${idle}"
sleep "${idle}"
echo "BENCH idle_done=1"
echo "BENCH finished=1"
