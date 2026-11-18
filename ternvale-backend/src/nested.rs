//! Probe Virtualization.framework nested virt (capability query only).

/// `VZGenericPlatformConfiguration.isNestedVirtualizationSupported`.
pub const VZ_NESTED_PROPERTY: &str = "nestedVirtualizationSupported";
/// Setter on an instance; Ternvale does not enable it in this step.
pub const VZ_NESTED_ENABLED: &str = "nestedVirtualizationEnabled";
/// Header `API_AVAILABLE(macos(15.0))`.
pub const VZ_NESTED_API_MACOS: &str = "15.0";

/// Result of [`probe_vz_nested`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VzNestedProbe {
    pub property: &'static str,
    pub enabled_property: &'static str,
    pub api_available_macos: &'static str,
    /// `None` if the class property is not in this macOS.
    pub supported: Option<bool>,
}

/// Ask VZ whether nested virtualization is available. Does not create a VM
/// and does not set `nestedVirtualizationEnabled`.
#[tracing::instrument(level = "info", target = "ternvale::nested", skip_all)]
pub fn probe_vz_nested() -> VzNestedProbe {
    let raw = ternvale_vz::nested_supported();
    let supported = match raw {
        1 => Some(true),
        0 => Some(false),
        _ => None,
    };
    tracing::info!(
        target: "ternvale::nested",
        raw,
        supported = ?supported,
        "VZ nestedVirtualizationSupported"
    );
    VzNestedProbe {
        property: VZ_NESTED_PROPERTY,
        enabled_property: VZ_NESTED_ENABLED,
        api_available_macos: VZ_NESTED_API_MACOS,
        supported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_names_match_vzgenericplatformconfiguration() {
        assert_eq!(VZ_NESTED_PROPERTY, "nestedVirtualizationSupported");
        assert_eq!(VZ_NESTED_ENABLED, "nestedVirtualizationEnabled");
        assert_eq!(VZ_NESTED_API_MACOS, "15.0");
    }
}
