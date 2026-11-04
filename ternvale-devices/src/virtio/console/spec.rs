//! virtio-console 1.2 §5.3 control messages and config layout.

/// Virtio console device id.
pub const VIRTIO_CONSOLE_ID: u32 = 3;
/// `VIRTIO_CONSOLE_F_MULTIPORT` (bit 1).
pub const F_MULTIPORT: u64 = 1 << 1;

/// Port 0 is the legacy console; the agent uses port 1.
pub const AGENT_PORT_ID: u32 = 1;
/// Ports the device advertises (`max_nr_ports`).
pub const MAX_PORTS: u32 = 2;
/// Queues: rx/tx per port plus control rx/tx. Spec 5.3.2.
pub const NUM_QUEUES: u16 = 2 * MAX_PORTS as u16 + 2;
/// Control receiveq (driver → device).
pub const CONTROL_RX: u16 = 2;
/// Control transmitq (device → driver).
pub const CONTROL_TX: u16 = 3;
/// Agent port receiveq (device → guest).
pub const AGENT_RX: u16 = 2 * AGENT_PORT_ID as u16 + 2;
/// Agent port transmitq (guest → device).
pub const AGENT_TX: u16 = 2 * AGENT_PORT_ID as u16 + 3;

/// virtio-win `vioserial` and Linux `virtio_console` port name.
///
/// Guest paths: Linux `/dev/virtio-ports/<name>`, Windows `\\.\Global\<name>`.
pub const AGENT_PORT_NAME: &str = ternvale_agent_proto::AGENT_SERIAL_NAME;

pub const DEVICE_READY: u16 = 0;
pub const PORT_ADD: u16 = 1;
pub const PORT_REMOVE: u16 = 2;
pub const PORT_READY: u16 = 3;
pub const CONSOLE_PORT: u16 = 4;
pub const RESIZE: u16 = 5;
pub const PORT_OPEN: u16 = 6;
pub const PORT_NAME: u16 = 7;

/// 8-byte `struct virtio_console_control` (little-endian). Spec 5.3.6.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Control {
    pub id: u32,
    pub event: u16,
    pub value: u16,
}

impl Control {
    /// Encode the header (no name payload).
    #[tracing::instrument(level = "trace", target = "ternvale::virtio::console", skip_all)]
    pub fn encode(self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0..4].copy_from_slice(&self.id.to_le_bytes());
        out[4..6].copy_from_slice(&self.event.to_le_bytes());
        out[6..8].copy_from_slice(&self.value.to_le_bytes());
        out
    }

    /// Parse a control header. Extra bytes (PORT_NAME) are ignored here.
    #[tracing::instrument(level = "trace", target = "ternvale::virtio::console", skip_all, fields(len = bytes.len()))]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 {
            return None;
        }
        Some(Self {
            id: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            event: u16::from_le_bytes(bytes[4..6].try_into().ok()?),
            value: u16::from_le_bytes(bytes[6..8].try_into().ok()?),
        })
    }

    /// Event name for logs.
    #[tracing::instrument(level = "trace", target = "ternvale::virtio::console", skip_all)]
    pub fn event_name(self) -> &'static str {
        match self.event {
            DEVICE_READY => "device_ready",
            PORT_ADD => "port_add",
            PORT_REMOVE => "port_remove",
            PORT_READY => "port_ready",
            CONSOLE_PORT => "console_port",
            RESIZE => "resize",
            PORT_OPEN => "port_open",
            PORT_NAME => "port_name",
            _ => "unknown",
        }
    }
}

/// `PORT_NAME` header plus NUL-terminated name.
#[tracing::instrument(level = "debug", target = "ternvale::virtio::console", skip_all)]
pub fn encode_port_name(id: u32, name: &str) -> Vec<u8> {
    let mut out = Control {
        id,
        event: PORT_NAME,
        value: 1,
    }
    .encode()
    .to_vec();
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_round_trip() {
        let msg = Control {
            id: AGENT_PORT_ID,
            event: PORT_OPEN,
            value: 1,
        };
        assert_eq!(Control::decode(&msg.encode()), Some(msg));
        assert_eq!(msg.event_name(), "port_open");
    }

    #[test]
    fn port_name_is_nul_terminated() {
        let bytes = encode_port_name(AGENT_PORT_ID, AGENT_PORT_NAME);
        assert_eq!(
            &bytes[0..8],
            &Control {
                id: AGENT_PORT_ID,
                event: PORT_NAME,
                value: 1,
            }
            .encode()
        );
        assert!(bytes.ends_with(b"org.ternvale.agent\0"));
    }

    #[test]
    fn queue_indices_match_spec() {
        assert_eq!(CONTROL_RX, 2);
        assert_eq!(CONTROL_TX, 3);
        assert_eq!(AGENT_RX, 4);
        assert_eq!(AGENT_TX, 5);
        assert_eq!(NUM_QUEUES, 6);
    }
}
