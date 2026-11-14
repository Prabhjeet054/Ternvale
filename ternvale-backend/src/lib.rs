//! Hypervisor backends behind one trait ([`VmBackend`]).
//!
//! Callers outside a backend implementation must use this crate. Capability
//! flags, not `if backend == "vz"` branches, decide what is offered.

use std::sync::Mutex;

use ternvale_config::{BackendKind, VmConfig};
use ternvale_vmm::VmState;

mod caps;
/// Conformance helpers invoked by [`backend_conformance`].
pub mod conformance;
mod error;
mod macos;
mod macos_limit;
mod mock;
mod native;
mod opened;
mod select;
mod types;
mod vz;

pub use caps::BackendCapabilities;
pub use conformance::{
    graceful_stop, invalid_transitions, lifecycle, run_all, stats_shape, unsupported_features,
};
pub use error::BackendError;
pub use macos::{
    clone_macos_platform, download_restore, inspect_restore, install_macos, new_machine_identifier,
    persist_platform, require_supported,
};
pub use macos_limit::{refuse_if_at_limit, running_macos_guests, MACOS_CONCURRENT_LIMIT};
pub use mock::MockBackend;
pub use native::NativeBackend;
pub use opened::Opened;
pub use select::{resolve, select, Selection};
pub use ternvale_vz::{DownloadResult, MacosIdentity, MacosRestoreInfo, AUX_NAME, IDENTITY_NAME};
pub use types::{
    AgentChannel, BackendStats, DisplaySurface, InputSink, MediaAttachment, MediaSlot, StopMode,
};
pub use vz::VzBackend;

/// Async VM operations every backend implements.
pub trait VmBackend: Send + Sync {
    /// Feature flags. A false bit is [`BackendError::Unsupported`], never a no-op.
    fn capabilities(&self) -> BackendCapabilities;

    /// Created → Running.
    fn start(&self) -> impl std::future::Future<Output = Result<(), BackendError>> + Send;

    /// Running → Paused.
    fn pause(&self) -> impl std::future::Future<Output = Result<(), BackendError>> + Send;

    /// Paused → Running.
    fn resume(&self) -> impl std::future::Future<Output = Result<(), BackendError>> + Send;

    /// Stop with [`StopMode::Graceful`] or [`StopMode::Force`].
    fn stop(
        &self,
        mode: StopMode,
    ) -> impl std::future::Future<Output = Result<(), BackendError>> + Send;

    /// Current lifecycle state.
    fn state(&self) -> VmState;

    /// Stream of state values; the first item is the current state.
    fn subscribe_state(&self) -> std::sync::mpsc::Receiver<VmState>;

    /// CPU counts used by the conformance suite.
    fn query_stats(
        &self,
    ) -> impl std::future::Future<Output = Result<BackendStats, BackendError>> + Send;

    /// Attach an ISO (or similar) in `slot`.
    fn attach_media(
        &self,
        slot: MediaSlot,
        path: &std::path::Path,
    ) -> impl std::future::Future<Output = Result<(), BackendError>> + Send;

    /// Detach `slot`.
    fn detach_media(
        &self,
        slot: MediaSlot,
    ) -> impl std::future::Future<Output = Result<(), BackendError>> + Send;

    /// Save a snapshot. Native without a snapshot host maps to Unsupported.
    fn snapshot_save(
        &self,
        label: &str,
        path: &std::path::Path,
    ) -> impl std::future::Future<Output = Result<(), BackendError>> + Send;

    /// Restore a snapshot.
    fn snapshot_restore(
        &self,
        label: &str,
        path: &std::path::Path,
    ) -> impl std::future::Future<Output = Result<(), BackendError>> + Send;

    /// Guest framebuffer. Native has no custom pipeline.
    fn display_surface(
        &self,
    ) -> impl std::future::Future<Output = Result<DisplaySurface, BackendError>> + Send;

    /// Keyboard / pointer sink.
    fn input_sink(&self) -> std::sync::Arc<dyn InputSink>;

    /// vsock-like byte stream to the guest agent.
    fn agent_channel(
        &self,
    ) -> impl std::future::Future<Output = Result<AgentChannel, BackendError>> + Send;
}

/// Open the resolved backend (native stub orchestrator or VZ Linux host).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::backend",
    skip_all,
    fields(vm = %config.name, backend = %config.backend)
)]
pub async fn open(config: VmConfig) -> Result<Opened, BackendError> {
    let selection = select(&config)?;
    match selection.resolved {
        BackendKind::Native => Ok(Opened::Native(NativeBackend::create(config).await?)),
        BackendKind::Vz => Ok(Opened::Vz(VzBackend::create_live(config).await?)),
        BackendKind::Auto => Err(BackendError::Unsupported { feature: "auto" }),
    }
}

/// [`open`] for callers without an async runtime.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::backend",
    skip_all,
    fields(vm = %config.name)
)]
pub fn open_sync(config: VmConfig) -> Result<Opened, BackendError> {
    pollster::block_on(open(config))
}

/// Run the conformance suite against `$make()`, which must yield a Created backend.
#[macro_export]
macro_rules! backend_conformance {
    ($name:ident, $cpus:expr, $make:expr) => {
        #[test]
        fn $name() {
            pollster::block_on(async {
                $crate::conformance::run_all(|| async { $make }, $cpus).await;
            });
        }
    };
}

fn lock_listeners<T>(mutex: &Mutex<T>) -> ternvale_vmm::lockwatch::Guard<'_, T> {
    ternvale_vmm::lockwatch::lock(mutex, "backend-listeners")
}

fn lock_media<T>(mutex: &Mutex<T>) -> ternvale_vmm::lockwatch::Guard<'_, T> {
    ternvale_vmm::lockwatch::lock(mutex, "backend-media")
}

fn lock_agent<T>(mutex: &Mutex<T>) -> ternvale_vmm::lockwatch::Guard<'_, T> {
    ternvale_vmm::lockwatch::lock(mutex, "backend-agent")
}

fn lock_state<T>(mutex: &Mutex<T>) -> ternvale_vmm::lockwatch::Guard<'_, T> {
    ternvale_vmm::lockwatch::lock(mutex, "backend-state")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternvale_config::{BackendKind, Profile, TpmSection, VmConfig};

    fn dummy_config(name: &str, cpus: u32) -> VmConfig {
        use std::path::PathBuf;
        VmConfig {
            name: name.into(),
            profile: Profile::Linux,
            backend: BackendKind::Native,
            cpus,
            ram_mib: 256,
            ram_min_mib: None,
            kernel: PathBuf::from("/k"),
            initrd: None,
            cmdline: String::new(),
            boot_disk: false,
            disks: Vec::new(),
            iso: None,
            nics: Vec::new(),
            serial_log: PathBuf::from("/tmp/s.log"),
            firmware: None,
            nvram: None,
            firmware_tables: None,
            vsock: None,
            tpm: None,
            usb: None,
            audio: None,
            shares: Vec::new(),
            power: Default::default(),
            encryption: false,
            agent: Default::default(),
        }
    }

    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-backend");
    }

    #[test]
    fn native_conformance() {
        pollster::block_on(async {
            run_all(
                || async {
                    NativeBackend::create(dummy_config("native-conf", 2))
                        .await
                        .expect("create")
                },
                2,
            )
            .await;
        });
    }

    #[test]
    fn mock_conformance() {
        pollster::block_on(async {
            run_all(|| async { MockBackend::new("mock-conf", 2) }, 2).await;
        });
    }

    #[test]
    fn open_opens_vz_linux() {
        let mut config = dummy_config("vz-vm", 1);
        config.backend = BackendKind::Vz;
        config.tpm = Some(TpmSection {
            enabled: false,
            ..TpmSection::default()
        });
        match pollster::block_on(open(config)) {
            Ok(Opened::Vz(backend)) => {
                assert!(backend.capabilities().guest_linux);
                assert!(!backend.capabilities().tpm);
            }
            Ok(Opened::Native(_)) => panic!("expected Opened::Vz"),
            Err(error) => panic!("{error}"),
        }
    }

    #[test]
    fn open_vz_rejects_tpm() {
        let mut config = dummy_config("vz-tpm", 1);
        config.backend = BackendKind::Vz;
        match pollster::block_on(open(config)) {
            Err(BackendError::UnsupportedOption {
                capability: "tpm", ..
            }) => {}
            Err(error) => panic!("{error}"),
            Ok(_) => panic!("tpm must fail on vz"),
        }
    }

    #[test]
    fn vz_stub_conformance() {
        pollster::block_on(async {
            run_all(
                || async {
                    let mut config = dummy_config("vz-conf", 2);
                    config.backend = BackendKind::Vz;
                    config.tpm = Some(TpmSection {
                        enabled: false,
                        ..TpmSection::default()
                    });
                    VzBackend::create(config).await.expect("create")
                },
                2,
            )
            .await;
        });
    }

    #[test]
    fn api_cli_and_client_do_not_take_vz_or_hv_internals() {
        let api = include_str!("../../ternvale-api/Cargo.toml");
        let cli = include_str!("../../ternvale-cli/Cargo.toml");
        let client = include_str!("../../ternvale-client/Cargo.toml");
        for (name, text) in [("api", api), ("cli", cli), ("client", client)] {
            assert!(
                !text.contains("ternvale-vz"),
                "{name} must not depend on ternvale-vz"
            );
        }
        assert!(
            !api.contains("ternvale-hv"),
            "ternvale-api must not depend on ternvale-hv"
        );
        assert!(
            !client.contains("ternvale-hv"),
            "ternvale-client must not depend on ternvale-hv"
        );
    }
}
