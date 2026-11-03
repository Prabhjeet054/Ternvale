//! virtio-balloon through a split queue in fake guest memory.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_vmm::{GuestMemory, HOST_PAGE_SIZE};

use super::pages::{CountingReclaim, GUEST_PAGE, PFN_SHIFT};
use super::{BalloonConfig, VirtioBalloon, F_STATS_VQ, VIRTIO_BALLOON_ID};
use crate::virtio::irq::VirtioIrq;
use crate::virtio::{QueueNotify, VirtioDevice};

struct Fixture {
    memory: Arc<Mutex<GuestMemory>>,
    raised: Arc<AtomicBool>,
    base: u64,
}

impl Fixture {
    fn new(reclaim: CountingReclaim) -> (Self, VirtioBalloon) {
        let mut mem = GuestMemory::new().expect("memory");
        let base = 0x4000_0000u64;
        mem.add_region(base, HOST_PAGE_SIZE * 4).expect("region");
        mem.add_region(0, HOST_PAGE_SIZE).expect("ram");
        let memory = Arc::new(Mutex::new(mem));
        let irq = VirtioIrq::new();
        let raised = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&raised);
        irq.set_hook(Arc::new(move |level| {
            flag.store(level, Ordering::Release);
        }));
        let balloon = VirtioBalloon::new(
            Arc::clone(&memory),
            irq,
            BalloonConfig {
                ram_mib: 256,
                ram_min_mib: 64,
                metrics: None,
                vcpus: 1,
                watch_pressure: false,
                auto_policy: false,
                reclaim: Some(Box::new(reclaim)),
            },
        );
        (
            Self {
                memory,
                raised,
                base,
            },
            balloon,
        )
    }

    fn write_desc(&self, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let mut mem = self.memory.lock().expect("mem");
        let at = self.base + u64::from(index) * 16;
        mem.write_u64(at, addr).expect("addr");
        mem.write_u32(at + 8, len).expect("len");
        mem.write_u16(at + 12, flags).expect("flags");
        mem.write_u16(at + 14, next).expect("next");
    }

    fn notify(&self, balloon: &VirtioBalloon, index: u16) {
        balloon.notify(QueueNotify {
            index,
            size: 4,
            desc: self.base,
            avail: self.base + 0x400,
            used: self.base + 0x800,
            features: 0,
        });
    }

    fn wait_used(&self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let mem = self.memory.lock().expect("mem");
            if mem.read_u16(self.base + 0x802).expect("idx") >= 1 {
                return;
            }
            drop(mem);
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("balloon used ring did not advance");
    }
}

#[test]
fn identity_and_features() {
    let reclaim = CountingReclaim::default();
    let (fx, balloon) = Fixture::new(reclaim);
    assert_eq!(balloon.device_id(), VIRTIO_BALLOON_ID);
    assert_ne!(balloon.device_features() & F_STATS_VQ, 0);
    assert_eq!(balloon.num_queues(), 4);
    assert_eq!(balloon.read_config(0, 4), 0);
    drop(fx);
}

#[test]
fn inflate_four_guest_pages_advises_one_host_page() {
    let reclaim = CountingReclaim::default();
    let counters = reclaim.clone();
    let (fx, balloon) = Fixture::new(reclaim);
    let data = fx.base + 0x1000;
    let per_host = HOST_PAGE_SIZE / GUEST_PAGE;
    {
        let mut mem = fx.memory.lock().expect("mem");
        for i in 0..per_host {
            let pfn = i as u32;
            mem.write_u32(data + u64::from(i) * 4, pfn).expect("pfn");
        }
        mem.write_u16(fx.base + 0x400 + 4, 0).expect("head");
        mem.write_u16(fx.base + 0x402, 1).expect("idx");
    }
    fx.write_desc(0, data, (per_host * 4) as u32, 0, 0);
    fx.notify(&balloon, 0);
    fx.wait_used();
    assert!(fx.raised.load(Ordering::Acquire));
    assert_eq!(counters.reusable.load(Ordering::SeqCst), 1);
    drop(balloon);
    drop(fx);
}

#[test]
fn stats_vq_records_in_use() {
    let reclaim = CountingReclaim::default();
    let (fx, balloon) = Fixture::new(reclaim);
    let data = fx.base + 0x1000;
    {
        let mut mem = fx.memory.lock().expect("mem");
        // packed {tag:u16, val:u64} memtot then avail
        mem.write_u16(data, 5).expect("tag tot");
        mem.write_u64(data + 2, 256 << 20).expect("tot");
        mem.write_u16(data + 10, 6).expect("tag avail");
        mem.write_u64(data + 12, 200 << 20).expect("avail");
        mem.write_u16(fx.base + 0x400 + 4, 0).expect("head");
        mem.write_u16(fx.base + 0x402, 1).expect("idx");
    }
    fx.write_desc(0, data, 20, 0, 0);
    fx.notify(&balloon, 2);
    fx.wait_used();
    let snap = balloon.inner.snapshot();
    assert_eq!(snap.guest_memtot_bytes, Some(256 << 20));
    assert_eq!(snap.guest_in_use_bytes, Some(56 << 20));
}

#[test]
fn never_targets_below_guest_in_use() {
    let reclaim = CountingReclaim::default();
    let (fx, balloon) = Fixture::new(reclaim);
    balloon.inner.guest.update(5, 256 << 20);
    balloon.inner.guest.update(6, 10 << 20);
    balloon
        .inner
        .policy
        .set_pressure(ternvale_hv::MemoryPressure::Critical);
    let mut sample = super::policy::IdleSample::new();
    sample.idle = true;
    let target = balloon
        .inner
        .policy
        .refresh(&balloon.inner.guest, &mut sample);
    let in_use_pages = ((256 - 10) << 20) / (1u64 << PFN_SHIFT);
    let ram_pages = (256u64 << 20) / (1 << PFN_SHIFT);
    assert!(u64::from(target) <= ram_pages.saturating_sub(in_use_pages));
    drop(fx);
}
