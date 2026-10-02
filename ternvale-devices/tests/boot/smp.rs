//! SMP scenario: boot the busybox initrd on several CPUs (default 4), check
//! `nproc` and `/proc/cpuinfo`, require an idle guest to cost the host little,
//! run one `dd` per CPU, and require every CPU to be busy in `/proc/stat` and
//! to run a `dd` in `top`.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_devices::{last_lines, Pl011, Step};
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{
    assets_root, banner_timeout, boot_cpus, drive, init_logging, log_dir, restore_stdin,
    stdin_pipe, write_result,
};
use crate::idle;

const COMMAND: Duration = Duration::from_secs(30);
/// Megabytes each `dd` copies in the literal workload (`count=2000`).
const DD_COUNT: u32 = 2000;
/// Megabytes for the sustained workload: minutes at ~23 GB/s, ended by `killall`.
const SUSTAINED_COUNT: u32 = 4_000_000;
/// Minimum share of a CPU's time spent busy while the `dd`s run.
const MIN_BUSY_PCT: u64 = 50;
/// The PL011 RX FIFO is 16 bytes.
const CHUNK: usize = 16;

pub fn run() -> Result<(), String> {
    let log_dir = log_dir();
    let serial_log = log_dir.join("guest-serial.log");
    let guard = init_logging("boot-smp", &log_dir)?;
    tracing::info!(
        target: "ternvale::boot",
        dir = %log_dir.display(),
        host_log = %guard.log_path().display(),
        scenario = "smp",
        "boot harness logs"
    );
    let cpus = boot_cpus(4)?;
    if cpus < 2 {
        return Err(format!(
            "the smp scenario needs at least 2 cpus, got {cpus}"
        ));
    }
    let root = assets_root();
    let vm = VmConfig {
        name: "boot-smp".to_string(),
        cpus,
        ram_mib: 256,
        kernel: root.join("Image"),
        initrd: Some(root.join("initramfs.cpio")),
        cmdline: String::new(),
        boot_disk: false,
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: serial_log.clone(),
        firmware: None,
    };
    let uart = Pl011::open(&serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let serial_for_feed = serial_log.clone();
    let host_log = guard.log_path().to_path_buf();
    let feeder = std::thread::spawn(move || {
        drive(&serial_for_feed, &mut input, &flag, &host_log, script(cpus))
    });
    let idle_watch = idle::watch(serial_log.clone(), Arc::clone(&cancel));

    let started = Instant::now();
    let exit = Machine::run_until(&vm, Box::new(uart), Arc::clone(&cancel));
    cancel.store(true, Ordering::Release);
    let script_result = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    let idle_cost = idle_watch.join().unwrap_or(None);
    restore_stdin(saved);

    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(&log_dir, &exit, &script_result, &serial);
    let checked = script_result
        .and_then(|()| idle::check(idle_cost))
        .and_then(|()| check_activity(&serial, cpus));
    let host = guard.log_path().display().to_string();
    match (checked, exit) {
        (Ok(()), Ok(ExitReason::SystemOff)) => {
            tracing::info!(
                target: "ternvale::boot",
                cpus,
                total_ms = started.elapsed().as_millis() as u64,
                scenario = "smp",
                "boot harness passed"
            );
            drop(guard);
            Ok(())
        }
        (checked, exit) => Err(format!(
            "check={checked:?} exit={exit:?}\nlog={host}\n--- serial ---\n{}",
            last_lines(&serial, 40)
        )),
    }
}

/// `command` typed in FIFO-sized pieces. Markers are written as `A""B` so the
/// echoed command line never matches the output pattern `AB`.
fn typed(command: &str) -> Vec<Step> {
    command
        .as_bytes()
        .chunks(CHUNK)
        .map(|chunk| Step::Send {
            data: chunk.to_vec(),
        })
        .collect()
}

fn expect(pattern: &str, timeout: Duration) -> Step {
    Step::Expect {
        pattern: pattern.to_string(),
        timeout,
    }
}

fn script(cpus: u32) -> Vec<Step> {
    let banner = banner_timeout();
    let list: Vec<String> = (1..=cpus).map(|i| i.to_string()).collect();
    let mut steps = vec![
        expect("Linux version", banner),
        expect(
            &format!("SMP: Total of {cpus} processors activated"),
            banner,
        ),
        expect("# ", banner),
    ];
    let dd_loop = |count: u32| {
        format!(
            "for i in {}; do (dd if=/dev/zero of=/dev/null bs=1M count={count} &); done",
            list.join(" ")
        )
    };
    // This initramfs has only /bin/busybox and mounts neither /proc nor /dev.
    let commands: [(String, String); 10] = [
        (
            "/bin/busybox --install -s /bin; mkdir -p /proc /dev; mount -t proc proc /proc; mount -t devtmpfs dev /dev; echo SETUP\"\"_OK\n".into(),
            "SETUP_OK".into(),
        ),
        // Every CPU idle; `idle::watch` measures the host CPU in between.
        (
            "echo IDLE\"\"_A; sleep 3; echo IDLE\"\"_B\n".into(),
            idle::END.into(),
        ),
        ("echo nproc=$(nproc)\n".into(), format!("nproc={cpus}")),
        (
            "echo procs=$(cat /proc/cpuinfo | grep -c processor)\n".into(),
            format!("procs={cpus}"),
        ),
        // The literal workload finishes in well under a second.
        (
            format!("{}; sleep 2; echo LITERAL\"\"_END\n", dd_loop(DD_COUNT)),
            "LITERAL_END".into(),
        ),
        // The sustained one runs until `killall`, so top and /proc/stat see it.
        (
            format!("{}; echo STARTED\"\"_DD\n", dd_loop(SUSTAINED_COUNT)),
            "STARTED_DD".into(),
        ),
        (
            "sleep 1; echo STAT\"\"_A; grep ^cpu /proc/stat; sleep 2; echo STAT\"\"_B; grep ^cpu /proc/stat; echo STAT\"\"_END\n".into(),
            "STAT_END".into(),
        ),
        (
            "top -b -n 1 | head -n 30; echo TOP\"\"_END\n".into(),
            "TOP_END".into(),
        ),
        (
            "mpstat -P ALL 1 1; echo MPSTAT\"\"_END\n".into(),
            "MPSTAT_END".into(),
        ),
        (
            "killall dd; echo KILL\"\"_END\n".into(),
            "KILL_END".into(),
        ),
    ];
    // Wait for the prompt too: text typed before busybox prints its cursor
    // query (ESC[6n) is swallowed while it waits for the reply.
    for (command, pattern) in &commands {
        steps.extend(typed(command));
        steps.push(expect(pattern, COMMAND));
        steps.push(expect("# ", COMMAND));
    }
    steps.extend(typed("poweroff -f\n"));
    steps
}

/// Lines strictly between the line `start` and the line `end`.
fn section<'a>(serial: &'a str, start: &str, end: &str) -> Vec<&'a str> {
    let mut lines = serial.lines().map(|line| line.trim_end_matches('\r'));
    if lines.by_ref().all(|line| line.trim() != start) {
        return Vec::new();
    }
    lines.take_while(|line| line.trim() != end).collect()
}

/// `cpuN` -> (busy, total) jiffies from `grep ^cpu /proc/stat` output.
fn cpu_times(lines: &[&str]) -> Vec<(String, u64, u64)> {
    lines
        .iter()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            if !name.starts_with("cpu") || name == "cpu" {
                return None;
            }
            // user nice system idle iowait irq softirq steal
            let values: Vec<u64> = fields.take(8).filter_map(|v| v.parse().ok()).collect();
            if values.len() < 8 {
                return None;
            }
            let total: u64 = values.iter().sum();
            Some((name.to_string(), total - values[3] - values[4], total))
        })
        .collect()
}

/// Every CPU busy at least [`MIN_BUSY_PCT`] between the two `/proc/stat`
/// samples, and every CPU running a `dd` in `top`.
fn check_activity(serial: &str, cpus: u32) -> Result<(), String> {
    let literal_done = serial.matches(&format!("{DD_COUNT}+0 records out")).count();
    tracing::info!(target: "ternvale::boot", literal_done, cpus, "literal dd runs that finished");
    if literal_done != cpus as usize {
        return Err(format!("{literal_done} of {cpus} literal dd runs finished"));
    }
    if serial.contains("no process killed") {
        return Err("the sustained dd workload ended before killall".to_string());
    }
    let before = cpu_times(&section(serial, "STAT_A", "STAT_B"));
    let after = cpu_times(&section(serial, "STAT_B", "STAT_END"));
    let mut busy = Vec::new();
    for cpu in 0..cpus {
        let name = format!("cpu{cpu}");
        let find = |set: &[(String, u64, u64)]| {
            set.iter()
                .find(|(n, _, _)| *n == name)
                .map(|&(_, busy, total)| (busy, total))
        };
        let (Some((busy_a, total_a)), Some((busy_b, total_b))) = (find(&before), find(&after))
        else {
            return Err(format!("{name} missing from /proc/stat samples"));
        };
        let total = total_b.saturating_sub(total_a).max(1);
        busy.push((cpu, busy_b.saturating_sub(busy_a) * 100 / total));
    }
    tracing::info!(target: "ternvale::boot", per_cpu_busy_pct = ?busy, "guest /proc/stat busy share during dd");

    let top = section(serial, "STAT_END", "TOP_END");
    let header = top
        .iter()
        .rposition(|line| line.contains("%CPU COMMAND"))
        .ok_or("top printed no process header")?;
    let columns: Vec<&str> = top[header].split_whitespace().collect();
    let column = columns
        .iter()
        .position(|field| *field == "CPU")
        .ok_or("top header has no CPU column")?;
    // COMMAND is last and holds the whole command line, e.g. `dd if /dev/zero ...`.
    let command = columns.len() - 1;
    let mut dd_cpus = BTreeSet::new();
    for line in &top[header + 1..] {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.get(command) == Some(&"dd") {
            tracing::info!(target: "ternvale::boot", line = %line.trim(), "top dd process");
            if let Some(cpu) = fields.get(column).and_then(|v| v.parse::<u32>().ok()) {
                dd_cpus.insert(cpu);
            }
        }
    }
    tracing::info!(target: "ternvale::boot", dd_cpus = ?dd_cpus, "cpus running dd in top");

    if let Some((cpu, pct)) = busy.iter().find(|(_, pct)| *pct < MIN_BUSY_PCT) {
        return Err(format!(
            "cpu{cpu} was only {pct}% busy during the dd workload ({busy:?})"
        ));
    }
    let all: BTreeSet<u32> = (0..cpus).collect();
    if dd_cpus != all {
        return Err(format!(
            "top shows dd on cpus {dd_cpus:?}, expected {all:?}"
        ));
    }
    Ok(())
}
