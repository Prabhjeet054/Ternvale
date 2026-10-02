//! Virtqueue configuration writes. Descriptor walking lives in `virtio::queue`.

use super::{QueueAddr, VirtioCore};

impl VirtioCore {
    /// Set the ring size of queue `sel`. 0 is allowed; otherwise a power of two
    /// no larger than `num_max`. Rejected while the queue is ready.
    pub(crate) fn set_queue_num(&mut self, sel: u32, value: u32) {
        let Some(queue) = self.queue(sel) else {
            self.warn_queue(sel, "queue num for an unknown queue");
            return;
        };
        if queue.ready {
            self.warn_queue(sel, "queue num while the queue is ready");
            return;
        }
        let max = queue.num_max;
        if value != 0 && (value > max || !value.is_power_of_two()) {
            tracing::warn!(
                target: "ternvale::virtio::transport",
                name = %self.name,
                queue = sel,
                value,
                max,
                "rejected queue num"
            );
            return;
        }
        if let Some(queue) = self.queue_mut(sel) {
            queue.num = value;
        }
    }

    /// Driver write of the ready / enable flag for queue `sel`.
    pub(crate) fn set_queue_ready(&mut self, sel: u32, value: u32) {
        let Some(queue) = self.queue(sel) else {
            self.warn_queue(sel, "queue ready for an unknown queue");
            return;
        };
        if value == 0 {
            if queue.ready {
                self.warn_queue(sel, "queue ready cleared without a device reset");
            }
            return;
        }
        if value != 1 {
            self.warn_queue(sel, "queue ready value is not 0 or 1");
            return;
        }
        if queue.num == 0 || queue.desc == 0 || queue.driver == 0 || queue.device == 0 {
            self.warn_queue(sel, "queue ready before the queue is configured");
            return;
        }
        let num = queue.num;
        if let Some(queue) = self.queue_mut(sel) {
            queue.ready = true;
        }
        tracing::info!(
            target: "ternvale::virtio::transport",
            name = %self.name,
            queue = sel,
            num,
            "virtio queue ready"
        );
    }

    /// Driver kick for queue `index`. Reaches the device only once it is ready.
    pub(crate) fn notify(&mut self, index: u32) {
        let notify = {
            let Some(queue) = self.queue(index) else {
                tracing::warn!(
                    target: "ternvale::virtio::transport",
                    name = %self.name,
                    queue = index,
                    "notify for an unknown queue"
                );
                return;
            };
            if !queue.ready {
                tracing::warn!(
                    target: "ternvale::virtio::transport",
                    name = %self.name,
                    queue = index,
                    "notify for a queue that is not ready"
                );
                return;
            }
            crate::virtio::QueueNotify {
                index: index as u16,
                size: queue.num as u16,
                desc: queue.desc,
                avail: queue.driver,
                used: queue.device,
                features: self.driver_features,
            }
        };
        tracing::trace!(
            target: "ternvale::virtio::transport",
            name = %self.name,
            queue = index,
            "virtio queue notify"
        );
        self.device.notify(notify);
    }

    /// Write the low or high 32 bits of one ring address of queue `sel`.
    pub(crate) fn set_queue_addr(&mut self, sel: u32, which: QueueAddr, high: bool, value: u32) {
        let Some(queue) = self.queue(sel) else {
            self.warn_queue(sel, "queue address for an unknown queue");
            return;
        };
        if queue.ready {
            self.warn_queue(sel, "queue address while the queue is ready");
            return;
        }
        let Some(queue) = self.queue_mut(sel) else {
            return;
        };
        let addr = match which {
            QueueAddr::Desc => &mut queue.desc,
            QueueAddr::Driver => &mut queue.driver,
            QueueAddr::Device => &mut queue.device,
        };
        if high {
            *addr = (*addr & 0xffff_ffff) | (u64::from(value) << 32);
        } else {
            *addr = (*addr & 0xffff_ffff_0000_0000) | u64::from(value);
        }
    }
}
