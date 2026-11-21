//! `ternvale print list` — printers shared with a VM.

use std::process::ExitCode;

use anyhow::{Context, Result};
use ternvale_config::VmConfig;
use ternvale_devices::{list_host_queues, noop_open, PrintPolicy};

use crate::json_out;
use crate::protocol::{PrinterJson, Response};

/// `ternvale print list <vm>`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm))]
pub fn list(vm: &str, json: bool) -> Result<ExitCode> {
    let response = list_response(vm)?;
    emit(json, &response, || {
        if let Some(printers) = &response.printers {
            if printers.is_empty() {
                println!("printing off (set [print] enabled = true)");
            }
            for printer in printers {
                println!("{}\t{}\t{}", printer.id, printer.name, printer.kind);
            }
        }
    })
}

/// Hub `print.list`.
#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all, fields(vm))]
pub fn list_response(vm: &str) -> Result<Response> {
    let path = crate::paths::vm_config_path(vm)?;
    let config = VmConfig::from_path(&path).with_context(|| format!("load {}", path.display()))?;
    let Some(section) = &config.print else {
        let mut response = Response::ok();
        response.printers = Some(Vec::new());
        return Ok(response);
    };
    if !section.enabled {
        tracing::info!(target: "ternvale::print", vm, "print opt-in is off");
        let mut response = Response::ok();
        response.printers = Some(Vec::new());
        return Ok(response);
    }
    let policy = PrintPolicy::from_config(
        vm,
        section,
        &list_host_queues(),
        std::env::temp_dir(),
        noop_open(),
    );
    let mut response = Response::ok();
    response.printers = Some(
        policy
            .guest_printers()
            .into_iter()
            .map(|p| PrinterJson {
                id: p.id,
                name: p.name,
                kind: p.kind,
            })
            .collect(),
    );
    Ok(response)
}

fn emit(json: bool, response: &Response, human: impl FnOnce()) -> Result<ExitCode> {
    if !response.ok {
        json_out::emit(
            json,
            serde_json::to_value(response).unwrap_or(serde_json::json!({"ok":false})),
            || {},
        )?;
        if let Some(error) = &response.error {
            if !json {
                eprintln!("{error}");
            }
        }
        return Ok(ExitCode::FAILURE);
    }
    json_out::emit(
        json,
        serde_json::to_value(response).unwrap_or(serde_json::json!({"ok":true})),
        human,
    )?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use ternvale_config::PrintSection;
    use ternvale_devices::{noop_open, PrintPolicy};

    #[test]
    fn empty_when_disabled() {
        let section = PrintSection::default();
        let policy = PrintPolicy::from_config(
            "lab",
            &section,
            &["Office".into()],
            std::env::temp_dir(),
            noop_open(),
        );
        assert!(policy.guest_printers().is_empty());
    }
}
