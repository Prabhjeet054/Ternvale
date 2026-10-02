//! The exit-reason and device-stats summary `ternvale run` prints when a VM
//! stops, also written to `<log stem>.summary.txt` for crash reports.

use std::path::PathBuf;

use ternvale_devices::{AgentStatus, VsockCounters};
use ternvale_vmm::{DeviceCount, ExitReason, StopCause, VmStats};

use crate::doctor::human as bytes;

/// How the run ended. `None` in [`VmSummary::exit`] means it has not yet.
pub type Exit = Result<ExitReason, String>;

/// One virtio-blk disk's counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskSummary {
    pub path: PathBuf,
    pub reqs: u64,
    pub bytes: u64,
    pub errors: u64,
}

/// One virtio-net NIC's counters, as [`ternvale_devices::NetStats::snapshot`]:
/// tx packets, tx bytes, tx dropped, rx packets, rx bytes, rx dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NicSummary {
    pub backend: String,
    pub counters: [u64; 6],
}

/// Everything the summary reports.
#[derive(Debug, Clone)]
pub struct VmSummary {
    pub exit: Option<Exit>,
    pub stats: VmStats,
    pub mmio_total: u64,
    pub mmio_unmapped: u64,
    pub mmio: Vec<DeviceCount>,
    pub disks: Vec<DiskSummary>,
    pub nics: Vec<NicSummary>,
    pub vsock: Option<VsockCounters>,
    pub agent: Option<AgentStatus>,
}

/// One line saying why the VM stopped, in words.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn exit_line(summary: &VmSummary) -> String {
    let name = &summary.stats.status.name;
    let host = match summary.stats.status.stop_cause {
        Some(StopCause::Shutdown) => Some("host shutdown request (ternvale stop)".to_string()),
        Some(StopCause::ForceStop) => Some("host force-stop (ternvale stop --force)".to_string()),
        Some(StopCause::Guest(reason)) => Some(describe(&reason)),
        None => None,
    };
    match (&summary.exit, host) {
        (None, _) => format!(
            "vm {name} is {} (snapshot, not stopped)",
            summary.stats.status.state
        ),
        (Some(Err(error)), _) => format!("vm {name} failed: {error}"),
        (Some(Ok(_)), Some(cause)) => format!("vm {name} stopped: {cause}"),
        (Some(Ok(reason)), None) => format!("vm {name} stopped: {}", describe(reason)),
    }
}

/// An [`ExitReason`] in words.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn describe(reason: &ExitReason) -> String {
    match reason {
        ExitReason::SystemOff => "guest powered off (PSCI SYSTEM_OFF)".to_string(),
        ExitReason::SystemReset => {
            "guest asked for a reset (PSCI SYSTEM_RESET); ternvale stops instead of rebooting"
                .to_string()
        }
        ExitReason::Exception {
            syndrome,
            virtual_address,
            physical_address,
        } => format!(
            "unhandled guest exception: ESR {syndrome:#x} (EC {:#04x}), VA {virtual_address:#x}, IPA {physical_address:#x}",
            (syndrome >> 26) & 0x3f
        ),
        ExitReason::Canceled => "vCPUs stopped by the host (run canceled)".to_string(),
        ExitReason::CpuOff => "last CPU turned itself off (PSCI CPU_OFF)".to_string(),
        ExitReason::Unknown { reason } => format!("unknown hypervisor exit reason {reason}"),
        other => format!("{other:?}"),
    }
}

/// The full multi-line summary.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn render(summary: &VmSummary) -> String {
    let status = &summary.stats.status;
    let mut out = vec![exit_line(summary)];
    let mut times = format!(
        "  uptime {}, paused {} ({} pauses)",
        ms(status.uptime_ms),
        ms(status.paused_ms),
        status.pauses
    );
    if let Some(cpu) = summary.stats.process_cpu_ms {
        times.push_str(&format!(", host CPU {}", ms(cpu)));
    }
    out.push(times);
    for cpu in &summary.stats.cpus {
        out.push(match cpu.stats {
            Some(s) => format!(
                "  vcpu {}: {} runs, {} in guest, {} WFI parks ({}), {} vtimer exits",
                cpu.cpu,
                s.runs,
                ms(s.guest_ms),
                s.wfi_parks,
                ms(s.park_ms),
                s.vtimer_exits
            ),
            None => format!("  vcpu {}: never started", cpu.cpu),
        });
    }
    let mut busy: Vec<&DeviceCount> = summary.mmio.iter().filter(|d| d.accesses > 0).collect();
    busy.sort_by(|a, b| b.accesses.cmp(&a.accesses).then(a.base.cmp(&b.base)));
    let devices: Vec<String> = busy
        .iter()
        .map(|d| format!("{}@{:#x} {}", d.name, d.base, d.accesses))
        .collect();
    out.push(format!(
        "  mmio: {} accesses ({} unmapped){}{}",
        summary.mmio_total,
        summary.mmio_unmapped,
        if devices.is_empty() { "" } else { ": " },
        devices.join(", ")
    ));
    for (index, disk) in summary.disks.iter().enumerate() {
        out.push(format!(
            "  disk {index} {}: {} requests, {}, {} errors",
            disk.path.display(),
            disk.reqs,
            bytes(disk.bytes),
            disk.errors
        ));
    }
    for (index, nic) in summary.nics.iter().enumerate() {
        let [txp, txb, txd, rxp, rxb, rxd] = nic.counters;
        out.push(format!(
            "  nic {index} ({}): tx {txp} packets / {} ({txd} dropped), rx {rxp} packets / {} ({rxd} dropped)",
            nic.backend,
            bytes(txb),
            bytes(rxb)
        ));
    }
    if let Some(v) = &summary.vsock {
        out.push(format!(
            "  vsock: tx {} packets / {}, rx {} packets / {}, {} connections, {} resets, {} dropped",
            v.tx_packets,
            bytes(v.tx_bytes),
            v.rx_packets,
            bytes(v.rx_bytes),
            v.connections,
            v.resets,
            v.dropped
        ));
    }
    if let Some(a) = &summary.agent {
        let mut line = format!("  agent: {}", a.state.as_str());
        if let Some(version) = a.version {
            line.push_str(&format!(" (v{version})"));
        }
        line.push_str(&format!(
            ", {} connects, {} disconnects, {}/{} pings answered",
            a.connects, a.disconnects, a.pongs, a.pings
        ));
        if let Some(error) = &a.last_error {
            line.push_str(&format!(", last error: {error}"));
        }
        out.push(line);
    }
    out.join("\n")
}

/// Log the summary's numbers as one structured INFO event.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn log(summary: &VmSummary) {
    let status = &summary.stats.status;
    let runs: u64 = summary
        .stats
        .cpus
        .iter()
        .filter_map(|c| c.stats.map(|s| s.runs))
        .sum();
    tracing::info!(
        target: "ternvale::cli",
        vm = %status.name,
        exit = %exit_line(summary),
        uptime_ms = status.uptime_ms,
        paused_ms = status.paused_ms,
        process_cpu_ms = ?summary.stats.process_cpu_ms,
        vcpu_runs = runs,
        mmio_total = summary.mmio_total,
        mmio_unmapped = summary.mmio_unmapped,
        disk_reqs = summary.disks.iter().map(|d| d.reqs).sum::<u64>(),
        disk_errors = summary.disks.iter().map(|d| d.errors).sum::<u64>(),
        net_tx_packets = summary.nics.iter().map(|n| n.counters[0]).sum::<u64>(),
        net_rx_packets = summary.nics.iter().map(|n| n.counters[3]).sum::<u64>(),
        vsock_connections = ?summary.vsock.as_ref().map(|v| v.connections),
        agent = ?summary.agent.as_ref().map(|a| a.state.as_str()),
        "vm summary"
    );
}

fn ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms} ms")
    } else {
        format!("{}.{:03} s", ms / 1_000, ms % 1_000)
    }
}

#[cfg(test)]
#[path = "summary_tests.rs"]
mod tests;
