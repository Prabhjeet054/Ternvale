//! Shared helpers for the hypervisor boot harness (`tests/boot.rs`).

use std::io::Write;
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ternvale_devices::{Progress, Session, Step};
use ternvale_vmm::ExitReason;

pub fn banner_timeout() -> Duration {
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

pub fn log_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TERNVALE_BOOT_LOG_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/boot-logs")
        .join(std::process::id().to_string())
}

pub fn assets_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test-assets")
}

pub fn stdin_pipe() -> Result<(std::fs::File, i32), String> {
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

pub fn restore_stdin(saved: i32) {
    // SAFETY: `saved` is the stdin fd captured before the pipe was installed.
    unsafe {
        libc::dup2(saved, libc::STDIN_FILENO);
        libc::close(saved);
    }
}

pub fn write_result(
    dir: &Path,
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

pub fn drive(
    serial_log: &Path,
    input: &mut std::fs::File,
    cancel: &AtomicBool,
    host_log: &Path,
    steps: Vec<Step>,
) -> Result<(), String> {
    let mut session = Session::new(steps, Instant::now());
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

pub fn init_logging(name: &str, log_dir: &Path) -> Result<ternvale_log::LogGuard, String> {
    init_logging_at(name, log_dir, "info")
}

/// Like [`init_logging`] with a `TERNVALE_LOG`-style filter, e.g. `info,ternvale::net=trace`.
pub fn init_logging_at(
    name: &str,
    log_dir: &Path,
    level: &str,
) -> Result<ternvale_log::LogGuard, String> {
    std::fs::create_dir_all(log_dir)
        .map_err(|err| format!("create {}: {err}", log_dir.display()))?;
    // SAFETY: this process owns the environment. The harness sets the log filter for its run.
    unsafe { std::env::set_var("TERNVALE_LOG", level) };
    let mut config = ternvale_log::LogConfig::new(name, log_dir.to_path_buf());
    config.level = level.to_string();
    ternvale_log::init(config).map_err(|err| err.to_string())
}

/// Chunked stdin write so PL011's 16-byte RX FIFO is not overrun.
#[allow(dead_code)]
pub fn send_chunks(input: &mut std::fs::File, chunks: &[&[u8]]) -> Result<(), String> {
    for chunk in chunks {
        input
            .write_all(chunk)
            .map_err(|err| format!("stdin write: {err}"))?;
        std::thread::sleep(Duration::from_millis(40));
    }
    Ok(())
}
