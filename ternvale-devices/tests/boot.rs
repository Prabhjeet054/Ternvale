//! Boot the test kernel and match serial output. Built with `--features boot-test`.
//!
//! `scripts/boot-test.sh` runs this binary and keeps the logs under `target/boot-logs/`.

use std::io::Write;
use std::os::fd::FromRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_devices::{Pl011, Progress, Session, Step};
use ternvale_vmm::{ExitReason, Machine};

fn main() {
    let code = match boot() {
        Ok(()) => 0,
        Err(error) => {
            tracing::error!(target: "ternvale::boot", error = %error, "boot harness failed");
            1
        }
    };
    std::process::exit(code);
}

fn boot() -> Result<(), String> {
    let log_dir = log_dir();
    std::fs::create_dir_all(&log_dir)
        .map_err(|err| format!("create {}: {err}", log_dir.display()))?;
    let serial_log = log_dir.join("guest-serial.log");
    // SAFETY: this process owns the environment. The harness sets the log filter for its run.
    unsafe { std::env::set_var("TERNVALE_LOG", "info") };
    let mut config = ternvale_log::LogConfig::new("boot", log_dir.clone());
    config.level = "info".to_string();
    let guard = ternvale_log::init(config).map_err(|err| err.to_string())?;
    tracing::info!(
        target: "ternvale::boot",
        dir = %log_dir.display(),
        host_log = %guard.log_path().display(),
        "boot harness logs"
    );

    let started = Instant::now();
    let cmdline = std::env::var("TERNVALE_BOOT_CMDLINE").unwrap_or_default();
    if !cmdline.is_empty() {
        tracing::info!(target: "ternvale::boot", cmdline, "boot harness cmdline override");
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test-assets");
    let vm = VmConfig {
        name: "boot".to_string(),
        cpus: 1,
        ram_mib: 256,
        kernel: root.join("Image"),
        initrd: Some(root.join("initramfs.cpio")),
        cmdline,
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
    let feeder = std::thread::spawn(move || drive(&serial_for_feed, &mut input, &flag, &host_log));

    let exit = Machine::run_until(&vm, Box::new(uart), Arc::clone(&cancel));
    let script = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);

    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    let host = guard.log_path().to_path_buf();
    drop(guard);
    write_result(&log_dir, &exit, &script, &serial);
    script?;
    match exit {
        Ok(ExitReason::SystemOff) => {
            tracing::info!(
                target: "ternvale::boot",
                boot_ms = started.elapsed().as_millis() as u64,
                "boot harness passed"
            );
            Ok(())
        }
        other => Err(format!(
            "exit={other:?}\nlog={}\n--- serial ---\n{}",
            host.display(),
            ternvale_devices::last_lines(&serial, 20)
        )),
    }
}

fn drive(
    serial_log: &std::path::Path,
    input: &mut std::fs::File,
    cancel: &AtomicBool,
    host_log: &std::path::Path,
) -> Result<(), String> {
    let mut session = Session::new(script(), Instant::now());
    let mut cursor_replies = 0usize;
    let mut last_io = Instant::now() - Duration::from_secs(1);
    loop {
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        let text = std::fs::read_to_string(serial_log).unwrap_or_default();
        let queries = text.matches("\u{1b}[6n").count();
        let settled = last_io.elapsed() >= Duration::from_millis(200);
        if cursor_replies < queries && settled {
            input
                .write_all(b"\x1b[24;80R")
                .map_err(|err| format!("cursor reply: {err}"))?;
            cursor_replies += 1;
            last_io = Instant::now();
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if session.waiting_to_send() && !settled {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        match session.poll(&text, Instant::now()) {
            Progress::Pending => std::thread::sleep(Duration::from_millis(50)),
            Progress::Send(data) => {
                input
                    .write_all(&data)
                    .map_err(|err| format!("stdin write: {err}"))?;
                last_io = Instant::now();
            }
            Progress::Done => return Ok(()),
            Progress::TimedOut { step, last_lines } => {
                cancel.store(true, Ordering::Release);
                return Err(format!(
                    "step {step} timed out\nlog={}\nserial={}\n--- last serial ---\n{last_lines}",
                    host_log.display(),
                    serial_log.display()
                ));
            }
        }
    }
}

fn script() -> Vec<Step> {
    let banner = banner_timeout();
    let command = Duration::from_secs(15);
    vec![
        Step::Expect {
            pattern: "Linux version".to_string(),
            timeout: banner,
        },
        Step::Expect {
            pattern: "# ".to_string(),
            timeout: banner,
        },
        Step::Send {
            data: b"/bin/busybox ".to_vec(),
        },
        Step::Send {
            data: b"uname -a\n".to_vec(),
        },
        Step::Expect {
            pattern: "aarch64".to_string(),
            timeout: command,
        },
        Step::Send {
            data: b"echo OK\n".to_vec(),
        },
        Step::Expect {
            pattern: "\nOK".to_string(),
            timeout: command,
        },
        Step::Send {
            data: b"/bin/busybox ".to_vec(),
        },
        Step::Send {
            data: b"poweroff -f\n".to_vec(),
        },
    ]
}

fn banner_timeout() -> Duration {
    match std::env::var("TERNVALE_BOOT_BANNER_SECS") {
        Ok(text) => match text.parse::<u64>() {
            Ok(secs) if secs > 0 => {
                tracing::info!(target: "ternvale::boot", secs, "banner timeout override");
                Duration::from_secs(secs)
            }
            _ => {
                tracing::warn!(
                    target: "ternvale::boot",
                    value = %text,
                    "ignored banner timeout override"
                );
                Duration::from_secs(60)
            }
        },
        Err(_) => Duration::from_secs(60),
    }
}

fn log_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TERNVALE_BOOT_LOG_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/boot-logs")
        .join(std::process::id().to_string())
}

fn stdin_pipe() -> Result<(std::fs::File, i32), String> {
    let mut ends = [0; 2];
    // SAFETY: `pipe` writes two open fds into `ends` on success.
    let rc = unsafe { libc::pipe(ends.as_mut_ptr()) };
    if rc != 0 {
        return Err("pipe failed".to_string());
    }
    // SAFETY: `dup` saves stdin. `dup2` installs the pipe as the process stdin.
    let saved = unsafe { libc::dup(libc::STDIN_FILENO) };
    if saved < 0 {
        return Err("dup stdin failed".to_string());
    }
    let rc = unsafe { libc::dup2(ends[0], libc::STDIN_FILENO) };
    if rc != libc::STDIN_FILENO {
        return Err("dup2 stdin failed".to_string());
    }
    unsafe { libc::close(ends[0]) };
    // SAFETY: `ends[1]` is the pipe write end this function returns.
    let input = unsafe { std::fs::File::from_raw_fd(ends[1]) };
    Ok((input, saved))
}

fn restore_stdin(saved: i32) {
    // SAFETY: `saved` is the stdin fd captured before the pipe was installed.
    unsafe {
        libc::dup2(saved, libc::STDIN_FILENO);
        libc::close(saved);
    }
}

fn write_result(
    dir: &std::path::Path,
    exit: &Result<ExitReason, ternvale_vmm::MachineError>,
    script: &Result<(), String>,
    serial: &str,
) {
    let body = format!("exit={exit:?}\nscript={script:?}\n");
    if let Err(error) = std::fs::write(dir.join("result.txt"), body) {
        tracing::warn!(target: "ternvale::boot", error = %error, "could not write result.txt");
    }
    tracing::info!(
        target: "ternvale::boot",
        serial_bytes = serial.len(),
        dir = %dir.display(),
        "saved boot artifacts"
    );
}
