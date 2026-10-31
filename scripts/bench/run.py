#!/usr/bin/env python3
"""Step 62 harness: boot the bench guest under Ternvale and QEMU/HVF."""

from __future__ import annotations

import argparse
import json
import os
import pty
import re
import shutil
import signal
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
ASSETS = ROOT / "test-assets" / "bench"
BENCH_LINE = re.compile(r"^BENCH[ _]([A-Za-z0-9_]+)=?(\S*)$")
CHUNK = 16
CHUNK_DELAY = 0.12


def log(message: str) -> None:
    print(f"bench: {message}", file=sys.stderr, flush=True)


def host_info() -> dict[str, Any]:
    def sysctl(name: str) -> str:
        proc = subprocess.run(["sysctl", "-n", name], capture_output=True, text=True)
        return proc.stdout.strip()

    sw = subprocess.run(["sw_vers"], capture_output=True, text=True).stdout
    fields = {}
    for line in sw.splitlines():
        if ":" in line:
            key, value = line.split(":", 1)
            fields[key.strip()] = value.strip()
    return {
        "model": sysctl("hw.model"),
        "cpu": sysctl("machdep.cpu.brand_string"),
        "ncpu": int(sysctl("hw.ncpu") or "0"),
        "mem_bytes": int(sysctl("hw.memsize") or "0"),
        "macos_product": fields.get("ProductName"),
        "macos_version": fields.get("ProductVersion"),
        "macos_build": fields.get("BuildVersion"),
        "uname": subprocess.run(["uname", "-a"], capture_output=True, text=True).stdout.strip(),
    }


def parse_bench_lines(text: str) -> dict[str, Any]:
    metrics: dict[str, Any] = {}
    for raw in text.splitlines():
        line = raw.strip().replace("\r", "")
        if line == "BENCH_LOGIN":
            metrics["login_seen"] = True
            continue
        match = BENCH_LINE.match(line)
        if not match:
            continue
        key, value = match.group(1), match.group(2)
        if value in ("", "null"):
            metrics[key] = None
        else:
            try:
                metrics[key] = int(value) if value.isdigit() else float(value)
            except ValueError:
                metrics[key] = value
    return metrics


def wait_serial(
    path: Path, needle: str, timeout: float, proc: subprocess.Popen[bytes] | None = None
) -> str:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if proc is not None and proc.poll() is not None:
            snippet = path.read_text(errors="replace")[-2000:] if path.exists() else ""
            raise TimeoutError(
                f"process exited {proc.returncode} while waiting for {needle!r}\n{snippet}"
            )
        text = path.read_text(errors="replace") if path.exists() else ""
        if needle in text:
            return text
        time.sleep(0.2)
    snippet = path.read_text(errors="replace")[-2000:] if path.exists() else ""
    raise TimeoutError(f"timed out waiting for {needle!r}\n{snippet}")


def type_chunks(proc: subprocess.Popen[bytes], text: str, chunk: int) -> None:
    data = text.encode()
    assert proc.stdin is not None
    for offset in range(0, len(data), chunk):
        proc.stdin.write(data[offset : offset + chunk])
        proc.stdin.flush()
        time.sleep(CHUNK_DELAY)


def clone_disk(src: Path, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    if dest.exists():
        dest.unlink()
    cloned = subprocess.run(["cp", "-c", str(src), str(dest)])
    if cloned.returncode != 0:
        shutil.copy2(src, dest)


def sample_host_idle(seconds: int, stop: threading.Event) -> float | None:
    """Whole-machine idle % over `seconds` (macOS `top` second sample)."""
    try:
        proc = subprocess.run(
            ["top", "-l", "2", "-s", str(max(1, seconds)), "-n", "0"],
            capture_output=True,
            text=True,
            timeout=seconds + 15,
        )
    except (subprocess.TimeoutExpired, FileNotFoundError):
        stop.set()
        return None
    idle = None
    for line in proc.stdout.splitlines():
        match = re.search(r"CPU usage:.*?([0-9.]+)% idle", line)
        if match:
            idle = float(match.group(1))
    stop.set()
    return idle


def signed_ternvale() -> Path:
    binary = ROOT / "target" / "release" / "ternvale"
    if not binary.is_file():
        binary = ROOT / "target" / "debug" / "ternvale"
    if not binary.is_file():
        log("building ternvale (release)")
        subprocess.run(
            ["cargo", "build", "-p", "ternvale-cli", "--release"],
            cwd=ROOT,
            check=True,
        )
        binary = ROOT / "target" / "release" / "ternvale"
    helper = ROOT / "scripts" / "sign-and-run.sh"
    return binary if helper.is_file() else binary


def write_config(work: Path, disk: Path, cpus: int, ram_mib: int) -> Path:
    serial = work / "guest-serial.log"
    serial.write_text("")
    text = f"""name = "bench"
cpus = {cpus}
ram_mib = {ram_mib}
kernel = "{ASSETS / "Image"}"
initrd = "{ASSETS / "initramfs.cpio"}"
cmdline = "console=ttyAMA0"
serial_log = "{serial}"

[[disks]]
path = "{disk}"
read_only = false

[[nics]]
backend = "loopback"
"""
    path = work / "vm.toml"
    path.write_text(text)
    return path


def run_ternvale(
    work: Path, cpus: int, ram_mib: int, idle_secs: int, timeout: float
) -> dict[str, Any]:
    disk = work / "ternvale.ext4"
    clone_disk(ASSETS / "rootfs.ext4", disk)
    config = write_config(work, disk, cpus, ram_mib)
    serial = work / "guest-serial.log"
    binary = signed_ternvale()
    helper = ROOT / "scripts" / "sign-and-run.sh"
    argv = [str(helper), str(binary), "run", str(config)]
    env = os.environ.copy()
    env["TERNVALE_RUN_DIR"] = str(work / "run")
    env["TERNVALE_LOG"] = "info"
    Path(env["TERNVALE_RUN_DIR"]).mkdir(parents=True, exist_ok=True)
    log(f"ternvale {' '.join(argv)}")
    started = time.monotonic()
    try:
        proc = subprocess.Popen(
            argv,
            stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL,
            stderr=(work / "ternvale.stderr").open("w"),
            cwd=ROOT,
            env=env,
        )
    except OSError as error:
        return {"error": str(error), "skipped": True}
    result: dict[str, Any] = {}
    try:
        wait_serial(serial, "BENCH_LOGIN", timeout, proc)
        result["boot_to_login_ms"] = int((time.monotonic() - started) * 1000)
        log(f"ternvale login in {result['boot_to_login_ms']} ms")
        cmd = f"BENCH_IDLE_SECS={idle_secs} /root/guest-bench.sh\n"
        type_chunks(proc, cmd, CHUNK)
        idle_stop = threading.Event()
        idle_holder: list[float | None] = [None]

        def idle_job() -> None:
            wait_serial(serial, "BENCH idle_start=", timeout, proc)
            idle_holder[0] = sample_host_idle(idle_secs, idle_stop)

        idle_thread = threading.Thread(target=idle_job, daemon=True)
        idle_thread.start()
        wait_serial(serial, "BENCH finished=1", timeout + idle_secs + 30, proc)
        idle_thread.join(timeout=idle_secs + 20)
        result.update(parse_bench_lines(serial.read_text(errors="replace")))
        result["host_idle_cpu_pct"] = idle_holder[0]
        type_chunks(proc, "poweroff -f\n", CHUNK)
        proc.wait(timeout=30)
    except TimeoutError as error:
        result["error"] = str(error)
        result["serial_tail"] = (
            serial.read_text(errors="replace")[-1500:] if serial.exists() else ""
        )
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
    except Exception as error:  # noqa: BLE001 — harness must record and continue to QEMU
        result["error"] = str(error)
        proc.kill()
    code = proc.poll()
    if code not in (0, None) and "error" not in result:
        result["error"] = f"ternvale exit {code}"
    return result


def run_qemu(work: Path, cpus: int, ram_mib: int, idle_secs: int, timeout: float) -> dict[str, Any]:
    qemu = shutil.which("qemu-system-aarch64")
    if not qemu:
        return {"error": "qemu-system-aarch64 not found", "skipped": True}
    disk = work / "qemu.ext4"
    clone_disk(ASSETS / "rootfs.ext4", disk)
    serial = work / "qemu-serial.log"
    if serial.exists():
        serial.unlink()
    args = [
        qemu,
        "-M",
        "virt",
        "-accel",
        "hvf",
        "-cpu",
        "host",
        "-smp",
        str(cpus),
        "-m",
        str(ram_mib),
        "-kernel",
        str(ASSETS / "Image"),
        "-initrd",
        str(ASSETS / "initramfs.cpio"),
        "-append",
        "console=ttyAMA0",
        "-nographic",
        "-no-reboot",
        "-drive",
        f"if=none,id=hd,format=raw,file={disk}",
        "-device",
        "virtio-blk-device,drive=hd",
        "-netdev",
        "user,id=n0,hostfwd=tcp:127.0.0.1:15201-:5201",
        "-device",
        "virtio-net-device,netdev=n0",
    ]
    iperf_server = None
    if shutil.which("iperf3"):
        iperf_server = subprocess.Popen(
            ["iperf3", "-s", "-1", "-p", "5201"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    log("qemu " + " ".join(args))
    serial.write_bytes(b"")
    started = time.monotonic()
    master, slave = pty.openpty()
    proc = subprocess.Popen(
        args,
        stdin=slave,
        stdout=slave,
        stderr=(work / "qemu.stderr").open("w"),
        close_fds=True,
    )
    os.close(slave)

    def pump() -> None:
        with serial.open("ab") as out:
            while True:
                try:
                    chunk = os.read(master, 256)
                except OSError:
                    break
                if not chunk:
                    break
                out.write(chunk)
                out.flush()

    threading.Thread(target=pump, daemon=True).start()
    result: dict[str, Any] = {}
    try:
        wait_serial(serial, "BENCH_LOGIN", timeout, proc)
        result["boot_to_login_ms"] = int((time.monotonic() - started) * 1000)
        log(f"qemu login in {result['boot_to_login_ms']} ms")
        os.write(master, f"BENCH_IDLE_SECS={idle_secs} /root/guest-bench.sh\r\n".encode())
        idle_stop = threading.Event()
        idle_holder: list[float | None] = [None]

        def idle_job() -> None:
            wait_serial(serial, "BENCH idle_start=", timeout, proc)
            idle_holder[0] = sample_host_idle(idle_secs, idle_stop)

        idle_thread = threading.Thread(target=idle_job, daemon=True)
        idle_thread.start()
        wait_serial(serial, "BENCH finished=1", timeout + idle_secs + 30, proc)
        idle_thread.join(timeout=idle_secs + 20)
        result.update(parse_bench_lines(serial.read_text(errors="replace")))
        result["host_idle_cpu_pct"] = idle_holder[0]
        if shutil.which("iperf3") and result.get("iperf3_server"):
            iperf = subprocess.run(
                ["iperf3", "-c", "127.0.0.1", "-p", "15201", "-t", "10", "-J"],
                capture_output=True,
                text=True,
                timeout=30,
            )
            if iperf.returncode == 0:
                data = json.loads(iperf.stdout)
                bps = data.get("end", {}).get("sum_received", {}).get("bits_per_second")
                if bps is not None:
                    result["iperf3_host_to_guest_mbps"] = float(bps) / 1e6
        os.write(master, b"poweroff -f\r\n")
        proc.wait(timeout=30)
    except TimeoutError as error:
        result["error"] = str(error)
        result["serial_tail"] = (
            serial.read_text(errors="replace")[-1500:] if serial.exists() else ""
        )
        proc.kill()
    finally:
        if iperf_server is not None and iperf_server.poll() is None:
            iperf_server.terminate()
    return result


def require_assets() -> None:
    for name in ("Image", "initramfs.cpio", "rootfs.ext4"):
        path = ASSETS / name
        if not path.is_file():
            raise SystemExit(
                f"missing {path} (run ./scripts/bench/make-image.sh; see scripts/bench/README.md)"
            )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cpus", type=int, default=int(os.environ.get("BENCH_CPUS", "4")))
    parser.add_argument("--ram-mib", type=int, default=int(os.environ.get("BENCH_RAM_MIB", "2048")))
    parser.add_argument(
        "--idle-secs", type=int, default=int(os.environ.get("BENCH_IDLE_SECS", "60"))
    )
    parser.add_argument("--timeout", type=float, default=300.0)
    parser.add_argument("--skip-ternvale", action="store_true")
    parser.add_argument("--skip-qemu", action="store_true")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    require_assets()
    stamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")
    work = ROOT / "target" / "bench-logs" / stamp
    work.mkdir(parents=True, exist_ok=True)
    out = args.out or (ROOT / "bench-results" / f"{stamp}.json")
    out.parent.mkdir(parents=True, exist_ok=True)
    payload: dict[str, Any] = {
        "timestamp": stamp,
        "hardware": host_info(),
        "config": {
            "cpus": args.cpus,
            "ram_mib": args.ram_mib,
            "idle_secs": args.idle_secs,
            "guest_image": str(ASSETS),
        },
        "runs": {},
    }
    if not args.skip_ternvale:
        log("starting Ternvale run")
        payload["runs"]["ternvale"] = run_ternvale(
            work, args.cpus, args.ram_mib, args.idle_secs, args.timeout
        )
    if not args.skip_qemu:
        log("starting QEMU/HVF run")
        payload["runs"]["qemu"] = run_qemu(
            work, args.cpus, args.ram_mib, args.idle_secs, args.timeout
        )
    out.write_text(json.dumps(payload, indent=2) + "\n")
    log(f"wrote {out}")
    print(out)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
