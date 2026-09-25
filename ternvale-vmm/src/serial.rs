//! Host-facing serial port the machine wires to the PL011 window.

use std::sync::Arc;

use crate::mmio::MmioDevice;

/// UART the orchestrator registers on the bus and feeds from host stdin.
pub trait SerialDevice: MmioDevice {
    /// Queue one byte from the host. `false` when the RX FIFO is full.
    fn push_rx(&mut self, byte: u8) -> bool;

    /// Called with the masked interrupt level after RX, TX, or a register write.
    fn set_irq_hook(&mut self, hook: Arc<dyn Fn(bool) + Send + Sync>);
}
