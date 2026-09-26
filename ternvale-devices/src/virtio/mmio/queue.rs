//! Virtqueue register writes. Descriptor walking lives in `virtio::queue`.

use super::{Queue, VirtioMmio};

impl VirtioMmio {
    pub(super) fn write_queue_num(&mut self, value: u32) {
        let Some(queue) = self.selected_mut() else {
            self.warn_queue("queue num for an unknown queue");
            return;
        };
        if queue.ready {
            self.warn_queue("queue num while the queue is ready");
            return;
        }
        let max = queue.num_max;
        let sel = self.queue_sel;
        let name = self.name.clone();
        if value != 0 && (value > max || !value.is_power_of_two()) {
            tracing::warn!(
                target: "ternvale::virtio::mmio",
                name,
                queue = sel,
                value,
                max,
                "rejected queue num"
            );
            return;
        }
        if let Some(queue) = self.selected_mut() {
            queue.num = value;
        }
    }

    pub(super) fn write_queue_ready(&mut self, value: u32) {
        let sel = self.queue_sel;
        let name = self.name.clone();
        let Some(queue) = self.selected_mut() else {
            self.warn_queue("queue ready for an unknown queue");
            return;
        };
        if value == 0 {
            if queue.ready {
                self.warn_queue("queue ready cleared without a device reset");
            }
            return;
        }
        if value != 1 {
            self.warn_queue("queue ready value is not 0 or 1");
            return;
        }
        if queue.num == 0 || queue.desc == 0 || queue.driver == 0 || queue.device == 0 {
            self.warn_queue("queue ready before the queue is configured");
            return;
        }
        let num = queue.num;
        queue.ready = true;
        tracing::info!(
            target: "ternvale::virtio::mmio",
            name,
            queue = sel,
            num,
            "virtio queue ready"
        );
    }

    pub(super) fn write_notify(&mut self, value: u32) {
        let notify = {
            let Some(queue) = self.queues.get(value as usize) else {
                tracing::warn!(
                    target: "ternvale::virtio::mmio",
                    name = %self.name,
                    queue = value,
                    "notify for an unknown queue"
                );
                return;
            };
            if !queue.ready {
                tracing::warn!(
                    target: "ternvale::virtio::mmio",
                    name = %self.name,
                    queue = value,
                    "notify for a queue that is not ready"
                );
                return;
            }
            crate::virtio::QueueNotify {
                index: value as u16,
                size: queue.num as u16,
                desc: queue.desc,
                avail: queue.driver,
                used: queue.device,
                features: self.driver_features,
            }
        };
        tracing::trace!(
            target: "ternvale::virtio::mmio",
            name = %self.name,
            queue = value,
            "virtio queue notify"
        );
        self.device.notify(notify);
    }

    pub(super) fn write_addr(
        &mut self,
        field: impl FnOnce(&mut Queue) -> &mut u64,
        high: bool,
        value: u32,
    ) {
        let Some(queue) = self.selected_mut() else {
            self.warn_queue("queue address for an unknown queue");
            return;
        };
        if queue.ready {
            self.warn_queue("queue address while the queue is ready");
            return;
        }
        let addr = field(queue);
        if high {
            *addr = (*addr & 0xffff_ffff) | (u64::from(value) << 32);
        } else {
            *addr = (*addr & 0xffff_ffff_0000_0000) | u64::from(value);
        }
    }
}
