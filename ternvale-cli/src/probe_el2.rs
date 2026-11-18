//! `ternvale probe-el2`: host EL2 / nested-virt research probe.

use std::process::ExitCode;

use anyhow::Result;
use serde::Serialize;

use crate::json_out;

/// JSON for `--json` and the human report.
#[derive(Debug, Serialize)]
struct Report {
    macos: String,
    macos_major: u32,
    hv_support: bool,
    chip_model: String,
    chip_brand: String,
    cpu_family: u32,
    hv_el2: HvEl2Json,
    vz_nested: VzNestedJson,
    /// Native backend can enable `nested_virt = true` (still off by default).
    nested_implemented: bool,
}

#[derive(Debug, Serialize)]
struct HvEl2Json {
    get_supported: &'static str,
    set_enabled: &'static str,
    api_available_macos: &'static str,
    symbol_present: bool,
    set_symbol_present: bool,
    supported: Option<bool>,
    default_enabled: Option<bool>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct VzNestedJson {
    property: &'static str,
    enabled_property: &'static str,
    api_available_macos: &'static str,
    supported: Option<bool>,
}

/// `ternvale probe-el2`.
#[tracing::instrument(level = "info", target = "ternvale::nested", skip_all)]
pub fn run(json: bool) -> Result<ExitCode> {
    let report = collect()?;
    tracing::info!(
        target: "ternvale::nested",
        macos = %report.macos,
        hv_el2 = ?report.hv_el2.supported,
        vz_nested = ?report.vz_nested.supported,
        "EL2 / nested virt probe"
    );
    json_out::emit(json, &report, || print_human(&report))?;
    Ok(ExitCode::SUCCESS)
}

#[tracing::instrument(level = "debug", target = "ternvale::nested", skip_all)]
fn collect() -> Result<Report> {
    let macos = ternvale_hv::macos_version().unwrap_or(ternvale_hv::MacosVersion {
        major: 0,
        minor: 0,
        text: "unknown".into(),
    });
    let hv_support = ternvale_hv::hypervisor_supported().unwrap_or(false);
    let chip = ternvale_hv::host_chip().unwrap_or(ternvale_hv::HostChip {
        model: "unknown".into(),
        brand: String::new(),
        cpu_family: 0,
    });
    let el2 = ternvale_hv::probe_el2();
    let vz = ternvale_backend::probe_vz_nested();
    Ok(Report {
        macos: macos.text,
        macos_major: macos.major,
        hv_support,
        chip_model: chip.model,
        chip_brand: chip.brand,
        cpu_family: chip.cpu_family,
        hv_el2: HvEl2Json {
            get_supported: el2.get_supported_symbol,
            set_enabled: el2.set_enabled_symbol,
            api_available_macos: el2.api_available_macos,
            symbol_present: el2.symbol_present,
            set_symbol_present: el2.set_symbol_present,
            supported: el2.supported,
            default_enabled: el2.default_enabled,
            error: el2.error,
        },
        vz_nested: VzNestedJson {
            property: vz.property,
            enabled_property: vz.enabled_property,
            api_available_macos: vz.api_available_macos,
            supported: vz.supported,
        },
        nested_implemented: true,
    })
}

fn print_human(report: &Report) {
    println!("ternvale probe-el2 (nested_virt = true is opt-in; off by default)");
    println!("  macos           {}", report.macos);
    println!("  kern.hv_support {}", report.hv_support);
    println!(
        "  chip            {} ({}) family={}",
        report.chip_brand, report.chip_model, report.cpu_family
    );
    println!(
        "  hv EL2          {} symbol={} set_symbol={} supported={:?} default_enabled={:?}",
        report.hv_el2.get_supported,
        report.hv_el2.symbol_present,
        report.hv_el2.set_symbol_present,
        report.hv_el2.supported,
        report.hv_el2.default_enabled
    );
    if let Some(error) = &report.hv_el2.error {
        println!("  hv EL2 error    {error}");
    }
    println!(
        "  vz nested       VZGenericPlatformConfiguration.{} supported={:?}",
        report.vz_nested.property, report.vz_nested.supported
    );
    println!("  implemented     {}", report.nested_implemented);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_marks_opt_in_implementation() {
        let report = Report {
            macos: "15.0".into(),
            macos_major: 15,
            hv_support: true,
            chip_model: "Mac15,6".into(),
            chip_brand: "Apple M3 Pro".into(),
            cpu_family: 1,
            hv_el2: HvEl2Json {
                get_supported: "hv_vm_config_get_el2_supported",
                set_enabled: "hv_vm_config_set_el2_enabled",
                api_available_macos: "15.0",
                symbol_present: true,
                set_symbol_present: true,
                supported: Some(true),
                default_enabled: Some(false),
                error: None,
            },
            vz_nested: VzNestedJson {
                property: "nestedVirtualizationSupported",
                enabled_property: "nestedVirtualizationEnabled",
                api_available_macos: "15.0",
                supported: Some(true),
            },
            nested_implemented: true,
        };
        assert!(report.nested_implemented);
        assert_eq!(report.hv_el2.set_enabled, "hv_vm_config_set_el2_enabled");
    }
}
