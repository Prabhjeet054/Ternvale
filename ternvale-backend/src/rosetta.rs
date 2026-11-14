//! Linux Rosetta is a **VZ** capability. Native guests do not get this share.

use crate::BackendError;

pub use ternvale_vz::{
    install_rosetta, require_rosetta_installed, rosetta_availability, rosetta_user_consented,
    RosettaAvailability, BINFMT_MAGIC, BINFMT_MASK, ROSETTA_FS_TAG, ROSETTA_GUEST_MOUNT,
    ROSETTA_RUNTIME,
};

/// `installRosetta` only after the operator types `yes`.
#[tracing::instrument(level = "info", target = "ternvale::rosetta", skip_all)]
pub fn install_after_consent(answer: &str) -> Result<(), BackendError> {
    install_rosetta(answer).map_err(BackendError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BackendError;

    #[test]
    fn no_consent_installs_nothing() {
        let err = install_after_consent("n").expect_err("declined");
        match err {
            BackendError::Io {
                op: "rosetta_install",
                ..
            } => {}
            other => panic!("{other}"),
        }
    }
}
