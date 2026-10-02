use std::path::PathBuf;

use ternvale_devices::{AgentState, AgentStatus, VsockCounters};
use ternvale_vmm::{
    CpuStats, DeviceCount, ExitReason, StopCause, VcpuStats, VmState, VmStats, VmStatus,
};

use super::{describe, exit_line, render, DiskSummary, NicSummary, VmSummary};

fn summary(exit: Option<super::Exit>, cause: Option<StopCause>) -> VmSummary {
    VmSummary {
        exit,
        stats: VmStats {
            status: VmStatus {
                name: "demo".into(),
                state: VmState::Stopped,
                cpus: 2,
                uptime_ms: 12_345,
                state_ms: 10,
                paused_ms: 250,
                pauses: 1,
                stop_cause: cause,
                failure: None,
            },
            cpus: vec![
                CpuStats {
                    cpu: 0,
                    in_guest: false,
                    stats: Some(VcpuStats {
                        runs: 900,
                        guest_ms: 11_000,
                        wfi_parks: 40,
                        park_ms: 500,
                        vtimer_exits: 300,
                    }),
                },
                CpuStats {
                    cpu: 1,
                    in_guest: false,
                    stats: None,
                },
            ],
            process_cpu_ms: Some(2_500),
        },
        mmio_total: 1_234,
        mmio_unmapped: 2,
        mmio: vec![
            DeviceCount {
                name: "pl011".into(),
                base: 0x900_0000,
                size: 0x1000,
                accesses: 1_000,
            },
            DeviceCount {
                name: "virtio-mmio".into(),
                base: 0xa00_0000,
                size: 0x200,
                accesses: 232,
            },
            DeviceCount {
                name: "pl031".into(),
                base: 0x901_0000,
                size: 0x1000,
                accesses: 0,
            },
        ],
        disks: vec![DiskSummary {
            path: PathBuf::from("/vm/root.img"),
            reqs: 120,
            bytes: 3 << 20,
            errors: 1,
        }],
        nics: vec![NicSummary {
            backend: "loopback".into(),
            counters: [10, 1_500, 0, 8, 900, 1],
        }],
        vsock: Some(VsockCounters {
            tx_packets: 40,
            tx_bytes: 2_048,
            rx_packets: 38,
            rx_bytes: 1_000,
            connections: 1,
            resets: 0,
            dropped: 0,
        }),
        agent: Some(AgentStatus {
            state: AgentState::Connected,
            version: Some(1),
            os: None,
            agent: None,
            connected_ms: Some(1),
            last_pong_ms: None,
            rtt_us: None,
            connects: 2,
            disconnects: 1,
            pings: 12,
            pongs: 11,
            last_error: Some("eof".into()),
        }),
    }
}

#[test]
fn exit_line_prefers_the_host_cause_and_names_failures() {
    let off = summary(Some(Ok(ExitReason::SystemOff)), None);
    assert_eq!(
        exit_line(&off),
        "vm demo stopped: guest powered off (PSCI SYSTEM_OFF)"
    );
    let stopped = summary(Some(Ok(ExitReason::Canceled)), Some(StopCause::Shutdown));
    assert_eq!(
        exit_line(&stopped),
        "vm demo stopped: host shutdown request (ternvale stop)"
    );
    let forced = summary(Some(Ok(ExitReason::Canceled)), Some(StopCause::ForceStop));
    assert!(exit_line(&forced).ends_with("host force-stop (ternvale stop --force)"));
    let guest = summary(
        Some(Ok(ExitReason::Canceled)),
        Some(StopCause::Guest(ExitReason::SystemReset)),
    );
    assert!(exit_line(&guest).contains("PSCI SYSTEM_RESET"));
    let failed = summary(Some(Err("vcpu 0: HV_ERROR".into())), None);
    assert_eq!(exit_line(&failed), "vm demo failed: vcpu 0: HV_ERROR");
    let live = summary(None, None);
    assert_eq!(
        exit_line(&live),
        "vm demo is stopped (snapshot, not stopped)"
    );
}

#[test]
fn exceptions_show_esr_class_and_addresses() {
    let text = describe(&ExitReason::Exception {
        syndrome: 0x9200_0046,
        virtual_address: 0xffff_0000,
        physical_address: 0x4000,
    });
    assert_eq!(
        text,
        "unhandled guest exception: ESR 0x92000046 (EC 0x24), VA 0xffff0000, IPA 0x4000"
    );
    assert_eq!(
        describe(&ExitReason::Unknown { reason: 9 }),
        "unknown hypervisor exit reason 9"
    );
}

#[test]
fn render_lists_cpus_busy_devices_and_device_counters() {
    let text = render(&summary(Some(Ok(ExitReason::SystemOff)), None));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[1],
        "  uptime 12.345 s, paused 250 ms (1 pauses), host CPU 2.500 s"
    );
    assert_eq!(
        lines[2],
        "  vcpu 0: 900 runs, 11.000 s in guest, 40 WFI parks (500 ms), 300 vtimer exits"
    );
    assert_eq!(lines[3], "  vcpu 1: never started");
    assert_eq!(
        lines[4],
        "  mmio: 1234 accesses (2 unmapped): pl011@0x9000000 1000, virtio-mmio@0xa000000 232"
    );
    assert_eq!(
        lines[5],
        "  disk 0 /vm/root.img: 120 requests, 3.0 MiB, 1 errors"
    );
    assert_eq!(
        lines[6],
        "  nic 0 (loopback): tx 10 packets / 1.5 KiB (0 dropped), rx 8 packets / 900 B (1 dropped)"
    );
    assert!(
        lines[7].starts_with("  vsock: tx 40 packets / 2.0 KiB"),
        "{}",
        lines[7]
    );
    assert_eq!(
        lines[8],
        "  agent: connected (v1), 2 connects, 1 disconnects, 11/12 pings answered, last error: eof"
    );
    assert_eq!(lines.len(), 9);
}

#[test]
fn render_skips_absent_devices() {
    let mut bare = summary(Some(Ok(ExitReason::SystemOff)), None);
    bare.disks.clear();
    bare.nics.clear();
    bare.vsock = None;
    bare.agent = None;
    bare.mmio.clear();
    bare.stats.process_cpu_ms = None;
    let text = render(&bare);
    assert!(
        text.ends_with("\n  mmio: 1234 accesses (2 unmapped)"),
        "{text}"
    );
    assert!(!text.contains("host CPU"));
    assert!(!text.contains("disk") && !text.contains("nic") && !text.contains("agent"));
}
