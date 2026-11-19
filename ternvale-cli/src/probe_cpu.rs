//! `ternvale probe-cpu`: dump Hypervisor.framework feature registers.

use std::collections::BTreeMap;
use std::process::ExitCode;

use anyhow::{Context, Result};
use serde::Serialize;
use ternvale_hv::{host_chip, hypervisor_supported, macos_version, read_host_features};

use crate::json_out;

#[derive(Debug, Serialize)]
struct Report {
    macos: String,
    hv_support: bool,
    chip_model: String,
    chip_brand: String,
    /// `hv_vcpu_config_get_feature_reg` — get only; no setter in the SDK.
    get_feature_reg: &'static str,
    set_feature_reg: Option<&'static str>,
    api_available_macos: &'static str,
    masking: &'static str,
    registers: BTreeMap<String, Option<String>>,
}

/// `ternvale probe-cpu`.
#[tracing::instrument(level = "info", target = "ternvale::cpuprofile", skip_all)]
pub fn run(json: bool) -> Result<ExitCode> {
    let report = collect().context("read host CPU feature registers")?;
    tracing::info!(
        target: "ternvale::cpuprofile",
        chip = %report.chip_model,
        regs = report.registers.len(),
        "CPU feature probe"
    );
    json_out::emit(json, &report, || print_human(&report))?;
    Ok(ExitCode::SUCCESS)
}

fn print_human(report: &Report) {
    println!("ternvale probe-cpu");
    println!("  macos           {}", report.macos);
    println!("  kern.hv_support {}", report.hv_support);
    println!(
        "  chip            {} ({})",
        report.chip_brand, report.chip_model
    );
    println!(
        "  get             {} (API_AVAILABLE macos({}))",
        report.get_feature_reg, report.api_available_macos
    );
    println!(
        "  set             {}",
        report
            .set_feature_reg
            .unwrap_or("none (header has get only)")
    );
    println!("  masking         {}", report.masking);
    for (name, value) in &report.registers {
        match value {
            Some(hex) => println!("  {name:<20} {hex}"),
            None => println!("  {name:<20} (unavailable)"),
        }
    }
}

fn collect() -> Result<Report> {
    let macos = macos_version().unwrap_or(ternvale_hv::MacosVersion {
        major: 0,
        minor: 0,
        text: "unknown".into(),
    });
    let chip = host_chip().unwrap_or(ternvale_hv::HostChip {
        model: "unknown".into(),
        brand: String::new(),
        cpu_family: 0,
    });
    let rows = read_host_features().context("hv_vcpu_config_get_feature_reg")?;
    let mut registers = BTreeMap::new();
    for row in rows {
        registers.insert(
            row.reg.name().to_string(),
            row.value.map(|v| format!("{v:#x}")),
        );
    }
    Ok(Report {
        macos: macos.text,
        hv_support: hypervisor_supported().unwrap_or(false),
        chip_model: chip.model,
        chip_brand: chip.brand,
        get_feature_reg: "hv_vcpu_config_get_feature_reg",
        set_feature_reg: None,
        api_available_macos: "11.0",
        masking:
            "no hv_vcpu_config_set_feature_reg; baseline tries hv_vcpu_set_sys_reg on ID_AA64*",
        registers,
    })
}
