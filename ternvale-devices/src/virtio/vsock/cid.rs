//! Guest context id (CID) assignment.
//!
//! CIDs 0 (hypervisor), 1 (local), and 2 (host) are reserved, and `u32::MAX`
//! is `VMADDR_CID_ANY`. The spec says the upper 32 bits of `guest_cid` must be
//! zero, so a guest CID is in `3..u32::MAX`. Leases are process-wide so two
//! devices in one process never share a CID.

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard};

use super::VsockError;

/// Lowest CID a guest may use.
pub const FIRST_GUEST_CID: u32 = 3;
/// `VMADDR_CID_ANY`; never a guest CID.
const CID_ANY: u32 = u32::MAX;

static LEASED: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

fn leased() -> MutexGuard<'static, BTreeSet<u32>> {
    ternvale_vmm::lockwatch::lock(&LEASED, "vsock-cid-leases")
}

/// A CID reserved for one device. Dropping it frees the CID.
#[derive(Debug)]
pub struct CidLease {
    cid: u32,
}

impl CidLease {
    /// Reserve `requested`, or the lowest free CID from 3 when `None`.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::vsock", fields(requested))]
    pub fn acquire(requested: Option<u32>) -> Result<Self, VsockError> {
        let mut set = leased();
        let cid = match requested {
            Some(cid) => {
                if !(FIRST_GUEST_CID..CID_ANY).contains(&cid) {
                    tracing::warn!(
                        target: "ternvale::virtio::vsock",
                        cid,
                        "guest cid is reserved"
                    );
                    return Err(VsockError::BadCid { cid });
                }
                if set.contains(&cid) {
                    tracing::warn!(target: "ternvale::virtio::vsock", cid, "guest cid in use");
                    return Err(VsockError::CidInUse { cid });
                }
                cid
            }
            None => (FIRST_GUEST_CID..CID_ANY)
                .find(|cid| !set.contains(cid))
                .ok_or(VsockError::NoFreeCid)?,
        };
        set.insert(cid);
        tracing::info!(
            target: "ternvale::virtio::vsock",
            cid,
            auto = requested.is_none(),
            "guest cid assigned"
        );
        Ok(Self { cid })
    }

    /// The reserved CID.
    pub fn cid(&self) -> u32 {
        self.cid
    }
}

impl Drop for CidLease {
    fn drop(&mut self) {
        leased().remove(&self.cid);
        tracing::debug!(target: "ternvale::virtio::vsock", cid = self.cid, "guest cid released");
    }
}

#[cfg(test)]
mod tests {
    use super::{CidLease, FIRST_GUEST_CID};
    use crate::virtio::vsock::VsockError;

    #[test]
    fn rejects_reserved_cids() {
        for cid in [0, 1, 2, u32::MAX] {
            assert!(matches!(
                CidLease::acquire(Some(cid)),
                Err(VsockError::BadCid { .. })
            ));
        }
    }

    #[test]
    fn explicit_cid_is_exclusive_until_dropped() {
        let first = CidLease::acquire(Some(70_001)).expect("first");
        assert_eq!(first.cid(), 70_001);
        assert!(matches!(
            CidLease::acquire(Some(70_001)),
            Err(VsockError::CidInUse { cid: 70_001 })
        ));
        drop(first);
        let again = CidLease::acquire(Some(70_001)).expect("free after drop");
        assert_eq!(again.cid(), 70_001);
    }

    #[test]
    fn automatic_cids_are_distinct_and_at_least_three() {
        let a = CidLease::acquire(None).expect("a");
        let b = CidLease::acquire(None).expect("b");
        assert!(a.cid() >= FIRST_GUEST_CID && b.cid() >= FIRST_GUEST_CID);
        assert_ne!(a.cid(), b.cid());
    }
}
