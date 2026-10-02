//! Host threads beside the vCPUs: stdin, SPI re-pulse and cancel poll, and the
//! hang watchdog. None of them owns a vCPU; they reach CPU 0 through
//! [`CpuPower::nudge`] and end the VM through [`CpuPower::request_stop`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::attach::{lock_serial, SharedSerial, SpiLevels};
use crate::smp::CpuPower;
use crate::vcpu::ExitReason;
use crate::watchdog::Watchdog;

/// SPIs are routed to CPU 0 (Linux's default `GICD_IROUTER`), so host input
/// and SPI re-pulses wake that vCPU.
const IRQ_CPU: u32 = 0;

pub(super) fn watch_loop(watchdog: Arc<Watchdog>, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_secs(1));
        if watchdog.poll().is_some() {
            tracing::debug!(target: "ternvale::vcpu", "hang watchdog fired");
        }
    }
}

pub(super) struct Poll {
    pub shutdown: Arc<AtomicBool>,
    pub power: Arc<CpuPower>,
    pub gic: Arc<ternvale_hv::Gic>,
    pub irq_level: Arc<AtomicBool>,
    pub spi_levels: SpiLevels,
    pub cancel: Arc<AtomicBool>,
}

pub(super) fn poll_loop(poll: Poll) {
    let uart_spi = 32 + crate::fdt::UART_SPI;
    while !poll.shutdown.load(Ordering::Acquire) {
        if poll.cancel.load(Ordering::Acquire) {
            tracing::warn!(target: "ternvale::boot", "cancel requested; stopping guest");
            poll.power.request_stop(ExitReason::Canceled);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        // hv_gic_set_spi(true) can be missed while the guest is inside an
        // emulated WFI. Pulse again only while the line stays high.
        if poll.irq_level.load(Ordering::Acquire) {
            if let Err(error) = poll.gic.set_spi(uart_spi, true) {
                tracing::debug!(
                    target: "ternvale::gic",
                    error = %error,
                    "uart spi reassert failed"
                );
            }
        }
        let levels = crate::lockwatch::lock(&poll.spi_levels, "spi-levels").clone();
        for (spi, level) in levels {
            if !level.load(Ordering::Acquire) {
                continue;
            }
            if let Err(error) = poll.gic.set_spi(spi, true) {
                tracing::debug!(
                    target: "ternvale::gic",
                    irq = spi,
                    error = %error,
                    "virtio spi reassert failed"
                );
            }
        }
        poll.power.nudge(IRQ_CPU);
    }
}

pub(super) fn stdin_loop(serial: SharedSerial, shutdown: Arc<AtomicBool>, power: Arc<CpuPower>) {
    let fd = libc::STDIN_FILENO;
    // SAFETY: F_GETFL reads the stdin status flags. The fd is the process stdin.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        tracing::warn!(target: "ternvale::uart", "stdin stays blocking; rx thread not started");
        return;
    }
    // SAFETY: F_SETFL only adds O_NONBLOCK to the flags just read.
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc < 0 {
        tracing::warn!(target: "ternvale::uart", "could not set stdin nonblocking");
        return;
    }
    let mut buf = [0u8; 64];
    while !shutdown.load(Ordering::Acquire) {
        // SAFETY: `buf` is a writable 64-byte stack buffer. stdin is nonblocking.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            let mut uart = lock_serial(&serial);
            for byte in &buf[..n as usize] {
                if !uart.push_rx(*byte) {
                    tracing::warn!(target: "ternvale::uart", "dropped stdin byte");
                }
            }
            drop(uart);
            power.nudge(IRQ_CPU);
        } else {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    // SAFETY: restore the flags captured before O_NONBLOCK was added.
    let restored = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    if restored < 0 {
        tracing::warn!(target: "ternvale::uart", "could not restore stdin flags");
    }
}
