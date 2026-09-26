//! Virtio status state machine. Every accepted or rejected write is logged.

use super::VirtioMmio;
use crate::virtio::{
    STATUS_ACKNOWLEDGE, STATUS_DRIVER, STATUS_DRIVER_OK, STATUS_FAILED, STATUS_FEATURES_OK,
    VIRTIO_F_VERSION_1,
};

const DRIVER_STATUS_BITS: u32 =
    STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK | STATUS_FEATURES_OK | STATUS_FAILED;

const ACK: u32 = STATUS_ACKNOWLEDGE;
const DRIVER: u32 = STATUS_ACKNOWLEDGE | STATUS_DRIVER;
const FEATURES: u32 = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK;
const DRIVER_OK: u32 = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK;

impl VirtioMmio {
    pub(super) fn write_driver_features(&mut self, value: u32) {
        if self.status & STATUS_FEATURES_OK != 0 {
            tracing::warn!(
                target: "ternvale::virtio::mmio",
                name = %self.name,
                "driver feature write after FEATURES_OK"
            );
            return;
        }
        match self.driver_features_sel {
            0 => self.driver_features = (self.driver_features & !0xffff_ffff) | u64::from(value),
            1 => {
                self.driver_features =
                    (self.driver_features & 0xffff_ffff) | (u64::from(value) << 32);
            }
            _ => tracing::warn!(
                target: "ternvale::virtio::mmio",
                name = %self.name,
                sel = self.driver_features_sel,
                "driver feature select out of range"
            ),
        }
    }

    pub(super) fn write_status(&mut self, requested: u32) {
        if requested == 0 {
            let from = self.status;
            self.reset();
            tracing::info!(
                target: "ternvale::virtio::mmio",
                name = %self.name,
                from,
                to = 0,
                "virtio status reset"
            );
            self.device.status_changed(0);
            return;
        }
        if let Err(reason) = self.check_status(requested) {
            tracing::error!(
                target: "ternvale::virtio::mmio",
                name = %self.name,
                from = self.status,
                requested,
                reason,
                "virtio status rejected"
            );
            return;
        }
        if requested == self.status {
            return;
        }
        let from = self.status;
        self.status = requested;
        tracing::info!(
            target: "ternvale::virtio::mmio",
            name = %self.name,
            from,
            to = requested,
            driver_features = format!("{:#x}", self.driver_features),
            "virtio status"
        );
        self.device.status_changed(requested);
    }

    fn check_status(&self, requested: u32) -> Result<(), &'static str> {
        if requested & !DRIVER_STATUS_BITS != 0 {
            return Err("unknown status bit");
        }
        let old = self.status & !STATUS_FAILED;
        let new = requested & !STATUS_FAILED;
        if new & old != old {
            return Err("status bits cleared without reset");
        }
        if !status_prefix(new) {
            return Err("out of order");
        }
        if status_step(new) > status_step(old) + 1 {
            return Err("skipped status step");
        }
        if new & STATUS_FEATURES_OK != 0 && old & STATUS_FEATURES_OK == 0 {
            self.check_features()?;
        }
        Ok(())
    }

    fn check_features(&self) -> Result<(), &'static str> {
        let offered = self.offered();
        if self.driver_features & !offered != 0 {
            return Err("driver features are not a subset of device features");
        }
        if self.driver_features & VIRTIO_F_VERSION_1 == 0 {
            return Err("missing VIRTIO_F_VERSION_1");
        }
        Ok(())
    }

    fn reset(&mut self) {
        self.device_features_sel = 0;
        self.driver_features_sel = 0;
        self.driver_features = 0;
        self.queue_sel = 0;
        self.status = 0;
        self.interrupt.ack(0b11);
        for queue in &mut self.queues {
            queue.num = 0;
            queue.ready = false;
            queue.desc = 0;
            queue.driver = 0;
            queue.device = 0;
        }
    }
}

fn status_prefix(status: u32) -> bool {
    status == 0 || status == ACK || status == DRIVER || status == FEATURES || status == DRIVER_OK
}

fn status_step(status: u32) -> u8 {
    match status {
        0 => 0,
        ACK => 1,
        DRIVER => 2,
        FEATURES => 3,
        DRIVER_OK => 4,
        _ => 5,
    }
}
