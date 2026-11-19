//! Restore refusal / `--force` with injected ID maps (no Hypervisor.framework).

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

use super::{evaluate_restore, format_mismatch_error, from_hex_map, mismatches, CpuFingerprint};

fn fp(pairs: &[(&str, u64)]) -> CpuFingerprint {
    CpuFingerprint {
        profile: "host".into(),
        id_registers: pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
    }
}

#[test]
fn restore_refuses_higher_guest_isar_nibble() {
    let snap = fp(&[("ID_AA64ISAR0_EL1", 0x2)]);
    let host = fp(&[("ID_AA64ISAR0_EL1", 0x1)]);
    let miss = mismatches(&snap, &host);
    assert_eq!(miss.len(), 1);
    assert!(miss[0].describe().contains("ID_AA64ISAR0_EL1"));
    let err = format_mismatch_error(&miss);
    assert!(err.contains("refusing snapshot restore"));
    assert!(err.contains("--force"));
}

#[test]
fn host_superset_is_ok_and_empty_snapshot_skips() {
    let snap = fp(&[("ID_AA64ISAR0_EL1", 0x1)]);
    let host = fp(&[("ID_AA64ISAR0_EL1", 0x2)]);
    assert!(mismatches(&snap, &host).is_empty());
    assert!(mismatches(&CpuFingerprint::default(), &host).is_empty());
}

#[test]
fn hex_round_trip() {
    let mut map = BTreeMap::new();
    map.insert("ID_AA64PFR0_EL1".into(), "0x11".into());
    let fp = from_hex_map(Some("baseline"), &map);
    assert_eq!(fp.profile, "baseline");
    assert_eq!(fp.id_registers["ID_AA64PFR0_EL1"], 0x11);
}

#[test]
fn injected_weaker_host_refuses_and_lists_mismatch() {
    let snap = fp(&[("ID_AA64ISAR0_EL1", 0x22)]);
    let host = fp(&[("ID_AA64ISAR0_EL1", 0x01)]);
    let err = evaluate_restore(&snap, &host, false).expect_err("refuse");
    assert!(err.contains("ID_AA64ISAR0_EL1"), "{err}");
    assert!(err.contains("nibble 1"), "{err}");
    assert!(err.contains("snapshot 0x2"), "{err}");
    assert!(err.contains("host 0x1"), "{err}");
}

#[test]
fn force_proceeds_and_logs_a_warning() {
    let _second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let snap = fp(&[("ID_AA64ISAR0_EL1", 0x2)]);
    let host = fp(&[("ID_AA64ISAR0_EL1", 0x0)]);
    tracing::subscriber::with_default(subscriber, || {
        evaluate_restore(&snap, &host, true).expect("force");
    });
    let text = String::from_utf8(captured.0.lock().expect("captured").clone()).expect("utf8");
    assert!(
        text.contains("restoring snapshot despite CPU feature mismatch (--force)"),
        "{text}"
    );
    assert!(text.contains("ID_AA64ISAR0_EL1"), "{text}");
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("captured").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
