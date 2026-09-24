//! Create order: `hv_vm_create`, then the GIC, then vCPUs.

use std::sync::Mutex;

use crate::error::HvError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    None,
    Vm,
    Gic,
    Vcpu,
}

#[derive(Debug)]
pub(crate) struct Order {
    phase: Phase,
}

impl Order {
    const fn new() -> Self {
        Self { phase: Phase::None }
    }

    pub(crate) fn on_vm_created(&mut self) {
        self.phase = Phase::Vm;
    }

    pub(crate) fn on_vm_destroyed(&mut self) {
        self.phase = Phase::None;
    }

    pub(crate) fn begin_gic(&mut self) -> Result<(), HvError> {
        match self.phase {
            Phase::Vm => {
                self.phase = Phase::Gic;
                Ok(())
            }
            Phase::None => Err(HvError::GicBeforeVm),
            Phase::Gic => Err(HvError::GicExists),
            Phase::Vcpu => Err(HvError::GicAfterVcpu),
        }
    }

    pub(crate) fn rollback_gic(&mut self) {
        if self.phase == Phase::Gic {
            self.phase = Phase::Vm;
        }
    }

    pub(crate) fn begin_vcpu(&mut self) -> Result<(), HvError> {
        match self.phase {
            Phase::Gic | Phase::Vcpu => {
                self.phase = Phase::Vcpu;
                Ok(())
            }
            Phase::None | Phase::Vm => Err(HvError::VcpuBeforeGic),
        }
    }
}

static ORDER: Mutex<Order> = Mutex::new(Order::new());

pub(crate) fn lock_order() -> std::sync::MutexGuard<'static, Order> {
    ORDER.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests {
    use super::Order;
    use crate::HvError;

    #[test]
    fn gic_requires_a_vm_and_precedes_vcpus() {
        let mut order = Order::new();
        assert!(matches!(order.begin_gic(), Err(HvError::GicBeforeVm)));
        assert!(matches!(order.begin_vcpu(), Err(HvError::VcpuBeforeGic)));
        order.on_vm_created();
        assert!(matches!(order.begin_vcpu(), Err(HvError::VcpuBeforeGic)));
        order.begin_gic().expect("gic");
        assert!(matches!(order.begin_gic(), Err(HvError::GicExists)));
        order.begin_vcpu().expect("vcpu");
        assert!(matches!(order.begin_gic(), Err(HvError::GicAfterVcpu)));
        order.on_vm_destroyed();
        assert!(matches!(order.begin_gic(), Err(HvError::GicBeforeVm)));
    }

    #[test]
    fn a_failed_gic_create_can_be_retried() {
        let mut order = Order::new();
        order.on_vm_created();
        order.begin_gic().expect("gic");
        order.rollback_gic();
        order.begin_gic().expect("retry");
    }
}
