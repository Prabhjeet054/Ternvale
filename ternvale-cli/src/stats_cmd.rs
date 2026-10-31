//! `ternvale stats <name> [--watch] [--json]`.

use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use ternvale_metrics::Snapshot;

use crate::client;
use crate::commands;
use crate::paths;
use crate::protocol::{Request, Response};

/// `ternvale stats <name>`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(vm = name, watch, interval_ms, json)
)]
pub fn stats(name: &str, watch: bool, interval_ms: u64, json: bool) -> Result<ExitCode> {
    let socket = paths::socket_path(name)?;
    let poll = Duration::from_millis(interval_ms.max(1));
    loop {
        let stream = match client::connect(&socket)? {
            Ok(stream) => stream,
            Err(reach) => {
                if watch {
                    tracing::info!(target: "ternvale::cli", vm = name, "watch: vm is gone");
                }
                println!("vm {name}: not running{}", stale(&reach, &socket));
                return Ok(ExitCode::FAILURE);
            }
        };
        let response = client::exchange(stream, &Request::QueryStats)?;
        if json {
            println!(
                "{}",
                serde_json::to_string(&response).context("encode stats response")?
            );
        } else {
            print_human(name, &response)?;
        }
        if !watch {
            return Ok(if response.ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
        thread::sleep(poll);
    }
}

fn stale(reach: &client::Reach, socket: &std::path::Path) -> String {
    match reach {
        client::Reach::NotRunning => String::new(),
        client::Reach::Stale => format!(" (stale socket {})", socket.display()),
    }
}

fn print_human(name: &str, response: &Response) -> Result<()> {
    let status = response
        .status
        .as_ref()
        .with_context(|| format!("vm {name} answered without a status"))?;
    print!("{}", commands::render(status, response.stats.as_ref()));
    if let Some(metrics) = response.stats.as_ref().and_then(|s| s.metrics.as_ref()) {
        print!("{}", render_metrics(metrics));
    }
    Ok(())
}

/// Human-readable lock-free metrics block.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn render_metrics(metrics: &Snapshot) -> String {
    let mut lines = vec![format!("  interrupts {}", metrics.interrupts)];
    for cpu in &metrics.vcpus {
        let kinds: Vec<String> = cpu
            .exits
            .iter()
            .filter(|e| e.count > 0)
            .map(|e| format!("{} {}", e.kind, e.count))
            .collect();
        lines.push(format!(
            "  metrics vcpu {}: guest {} ns, handle {} ns{}",
            cpu.cpu,
            cpu.guest_ns,
            cpu.handle_ns,
            if kinds.is_empty() {
                String::new()
            } else {
                format!(", {}", kinds.join(", "))
            }
        ));
    }
    for device in &metrics.devices {
        if device.accesses == 0 {
            continue;
        }
        lines.push(format!(
            "  mmio {}: {} accesses, p50 {} ns, p99 {} ns, max {} ns",
            device.name,
            device.accesses,
            device.latency.p50_ns,
            device.latency.p99_ns,
            device.latency.max_ns
        ));
    }
    lines.push(io_line("blk", &metrics.blk));
    lines.push(io_line("net", &metrics.net));
    lines.push(String::new());
    lines.join("\n")
}

fn io_line(name: &str, io: &ternvale_metrics::IoSnapshot) -> String {
    format!(
        "  {name}: {} count, {} bytes, latency p50 {} ns p99 {} ns max {} ns ({} samples)",
        io.count,
        io.bytes,
        io.latency.p50_ns,
        io.latency.p99_ns,
        io.latency.max_ns,
        io.latency.samples
    )
}

#[cfg(test)]
mod tests {
    use super::render_metrics;
    use ternvale_metrics::{ExitCount, ExitKind, Snapshot, VcpuSnapshot};

    #[test]
    fn render_includes_exit_kind_and_blk() {
        let text = render_metrics(&Snapshot {
            vcpus: vec![VcpuSnapshot {
                cpu: 0,
                guest_ns: 10,
                handle_ns: 2,
                exits: vec![ExitCount::new(ExitKind::Mmio, 4)],
            }],
            interrupts: 3,
            blk: ternvale_metrics::IoSnapshot {
                count: 7,
                bytes: 512,
                ..Default::default()
            },
            ..Default::default()
        });
        assert!(text.contains("interrupts 3"), "{text}");
        assert!(text.contains("mmio 4"), "{text}");
        assert!(text.contains("blk: 7 count"), "{text}");
    }
}
