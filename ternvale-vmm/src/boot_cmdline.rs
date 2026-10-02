//! Kernel command line defaults for initrd and disk-root boots.

/// Kernel command line used when the config leaves it empty (initrd / busybox).
pub const DEFAULT_CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init";

/// Default cmdline when [`crate::machine::Machine`] boots with `boot_disk`.
pub const DISK_ROOT_CMDLINE: &str =
    "console=ttyAMA0 earlycon=pl011,0x9000000 root=/dev/vda rootfstype=ext4 rw";

/// `cmdline`, or [`DEFAULT_CMDLINE`] when it is empty.
#[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
pub fn guest_cmdline(cmdline: &str) -> String {
    guest_cmdline_for(cmdline, false)
}

/// Resolve the guest cmdline, appending `root=/dev/vda` when `boot_disk` is set.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::boot",
    skip_all,
    fields(boot_disk)
)]
pub fn guest_cmdline_for(cmdline: &str, boot_disk: bool) -> String {
    let resolved = if boot_disk {
        if cmdline.is_empty() {
            DISK_ROOT_CMDLINE.to_string()
        } else if has_root_arg(cmdline) {
            cmdline.to_string()
        } else {
            format!("{cmdline} root=/dev/vda rootfstype=ext4 rw")
        }
    } else if cmdline.is_empty() {
        DEFAULT_CMDLINE.to_string()
    } else {
        cmdline.to_string()
    };
    tracing::info!(
        target: "ternvale::boot",
        cmdline = %resolved,
        boot_disk,
        "kernel cmdline"
    );
    resolved
}

fn has_root_arg(cmdline: &str) -> bool {
    cmdline
        .split_whitespace()
        .any(|token| token.starts_with("root="))
}

#[cfg(test)]
mod tests {
    use super::{guest_cmdline, guest_cmdline_for, DEFAULT_CMDLINE, DISK_ROOT_CMDLINE};

    #[test]
    fn empty_cmdline_uses_default() {
        assert_eq!(guest_cmdline(""), DEFAULT_CMDLINE);
        assert_eq!(guest_cmdline("console=ttyAMA0"), "console=ttyAMA0");
    }

    #[test]
    fn boot_disk_appends_root_when_missing() {
        assert_eq!(guest_cmdline_for("", true), DISK_ROOT_CMDLINE);
        assert_eq!(
            guest_cmdline_for("console=ttyAMA0", true),
            "console=ttyAMA0 root=/dev/vda rootfstype=ext4 rw"
        );
        assert_eq!(
            guest_cmdline_for("console=ttyAMA0 root=/dev/vda1", true),
            "console=ttyAMA0 root=/dev/vda1"
        );
    }
}
