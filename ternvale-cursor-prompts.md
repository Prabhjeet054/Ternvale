# Ternvale: Cursor Prompt Guide (30 Steps)

A step-by-step build plan for a from-scratch virtual machine monitor (VMM) for Apple Silicon Macs, written as prompts you paste into Cursor. Each step has a **Build prompt** and a **Test prompt**.

**Project name:** Ternvale. A tern is a bird that migrates between continents, which fits software that carries one operating system into another's world. A vale is a sheltered valley, which fits the idea of a safe space for guests to run in.

> **Before you commit to the name:** I only checked that no obvious software product called "Ternvale" shows up in a web search. That is not a trademark clearance. Check IP India (you are based in India), the USPTO trademark database, WIPO Global Brand Database, plus domain, GitHub, crates.io and Mac App Store availability. Backup names to run through the same checks: **Quillfold**, **Aerowick**, **Halcyvane**.

---

## Contents

1. [How to use this guide](#how-to-use-this-guide)
2. [Rules Cursor must follow at all times](#rules-cursor-must-follow-at-all-times)
3. [Prefix for every prompt](#prefix-for-every-prompt)
4. [Phase A: Foundation (Steps 1-6)](#phase-a-foundation-steps-1-6)
5. [Phase B: vCPU and MMIO core (Steps 7-12)](#phase-b-vcpu-and-mmio-core-steps-7-12)
6. [Phase C: Booting Linux (Steps 13-18)](#phase-c-booting-linux-steps-13-18)
7. [Phase D: Virtio devices and SMP (Steps 19-25)](#phase-d-virtio-devices-and-smp-steps-19-25)
8. [Phase E: Platform features and product basics (Steps 26-30)](#phase-e-platform-features-and-product-basics-steps-26-30)
9. [Debugging playbook](#debugging-playbook)
10. [What comes after Step 30](#what-comes-after-step-30)

---

## How to use this guide

1. Create the repo, then create `.cursor/rules/ternvale.mdc` and paste in the rules from the next section. It is set to `alwaysApply: true`, so Cursor loads it in every chat.
2. Create an empty `docs/PROGRESS.md`. Cursor updates it after every step, and it carries context between chats.
3. **Use one fresh Cursor chat per step.** Paste the prefix, then the Build prompt.
4. When the build finishes, paste the Test prompt in the same chat.
5. Do not start the next step until the test prompt passes and you have read the logs yourself.
6. Commit after every passing step (`git commit -m "step-NN: <title>"`).
7. Anything Cursor marks `TODO(verify)` must be checked against the ARM Architecture Reference Manual, Apple's Hypervisor.framework docs, the virtio spec, or QEMU's behavior. Do not trust it blindly, because this is a domain where AI tools are often confidently wrong.

**Prerequisites** (from the earlier guide): Apple Silicon Mac on a recent macOS, Xcode command line tools, Rust via `rustup`, `dtc`, `qemu` (as a reference VMM), and `llvm` (for `clang`, `llvm-objcopy`, `llvm-objdump`). Install with `brew install qemu dtc llvm`.

---

## Rules Cursor must follow at all times

Save this as `.cursor/rules/ternvale.mdc`:

```text
---
description: Ternvale project rules
alwaysApply: true
---

# Ternvale project rules (ALWAYS FOLLOW)

## Language & layout
- VMM core is Rust (edition 2021, stable). Workspace crates: ternvale-log, ternvale-config,
  ternvale-hv, ternvale-vmm, ternvale-devices, ternvale-cli. Guest test payloads live in
  guest-tests/.
- Target is aarch64-apple-darwin ONLY. Guests are ARM64 only. Never add x86 emulation.
- Keep files under ~400 lines; one responsibility per module.

## Logging (MANDATORY)
- Use the `tracing` crate everywhere. NEVER use println!/eprintln!/dbg! outside the CLI's
  user-facing output.
- Every public function: `#[tracing::instrument(level="debug", skip_all, fields(...))]`
  or an explicit span carrying vm_id / vcpu_id / device name.
- Log targets by subsystem: ternvale::hv, ternvale::mem, ternvale::vcpu, ternvale::mmio,
  ternvale::boot, ternvale::gic, ternvale::psci, ternvale::virtio::<dev>, ternvale::uart,
  ternvale::net, ternvale::cli.
- Levels: ERROR = unrecoverable or guest-fatal; WARN = unexpected but handled;
  INFO = lifecycle (start/stop/boot milestones); DEBUG = decisions and state changes;
  TRACE = every hv_* call with return code, every MMIO access (addr, size, value, direction),
  every virtqueue operation.
- Every hv_* FFI call is logged at TRACE with args + return code; a non-zero return is ERROR.
- Every `?`-propagated error must gain context (`.context()` / `map_err`) naming the operation.
- Log fields are structured (`addr=%format!("{:#x}", a)`), never string-concatenated.
- Logs go to stderr AND a file: ~/Library/Logs/Ternvale/ternvale-<vm>-<YYYYMMDD-HHMMSS>.log.
  Level is set by env TERNVALE_LOG (default info). Guest serial output goes to its own file.
- A panic hook must log the panic message, location and backtrace before exit.

## Errors & safety
- Library crates use `thiserror` typed errors; only ternvale-cli uses `anyhow`.
- No `unwrap()` / `expect()` outside tests. No silent `let _ =` on Results.
- Every `unsafe` block has a `// SAFETY:` comment stating the invariant and is as small as
  possible. Raw FFI lives ONLY in ternvale-hv; other crates use its safe wrappers.
- Never trust guest-controlled values (addresses, lengths, indices). Validate first;
  on bad input log WARN and fail the request, never panic.

## Testing & workflow
- Every task ends with: `cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`.
  Fix all failures before declaring the task done.
- Every module gets unit tests; every device gets a test that exercises it through the MMIO
  bus without a real VM where possible.
- Tests needing the hypervisor are marked `#[ignore = "needs-hv"]` and run through
  scripts/sign-and-run.sh (ad-hoc codesign with the hypervisor entitlement).
- Never claim something works without running it. Show the exact commands and their output.
- Do not add dependencies without stating why; prefer well-maintained crates.
- Do not change files outside the current task's scope. Do not refactor unrelated code.
- If a spec detail (ARM ARM, virtio, Hypervisor.framework) is uncertain, say so, name what
  to verify, and add a TODO(verify) instead of guessing.
- After each task, append to docs/PROGRESS.md: what changed, how to test, known issues.
- One step = one commit-sized change.
```

---

## Prefix for every prompt

Paste this at the top of every Build prompt and Test prompt:

```text
Follow .cursor/rules/ternvale.mdc. Read docs/PROGRESS.md first. Add tracing logs per the
logging rules. Run fmt/clippy/test at the end and update docs/PROGRESS.md.
```

---

# Phase A: Foundation (Steps 1-6)

## Step 1: Workspace and docs skeleton

**Build prompt**
```text
Create a Cargo workspace "ternvale" with crates: ternvale-log, ternvale-config, ternvale-hv,
ternvale-vmm, ternvale-devices, ternvale-cli. Add rust-toolchain.toml (stable, target
aarch64-apple-darwin), rustfmt.toml, clippy config, .gitignore, README.md, docs/PROGRESS.md,
docs/ARCHITECTURE.md (layer diagram: CLI -> vmm -> hv/devices), and docs/DEBUGGING.md (empty
headings for now). Each crate compiles with a stub lib.rs. Do not implement functionality yet.
```

**Test prompt**
```text
Run `cargo build --workspace`, `cargo clippy --workspace -- -D warnings` and
`cargo test --workspace`. Verify the workspace has exactly the six crates and that every crate
name matches the rules file. Report the outputs.
```

## Step 2: Logging crate

**Build prompt**
```text
Implement ternvale-log. Provide `init(LogConfig{vm_name, level, log_dir, json: bool})` that sets
up tracing-subscriber with: a stderr layer (human readable), a file layer via tracing-appender at
~/Library/Logs/Ternvale/ternvale-<vm>-<timestamp>.log, EnvFilter from TERNVALE_LOG, timestamps,
targets and thread ids. Add a panic hook that logs message, location and backtrace. Return a
guard that flushes on drop. Add a `log_hv_call!` macro that logs name, args and result code at
TRACE and at ERROR on non-zero. Document all log targets in docs/DEBUGGING.md.
```

**Test prompt**
```text
Write tests that: (1) init logging into a temp dir and confirm the file is created and contains
a written INFO line; (2) TERNVALE_LOG=ternvale::mmio=trace,info filters correctly; (3) the panic
hook writes a panic entry to the file; (4) the log_hv_call! macro logs ERROR on non-zero codes.
Run them and show the log file contents.
```

## Step 3: Errors and config model

**Build prompt**
```text
Implement ternvale-config with our OWN VM config format (serde + TOML): name, cpus, ram_mib,
kernel path, initrd path, cmdline, disks[{path, read_only}], nics[], serial log path, firmware
path (optional). Add validation (ram is a multiple of 16 MiB, cpus 1..=16, files exist) returning
a typed ConfigError via thiserror. Log validation decisions at DEBUG and failures at ERROR. Add a
shared TernvaleError enum in ternvale-config for cross-crate use.
```

**Test prompt**
```text
Write unit tests for: valid config round trip, each validation failure (bad ram, zero cpus,
missing kernel), and unknown-field rejection. Include a test asserting the error message names
the offending field. Run and show results.
```

## Step 4: Signing, entitlements and test runner

**Build prompt**
```text
Add entitlements/ternvale.entitlements with com.apple.security.hypervisor. Add
scripts/sign-and-run.sh that ad-hoc codesigns the given binary with those entitlements, then
execs it with the remaining args. Configure .cargo/config.toml so `cargo test` and `cargo run`
use it as the runner for aarch64-apple-darwin. Add a Makefile with targets: build, test,
test-hv, lint, sign, clean. The script must log each step and exit non-zero on failure.
```

**Test prompt**
```text
Create a trivial test marked #[ignore = "needs-hv"] that only asserts true. Run `make test` and
`make test-hv`. Then run `codesign -d --entitlements - <binary>` on a built test binary and show
that the hypervisor entitlement is present. Report any failures.
```

## Step 5: Safe Hypervisor.framework wrapper (VM lifecycle)

**Build prompt**
```text
In ternvale-hv, declare the FFI for Hypervisor.framework (hand-written extern "C" or bindgen;
link -framework Hypervisor). Implement a safe `Vm` type: `Vm::create()` -> hv_vm_create, Drop ->
hv_vm_destroy, with a typed HvError mapping hv_return_t codes to names (HV_ERROR, HV_BUSY,
HV_BAD_ARGUMENT, HV_NO_RESOURCES, HV_DENIED, HV_UNSUPPORTED, ...). Every call uses
log_hv_call!. Only one Vm may exist per process: enforce and log it. No raw pointers are
exposed publicly. Verify the exact return codes against Apple's headers and add TODO(verify)
where unsure.
```

**Test prompt**
```text
Write an ignored needs-hv test: create a Vm, drop it, create another. Write a non-hv test for
the error-code mapping. Add a test that a second concurrent Vm::create returns a typed error.
Run via `make test-hv` and show the trace log lines containing the hv_vm_create return codes.
```

## Step 6: Guest memory manager

**Build prompt**
```text
In ternvale-vmm add `GuestMemory`: allocates host memory via mmap (anonymous, zeroed), maps it
into the guest with hv_vm_map at a given GPA, and unmaps on drop. Enforce 16 KiB alignment for
GPA, size and host address (Apple Silicon host page size), returning typed errors otherwise.
Provide bounds-checked read/write helpers (read_u8/16/32/64, write_*, read_bytes/write_bytes)
using guest physical addresses, supporting multiple regions. Log every map/unmap and every
rejected access.
```

**Test prompt**
```text
Unit tests (no HV needed for the address-translation layer): out-of-range read/write,
cross-region access, misalignment rejection, overlap rejection. One needs-hv test maps 64 MiB
at 0x4000_0000. Also verify the host page size at runtime via sysconf and log it. Run all and
show results.
```

---

# Phase B: vCPU and MMIO core (Steps 7-12)

## Step 7: vCPU create and run loop skeleton

**Build prompt**
```text
In ternvale-vmm implement `Vcpu`: create (hv_vcpu_create with exit-info pointer), set/get
registers (PC, X0..X30, SP_EL1, CPSR, and key system registers via hv_vcpu_set_sys_reg), and
`run()` calling hv_vcpu_run and returning an `ExitReason` enum built from hv_vcpu_exit_t
(exception, vtimer_activated, canceled, unknown). A vCPU must be created and run on the same
thread: enforce that with a thread-id check and log it. Add a stop flag using hv_vcpus_exit.
No exception decoding yet beyond logging the raw syndrome at TRACE.
```

**Test prompt**
```text
needs-hv test: create a Vcpu, set PC to a mapped page containing an `hvc #0` instruction, run
once, and assert we get an exception exit whose raw syndrome is logged. Also test that calling
run() from a different thread than the creator returns a typed error. Show the logs.
```

## Step 8: ESR decoding and MMIO exit trace

**Build prompt**
```text
Add an `esr` module decoding ESR_EL2 syndromes: EC field (WFI/WFE 0x01, HVC64 0x16, SMC64 0x17,
sysreg trap 0x18, instruction abort 0x20/0x21, data abort 0x24/0x25, BRK 0x3C), and ISS for data
aborts (ISV, SAS size, SRT register, WnR write flag, SF). Produce a typed `ExitEvent` enum:
Mmio{gpa,size,write,reg}, Hvc, Smc, Wfi, SysReg{...}, Unknown{ec,iss}. Log every decoded event
at TRACE with all fields; log unknown events at ERROR with the raw ESR. Add a TODO(verify) note
about PC-advance rules differing between HVC and SMC.
```

**Test prompt**
```text
Table-driven unit tests with hand-crafted ESR values for each EC (no HV needed): a 32-bit write
by X3, a 64-bit read into X7, the ISV=0 case, HVC with imm16, and an unknown EC. Assert the
decoded fields exactly. Show test output.
```

## Step 9: Bare-metal guest test payload

**Build prompt**
```text
In guest-tests/ create a tiny AArch64 assembly program (built with clang
--target=aarch64-none-elf + llvm-objcopy to a raw .bin, via a Makefile target) that loads
0x0900_0000 into x1 and writes the bytes of "Hello from Ternvale\n" one at a time with `strb`,
then executes `hvc #0` to signal "done". Write a loader in ternvale-vmm that copies a raw binary
into guest RAM at 0x4000_0000 and sets PC there. Log the loaded size, address and a sha256 of
the payload.
```

**Test prompt**
```text
Build the payload, run `llvm-objdump -d` and confirm the instruction sequence. Then run it in a
Vcpu and verify each `strb` produces an MMIO write exit at 0x0900_0000 (logged), followed by the
HVC. Print the captured bytes. There is no UART device yet: just collect the bytes and assert
the string in the test.
```

## Step 10: MMIO bus

**Build prompt**
```text
Implement `MmioBus` in ternvale-vmm: a trait `MmioDevice { name(), read(offset,size)->u64,
write(offset,size,val) }`, registration of non-overlapping GPA ranges (reject overlaps with
typed errors), and dispatch from ExitEvent::Mmio. Unmapped reads return 0 with a WARN; unmapped
writes are ignored with a WARN. Every access logs at TRACE: device name, offset, size, value,
direction. Maintain per-device access counters, dumped at DEBUG on shutdown. Write results back
to the destination register on reads.
```

**Test prompt**
```text
Unit tests with a mock device: overlap rejection, boundary addresses (first/last byte),
unmapped access behavior, size handling (1/2/4/8), and counter accuracy. Also an integration
test running the Step 9 payload through the bus into a mock device and asserting the received
string.
```

## Step 11: PL011 UART

**Build prompt**
```text
Implement a PL011 UART device in ternvale-devices at 0x0900_0000 (registers UARTDR, UARTFR,
UARTIBRD, UARTFBRD, UARTLCR_H, UARTCR, UARTIMSC, UARTRIS, UARTMIS, UARTICR, plus the
PeriphID/PCellID registers). TX: append bytes to an output sink (stdout AND the serial log
file). RX: a queue for host stdin input with FR flags set correctly. Interrupt raising is a
stub for now (log it). Log register accesses at TRACE and TX'd bytes at DEBUG (batched per
line).
```

**Test prompt**
```text
Unit tests: write bytes via UARTDR and check sink output; FR TXFF/RXFE bits; PeriphID reads
match the PL011 values; a write to a read-only register is ignored with a WARN. Run the Step 9
payload with the real UART attached and confirm "Hello from Ternvale" appears on stdout and in
the serial log file.
```

## Step 12: Platform memory map

**Build prompt**
```text
Create `platform.rs` in ternvale-vmm as the single source of truth for the guest physical
layout: RAM base 0x4000_0000, GIC distributor/redistributor bases, UART 0x0900_0000, RTC, a
virtio-mmio window (32 slots x 0x200), PCIe ECAM and MMIO windows (reserved for later), and a
flash/firmware region. Model it on QEMU's `virt` machine layout but document every choice in
docs/ARCHITECTURE.md. Add a `validate()` that checks no overlaps and 16 KiB alignment, and a
`dump()` that logs the full map at INFO at VM start.
```

**Test prompt**
```text
Tests: validate() passes for the default; a deliberately overlapping layout fails; every region
is 16 KiB-aligned; RAM sizes of 256 MiB, 1 GiB and 4 GiB don't collide with device windows.
Compare our map against `qemu-system-aarch64 -M virt,dumpdtb=virt.dtb` decompiled with dtc, and
list the differences in docs/ARCHITECTURE.md.
```

---

# Phase C: Booting Linux (Steps 13-18)

## Step 13: Linux arm64 Image loader

**Build prompt**
```text
Implement a boot loader for the Linux arm64 `Image`: parse the 64-byte header (magic "ARM\x64"
at offset 56, text_offset, image_size, flags), place the kernel 2 MiB-aligned above RAM base
(honoring text_offset), place the initramfs and DTB at safe, non-overlapping addresses, and set
boot registers (x0 = DTB GPA, x1-x3 = 0, PC = kernel entry, CPSR = EL1h with interrupts
masked). Log every address chosen and reject a bad magic with a typed error. Add docs/BOOT.md
summarizing Documentation/arm64/booting.rst from the Linux kernel.
```

**Test prompt**
```text
Unit tests with a synthetic header: bad magic, zero image_size, kernel larger than RAM, and
overlap detection between kernel/initrd/dtb. Then add scripts/fetch-test-kernel.sh (or
documented Buildroot steps) that produces a minimal arm64 Image + busybox initramfs in
test-assets/. Verify the loader logs the correct entry address for it.
```

## Step 14: Device tree generation

**Build prompt**
```text
Generate the guest DTB in code (use the vm-fdt crate): root (#address-cells/#size-cells=2,
compatible), /chosen (bootargs, stdout-path, initrd-start/end), /memory, /cpus with PSCI enable
method, /psci (method="hvc"), /timer (armv8-timer PPIs), /intc (GICv3 with correct reg + PPI
config), /pl011@9000000 with clocks (apb-pclk fixed clock), and placeholders for virtio-mmio
nodes. Write the DTB into guest memory. Log the full DTB size and dump a decompiled copy to the
log directory when TERNVALE_DUMP_DTB=1.
```

**Test prompt**
```text
Generate a DTB, decompile it with `dtc -I dtb -O dts`, and assert with a test that it contains
the expected nodes and reg values from platform.rs. Also run dtc validation and confirm zero
warnings. Diff key nodes against QEMU's virt dtb and summarize the differences.
```

## Step 15: GICv3 via Hypervisor.framework

**Build prompt**
```text
In ternvale-hv add safe wrappers for the in-kernel GIC APIs (hv_gic_config_create, set
distributor and redistributor base, hv_gic_create, hv_gic_set_spi, and state/ICC register access
as needed), gated by a runtime OS-version check with a clear error if unsupported (verify the
required macOS version in Apple's docs and record it). The GIC must be created after
hv_vm_create and before vCPUs are created: enforce that ordering in the Vm builder and log it.
Expose `Gic::set_spi(irq, level)`. Ensure DTB GIC addresses come from platform.rs.
```

**Test prompt**
```text
needs-hv tests: create VM + GIC + one vCPU; assert ordering violations return typed errors;
assert the unsupported-OS path produces the expected error (simulate via an injected version).
Log the GIC distributor/redistributor sizes reported by the framework and compare them with
platform.rs.
```

## Step 16: Timer, WFI and PSCI handling

**Build prompt**
```text
Handle in the vCPU loop: HV_EXIT_REASON_VTIMER_ACTIVATED (mask/unmask via
hv_vcpu_set_vtimer_mask and inject the timer PPI through the GIC path per Apple's docs), WFI
(yield / wait for interrupt with a host-side sleep that a pending interrupt can cancel), and
PSCI over HVC/SMC: PSCI_VERSION, PSCI_FEATURES, CPU_ON, CPU_OFF, SYSTEM_OFF, SYSTEM_RESET,
MIGRATE_INFO_TYPE. Return PSCI codes in x0 correctly, advance PC correctly for HVC vs SMC
(check and record the rule), and shut the VM down cleanly on SYSTEM_OFF. Log every PSCI call at
DEBUG with args and return value.
```

**Test prompt**
```text
Unit test the PSCI dispatcher with table-driven inputs (function ID -> expected return value).
Extend guest-tests with a payload that calls PSCI_VERSION via hvc and prints the result over
the UART, then SYSTEM_OFF; run it and assert the exit is clean with the right printed version.
```

## Step 17: Boot Linux to a shell

**Build prompt**
```text
Create the `Vm` orchestrator in ternvale-vmm that wires everything: config -> memory -> GIC ->
bus -> UART -> DTB -> loader -> vCPU thread(s) -> run loop, with a clean shutdown path. Wire UART
interrupts to the GIC SPI and route host stdin to UART RX. Default kernel cmdline:
"console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init". Add a watchdog that logs a WARN with
the last 20 MMIO events and last PC if no MMIO/exit activity happens for 10 seconds (hang
detector).
```

**Test prompt**
```text
Boot the test kernel + initramfs. Success = kernel banner and a busybox prompt on serial within
60 seconds. If it hangs, use the watchdog output and the TRACE log to diagnose and fix, and
document what went wrong in docs/DEBUGGING.md. Run `echo hello` in the guest via stdin and show
it working.
```

## Step 18: Automated boot regression harness

**Build prompt**
```text
Add an integration test harness (a Rust test binary plus scripts/boot-test.sh) that boots the
kernel, feeds stdin commands, and matches expected serial output with timeouts (expect-style).
Scenarios: boot banner, `uname -a` contains aarch64, `echo OK`, and `poweroff` causes a clean
SYSTEM_OFF with exit code 0. Save the per-run logs and serial output as CI artifacts in
target/boot-logs/.
```

**Test prompt**
```text
Run the harness 10 times in a loop and report pass/fail counts and boot times. Any flake must be
investigated using the logs and fixed. Add a deliberately broken cmdline scenario and confirm
the harness reports a timeout with the last serial lines and a pointer to the log file.
```

---

# Phase D: Virtio devices and SMP (Steps 19-25)

## Step 19: Virtio-mmio transport

**Build prompt**
```text
Implement the virtio-mmio v2 (virtio 1.x) transport in ternvale-devices: registers MagicValue
"virt", Version=2, DeviceID, VendorID, DeviceFeatures/sel, DriverFeatures/sel, QueueSel,
QueueNumMax, QueueNum, QueueReady, QueueNotify, InterruptStatus/ACK, Status,
QueueDesc/Avail/Used low/high, ConfigGeneration, and device config space. Feature negotiation
must require VIRTIO_F_VERSION_1. Model the status state machine (ACKNOWLEDGE, DRIVER,
FEATURES_OK, DRIVER_OK, FAILED, reset) with logs for every transition. Make it generic over a
`VirtioDevice` trait. Register on the bus using the virtio-mmio slots in platform.rs, and
generate the matching DTB nodes.
```

**Test prompt**
```text
Unit tests driving the register interface like a guest driver: correct handshake sequence,
feature negotiation without VERSION_1 rejected, an out-of-order status write sets a logged
error, and device reset clears queues. Boot Linux with a dummy device and confirm the kernel's
virtio-mmio probe messages appear (dmesg | grep virtio).
```

## Step 20: Split virtqueue

**Build prompt**
```text
Implement a split-virtqueue engine: read the descriptor table/avail ring/used ring from guest
memory using GuestMemory helpers; support chained descriptors, the WRITE flag, INDIRECT
descriptors, and EVENT_IDX (optional, behind a feature flag). Provide an iterator yielding
descriptor chains as (readable buffers, writable buffers) and `add_used(head, len)`.
Defensively validate everything the guest controls (indices, lengths, loops, out-of-range
addresses) and never panic on bad input: log WARN and mark the queue broken. Log queue ops at
TRACE.
```

**Test prompt**
```text
Unit tests building rings in a fake guest memory: single/chained/indirect chains, wraparound of
16-bit indices, descriptor loops, out-of-bounds address, zero-length descriptors. Add a
fuzz-style test that feeds 10,000 random malformed rings and asserts no panics.
```

## Step 21: virtio-blk device

**Build prompt**
```text
Implement virtio-blk on the virtqueue engine: a raw image file backend (pread/pwrite,
sparse-aware), VIRTIO_BLK_T_IN/OUT/FLUSH/GET_ID, config space capacity, VIRTIO_BLK_F_FLUSH,
read-only mode, proper status byte writeback, bounds checks against capacity, and interrupt
injection via the GIC SPI. Run I/O on a dedicated thread per device with a channel from the
notify path. Log each request at TRACE (type, sector, len) and errors at ERROR. Add per-device
stats (reqs, bytes, errors).
```

**Test prompt**
```text
Unit tests with a temp image: read/write round trips, out-of-range sector, read-only write
rejected, flush. Then create a 512 MiB raw image with `truncate`, boot Linux, and inside the
guest run `dd if=/dev/urandom of=/dev/vda bs=1M count=16 && sync`, then verify on the host that
the image bytes are non-zero and the stats match. Show the logs.
```

## Step 22: Boot from a virtio-blk rootfs

**Build prompt**
```text
Add a config option to boot with root=/dev/vda. Provide scripts/make-rootfs.sh (Alpine or
Debian arm64 minirootfs into an ext4 raw image using tools available on macOS, or via a
one-time Docker/QEMU-built image, documented). Ensure the kernel has virtio-blk and ext4 built
in (document the kernel config). Add a boot scenario to the harness that boots from the disk,
writes a file, powers off, and confirms persistence on the next boot.
```

**Test prompt**
```text
Run the harness scenario end to end: boot, `echo persist > /root/t`, poweroff, boot again,
`cat /root/t` must print "persist". Run `fsck.ext4 -n` on the image from a helper or in the
guest and confirm it's clean. Report timings.
```

## Step 23: virtio-net with pluggable backends

**Build prompt**
```text
Implement virtio-net (VIRTIO_NET_F_MAC, STATUS, MRG_RXBUF off initially): RX and TX queues,
virtio_net_hdr handling, a `NetBackend` trait, a LoopbackBackend that answers ARP and ICMP echo
for a fake gateway (so networking is testable without privileges), and a stub VmnetBackend
behind a feature flag using vmnet.framework shared mode (note in docs that it may need root or
a restricted entitlement). Add TRACE packet summaries (ethertype, sizes) and a pcap writer
option (TERNVALE_PCAP=path) for Wireshark.
```

**Test prompt**
```text
Unit test TX -> loopback -> RX for an ARP request and an ICMP echo. Boot Linux, configure
`ip addr add 10.0.2.15/24 dev eth0`, run `ping -c 3 10.0.2.2` and assert 0% loss. Open the
captured pcap and confirm the frames with tshark/Wireshark filters. Show results.
```

## Step 24: virtio-rng and virtio-vsock

**Build prompt**
```text
Implement virtio-rng (fill from getentropy) and a virtio-vsock device (guest CID assignment,
connection tracking, stream sockets, credit-based flow control) exposing host-side connections
through a Unix domain socket per port, so the future guest agent can connect over vsock. Keep
the first version small: guest-initiated and host-initiated connections, RST/shutdown, and
credit updates. Log every vsock packet header at TRACE.
```

**Test prompt**
```text
Boot Linux with both. In the guest check that `cat /dev/hwrng | head -c 16 | xxd` produces
random data. For vsock, run a tiny guest test program (or socat with VSOCK) that connects to the
host on port 5000, exchange "ping"/"pong", and assert the host side receives it. Show logs.
```

## Step 25: Multi-vCPU (SMP)

**Build prompt**
```text
Make the VMM SMP-capable: one host thread per vCPU (each creates and runs its own vCPU),
secondary vCPUs start powered off and are started by PSCI CPU_ON with entry point and context
id, per-vCPU GIC redistributor setup, per-vCPU logging spans, and a coordinated shutdown
(hv_vcpus_exit on all). Protect shared structures (bus, devices) with appropriate locking;
document the lock order in docs/ARCHITECTURE.md to avoid deadlocks. Update the DTB /cpus nodes
with correct MPIDR values.
```

**Test prompt**
```text
Boot with cpus=4; in the guest verify `nproc` = 4 and
`cat /proc/cpuinfo | grep -c processor` = 4. Run a parallel workload
(e.g., `for i in 1 2 3 4; do (dd if=/dev/zero of=/dev/null bs=1M count=2000 &); done`) and
confirm all CPUs show activity in `top`. Run the boot harness 20 times to detect races, and add
a deadlock detector (log lock waits > 2s at WARN).
```

---

# Phase E: Platform features and product basics (Steps 26-30)

## Step 26: PCIe host bridge (ECAM)

**Build prompt**
```text
Implement a PCIe generic ECAM host bridge: config-space reads/writes routed via the MMIO bus, a
device model for a host bridge at 00:00.0, the BAR sizing/assignment protocol, an INTx/MSI-X
plan (stub with logs, document the design), and a DTB node "pci-host-ecam-generic" with ranges.
Port virtio-blk to virtio-pci (modern, with capabilities: common, notify, isr, device cfg) so a
PCI transport exists, since Windows and UEFI will need PCI. Keep virtio-mmio working.
```

**Test prompt**
```text
Boot Linux and confirm `lspci -nn` shows the host bridge and virtio-blk (vendor 1af4). Mount a
disk that is attached over PCI. Unit test config-space BAR sizing (write 0xFFFFFFFF, read back
the mask) and capability list walking. Log all config accesses at TRACE and show a sample.
```

## Step 27: UEFI (EDK2) boot

**Build prompt**
```text
Add firmware support: load an EDK2 AArch64 image (from config.firmware) into a firmware region
at the address EDK2 expects (verify against QEMU's virt firmware layout), add a variable-store
region (persisted to a per-VM NVRAM file), and boot into UEFI instead of directly into the
kernel. If EDK2 requires fw_cfg or specific devices, implement the minimum and document each
requirement (TODO(verify) where uncertain). Goal: reach the UEFI shell and see the boot menu on
the UART.
```

**Test prompt**
```text
Boot the firmware and assert the serial log contains the UEFI banner. Attach a Debian/Alpine
arm64 ISO as a virtio disk and try booting the installer from UEFI. Where it fails, compare to
`qemu-system-aarch64 -M virt -accel hvf -bios <same firmware>` and list the missing devices or
behavior in docs/DEBUGGING.md.
```

## Step 28: VM lifecycle CLI and control socket

**Build prompt**
```text
Build ternvale-cli with clap: `ternvale run <config>`, `ternvale validate <config>`,
`ternvale create-disk <path> <size>`, `ternvale status <name>`, and
`ternvale pause|resume|stop <name>`. The running VM exposes a Unix control socket (JSON lines:
status, pause, resume, shutdown, force-stop, query-stats) at
~/Library/Application Support/Ternvale/run/<name>.sock. Implement vCPU pause/resume via
hv_vcpus_exit plus a state machine (Created, Running, Paused, Stopping, Stopped, Failed) with
every transition logged. Use anyhow only here, with context on every error.
```

**Test prompt**
```text
Integration test: start a VM in the background; `ternvale status` shows Running; `ternvale
pause` freezes the guest (a guest `while true; do date; sleep 1; done` stops printing);
`ternvale resume` continues; `ternvale stop` exits cleanly. Invalid transitions (pause a
stopped VM) must return clear errors. Show the state-transition logs.
```

## Step 29: Guest agent protocol skeleton

**Build prompt**
```text
Create a new crate ternvale-agent-proto (shared types: length-prefixed JSON or protobuf
messages: Hello{version,os}, Ping/Pong, SetResolution{w,h}, ClipboardSet{text}, Shutdown) and a
guest-side `ternvale-agent` binary (static aarch64-linux-musl) that connects to the host over
vsock port 5000, sends Hello, and answers Ping. On the host, add an AgentServer that accepts the
connection on the vsock Unix socket from Step 24, tracks agent connection state, and exposes it
in `ternvale status`. Version the protocol and log every message at DEBUG. Implement reconnect
with backoff.
```

**Test prompt**
```text
Unit tests for message encode/decode including truncated and oversized frames. Integration:
copy the agent into the rootfs, boot, and confirm `ternvale status` shows "agent: connected
(v1)". Kill the agent in the guest and confirm the host logs the disconnect, then restart it
and confirm reconnect.
```

## Step 30: Hardening, log tooling and release checklist

**Build prompt**
```text
Finalize the basic version: (1) add `ternvale doctor` that checks macOS version, hypervisor
availability, entitlement present, GIC support, disk space and log directory, printing pass/fail
with fixes; (2) add `ternvale logs <name> [--follow] [--level] [--target]` to view and filter
log files; (3) add a crash-report bundle command that zips logs, config, DTB dump and the last
MMIO events; (4) print an exit-reason and device stats summary at VM shutdown; (5) write
docs/DEBUGGING.md with a triage flow ("guest hangs at boot", "no console output", "virtio
device not found", "PSCI CPU_ON fails") and docs/ROADMAP.md (next: framebuffer/GPU, Windows
ACPI + TPM, snapshots, SwiftUI app, signed drivers). Run cargo audit and cargo deny.
```

**Test prompt**
```text
Run the complete regression: `make lint test test-hv` plus the boot harness (direct-kernel,
virtio-blk root, net ping, SMP=4, vsock agent, UEFI banner). Produce a table of pass/fail and
timings in docs/PROGRESS.md. Deliberately break something (bad kernel path, corrupt disk, wrong
cmdline) and verify `ternvale doctor`, the logs and the crash bundle make the cause obvious
within one minute.
```

---

## Debugging playbook

When something hangs or misbehaves, work through this order:

1. **Turn up logging:** `TERNVALE_LOG=ternvale::mmio=trace,ternvale::hv=trace,info ternvale run vm.toml`
2. **Read the last MMIO accesses.** A guest that stops right after touching an address you have no device for is the most common cause of a silent hang.
3. **Check the watchdog output** (last PC and last 20 MMIO events).
4. **Decode the last exit syndrome** (ESR). An unknown EC is logged at ERROR with the raw value.
5. **Run the same guest in QEMU** (`qemu-system-aarch64 -M virt -accel hvf ...`) and compare which devices and registers it touches.
6. **Dump and inspect the DTB** with `TERNVALE_DUMP_DTB=1`, then read it with `dtc`.
7. **Bisect.** Disable devices one at a time (network, disk, extra CPUs) until the guest boots again.

Log files live in `~/Library/Logs/Ternvale/`. Guest serial output is written to its own file next to them.

---

## What comes after Step 30

The basic version boots Linux with disk, network, vsock, SMP, PCI and UEFI. These are the next phases, and each deserves its own 30-step guide:

- **Windows 11 ARM:** ACPI table generation (DSDT, FADT, MADT, GTDT, MCFG, SPCR), a virtual TPM 2.0, and virtio-win drivers.
- **Display:** a framebuffer first, then a paravirtual GPU and a Metal-backed rendering path.
- **Snapshots and clones:** your own layered disk format plus memory/device state save and restore.
- **Input and audio:** virtio-input, USB xHCI, and CoreAudio output.
- **Native app:** a SwiftUI front end talking to the VMM over the control socket, with the VMM in a sandboxed helper process.
- **Guest tools:** clipboard, shared folders (virtiofs), dynamic resolution, and window integration.
- **Distribution:** Developer ID signing, notarization, and the Apple entitlements request for bridged networking.
