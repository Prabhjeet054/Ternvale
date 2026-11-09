//! JSON-RPC method names. New methods may be added; existing names are frozen.

/// One control method (JSON-RPC `method` string).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    Handshake,
    List,
    Create,
    Delete,
    Start,
    Stop,
    Pause,
    Resume,
    SnapshotCreate,
    SnapshotRestore,
    SnapshotList,
    SnapshotDelete,
    Clone,
    Stats,
    Balloon,
    MediaAttach,
    MediaDetach,
    UsbAttach,
    NetConfigure,
    Subscribe,
    /// Per-VM legacy alias of `stats` (`query-stats` on the cmd protocol).
    Status,
}

impl Method {
    /// Wire name.
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Handshake => "handshake",
            Self::List => "list",
            Self::Create => "create",
            Self::Delete => "delete",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::SnapshotCreate => "snapshot.create",
            Self::SnapshotRestore => "snapshot.restore",
            Self::SnapshotList => "snapshot.list",
            Self::SnapshotDelete => "snapshot.delete",
            Self::Clone => "clone",
            Self::Stats => "stats",
            Self::Balloon => "balloon",
            Self::MediaAttach => "media.attach",
            Self::MediaDetach => "media.detach",
            Self::UsbAttach => "usb.attach",
            Self::NetConfigure => "net.configure",
            Self::Subscribe => "subscribe",
            Self::Status => "status",
        }
    }

    /// Parse a wire name.
    #[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(name))]
    pub fn parse(name: &str) -> Option<Self> {
        METHODS.iter().copied().find(|m| m.as_str() == name)
    }
}

/// Every method in schema order. Do not reorder: Swift generation and docs follow this.
pub const METHODS: &[Method] = &[
    Method::Handshake,
    Method::List,
    Method::Create,
    Method::Delete,
    Method::Start,
    Method::Stop,
    Method::Pause,
    Method::Resume,
    Method::SnapshotCreate,
    Method::SnapshotRestore,
    Method::SnapshotList,
    Method::SnapshotDelete,
    Method::Clone,
    Method::Stats,
    Method::Balloon,
    Method::MediaAttach,
    Method::MediaDetach,
    Method::UsbAttach,
    Method::NetConfigure,
    Method::Subscribe,
    Method::Status,
];
