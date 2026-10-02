//! The `os` string sent in `Hello`.

/// `"<kernel> <release> <arch>"` on Linux (from `/proc/sys/kernel`), else
/// `"<os> <arch>"`.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
pub fn os_description() -> String {
    let arch = std::env::consts::ARCH;
    let read = |path: &str| {
        std::fs::read_to_string(path)
            .map(|text| text.trim().to_string())
            .map_err(|error| {
                tracing::debug!(target: "ternvale::agent", path, error = %error, "kernel info unavailable");
            })
            .ok()
            .filter(|text| !text.is_empty())
    };
    let description = match (
        read("/proc/sys/kernel/ostype"),
        read("/proc/sys/kernel/osrelease"),
    ) {
        (Some(kind), Some(release)) => format!("{kind} {release} {arch}"),
        _ => format!("{} {arch}", std::env::consts::OS),
    };
    let mut description = description;
    description.truncate(ternvale_agent_proto::MAX_NAME);
    description
}
