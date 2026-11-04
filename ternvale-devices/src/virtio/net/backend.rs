//! Host side of a virtio-net device.

use std::sync::Arc;

use ternvale_config::{ForwardProto, Nic};
use ternvale_metrics::Metrics;
use ternvale_net::{Forward, PolicyEngine, Proto, UserNet};

use super::loopback::LoopbackBackend;
use super::NetError;

/// Moves Ethernet frames (no virtio header, no FCS) between the device and the host.
///
/// The worker calls both methods from one thread. `recv` must not block: it
/// returns `Ok(None)` when no frame is waiting, and the worker polls again.
pub trait NetBackend: Send {
    /// Short name for logs, such as `loopback`.
    fn name(&self) -> &'static str;

    /// Take one guest TX frame.
    fn send(&mut self, frame: &[u8]) -> Result<(), NetError>;

    /// Next frame for the guest, if any.
    fn recv(&mut self) -> Result<Option<Vec<u8>>, NetError>;

    /// Virtio link status (`VIRTIO_NET_S_LINK_UP`). Defaults to up.
    fn link_up(&self) -> bool {
        true
    }

    /// MAC the guest should use. `None` keeps the VMM default.
    fn guest_mac(&self) -> Option<[u8; 6]> {
        None
    }

    /// Bind the virtio MAC and metrics sink used by outbound policy.
    fn attach_policy(&mut self, _mac: [u8; 6], _metrics: Arc<Metrics>) {}
}

/// Build the backend named in a config `nics[].backend` entry.
///
/// `user` (default) and `loopback` always work. `vmnet` needs the `vmnet` cargo feature.
#[tracing::instrument(level = "debug", target = "ternvale::net", fields(name))]
pub fn open_backend(name: &str) -> Result<Box<dyn NetBackend>, NetError> {
    let nic = Nic {
        backend: name.to_string(),
        mode: ternvale_config::NicMode::Shared,
        network: None,
        bridge_interface: None,
        forwards: Vec::new(),
        policy: ternvale_config::NicPolicy::default(),
    };
    open_nic(&nic)
}

/// Open the backend described by a full `[[nics]]` entry (mode / network / bridge).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::net",
    skip_all,
    fields(backend = %nic.backend, mode = nic.mode.as_str())
)]
pub fn open_nic(nic: &Nic) -> Result<Box<dyn NetBackend>, NetError> {
    match nic.backend.as_str() {
        "user" => open_user(nic),
        "loopback" => {
            tracing::info!(
                target: "ternvale::net",
                backend = "loopback",
                "net backend selected"
            );
            Ok(Box::new(LoopbackBackend::new()))
        }
        "vmnet" => open_vmnet(nic),
        other => {
            tracing::error!(target: "ternvale::net", backend = other, "unknown net backend");
            Err(NetError::UnknownBackend {
                name: other.to_string(),
            })
        }
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::net::user", skip_all)]
fn open_user(nic: &Nic) -> Result<Box<dyn NetBackend>, NetError> {
    let forwards: Vec<Forward> = nic
        .forwards
        .iter()
        .map(|fwd| Forward {
            proto: match fwd.proto {
                ForwardProto::Tcp => Proto::Tcp,
                ForwardProto::Udp => Proto::Udp,
            },
            host: std::net::Ipv4Addr::LOCALHOST,
            host_port: fwd.host_port,
            guest_port: fwd.guest_port,
        })
        .collect();
    tracing::info!(
        target: "ternvale::net::user",
        backend = "user",
        forwards = forwards.len(),
        allow_lan = nic.policy.allow_lan,
        "net backend selected"
    );
    let inner = UserNet::new(forwards).map_err(|error| NetError::User {
        reason: error.to_string(),
    })?;
    Ok(Box::new(PolicyBackend::wrap(
        UserBackend { inner },
        PolicyEngine::from_nic(nic),
    )))
}

/// User-mode NAT wrapped as a [`NetBackend`].
struct UserBackend {
    inner: UserNet,
}

impl NetBackend for UserBackend {
    fn name(&self) -> &'static str {
        "user"
    }

    fn send(&mut self, frame: &[u8]) -> Result<(), NetError> {
        self.inner.send(frame).map_err(|error| NetError::User {
            reason: error.to_string(),
        })
    }

    fn recv(&mut self) -> Result<Option<Vec<u8>>, NetError> {
        Ok(self.inner.poll())
    }
}

/// Guest-TX filter in front of user NAT or vmnet.
struct PolicyBackend {
    inner: Box<dyn NetBackend>,
    policy: PolicyEngine,
    metrics: Option<Arc<Metrics>>,
}

impl PolicyBackend {
    fn wrap(inner: impl NetBackend + 'static, policy: PolicyEngine) -> Self {
        Self {
            inner: Box::new(inner),
            policy,
            metrics: None,
        }
    }

    fn record_drop(&self, rule: &'static str) {
        if let Some(metrics) = &self.metrics {
            metrics.record_policy_drop(rule);
        }
    }
}

impl NetBackend for PolicyBackend {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    #[tracing::instrument(level = "trace", target = "ternvale::net::user", skip_all, fields(len = frame.len()))]
    fn send(&mut self, frame: &[u8]) -> Result<(), NetError> {
        if let Some(rule) = self.policy.decide_tx(frame) {
            self.record_drop(rule);
            return Ok(());
        }
        self.inner.send(frame)
    }

    fn recv(&mut self) -> Result<Option<Vec<u8>>, NetError> {
        let frame = self.inner.recv()?;
        if let Some(ref bytes) = frame {
            self.policy.observe_rx(bytes);
        }
        Ok(frame)
    }

    fn link_up(&self) -> bool {
        self.inner.link_up()
    }

    fn guest_mac(&self) -> Option<[u8; 6]> {
        self.inner.guest_mac()
    }

    fn attach_policy(&mut self, mac: [u8; 6], metrics: Arc<Metrics>) {
        self.policy.set_mac(mac);
        self.metrics = Some(metrics);
    }
}

#[cfg(feature = "vmnet")]
fn open_vmnet(nic: &Nic) -> Result<Box<dyn NetBackend>, NetError> {
    let backend = super::vmnet::VmnetBackend::open(nic)?;
    Ok(Box::new(PolicyBackend::wrap(
        backend,
        PolicyEngine::from_nic(nic),
    )))
}

#[cfg(not(feature = "vmnet"))]
fn open_vmnet(_nic: &Nic) -> Result<Box<dyn NetBackend>, NetError> {
    tracing::error!(
        target: "ternvale::net",
        "vmnet backend requested but ternvale-devices was built without the vmnet feature"
    );
    Err(NetError::Unsupported {
        backend: "vmnet",
        reason: "built without the `vmnet` cargo feature",
    })
}
