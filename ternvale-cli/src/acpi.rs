//! `ternvale acpi-dump`: read Ternvale's ACPI tables back out of a real VM's
//! guest memory into `<out>/ternvale/`. With `--qemu-ram`, also walk the
//! tables in a RAM image saved from QEMU `-M virt` (`pmemsave`) into
//! `<out>/qemu/` and write `<out>/compare.md`, a by-signature diff of the two
//! sets. `scripts/acpi-compare.sh` drives QEMU and this command.

use std::collections::HashSet;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use ternvale_acpi::DumpedTable;

/// Dump, and compare when `qemu_ram` is given. `ram_base` is the guest
/// physical address of the image's first byte.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(out = %out.display(), qemu_ram = ?qemu_ram, ram_base = %format!("{ram_base:#x}"))
)]
pub fn acpi_dump(out: &Path, qemu_ram: Option<&Path>, ram_base: u64) -> Result<ExitCode> {
    let ours = ternvale_vmm::dump_guest_acpi()
        .context("read ternvale's acpi tables back from guest memory")?;
    let ours_dir = out.join("ternvale");
    write_set(&ours_dir, &ours)?;
    println!("ternvale tables ({}):", ours_dir.display());
    print!("{}", listing(&ours));
    let Some(ram) = qemu_ram else {
        return Ok(ExitCode::SUCCESS);
    };
    let theirs = qemu_tables(ram, ram_base)?;
    let qemu_dir = out.join("qemu");
    write_set(&qemu_dir, &theirs)?;
    println!("qemu tables ({}):", qemu_dir.display());
    print!("{}", listing(&theirs));
    let report = comparison(&ours, &theirs);
    let path = out.join("compare.md");
    std::fs::write(&path, &report).with_context(|| format!("write {}", path.display()))?;
    tracing::info!(target: "ternvale::cli", report = %path.display(), "acpi comparison written");
    println!("\n{report}");
    Ok(ExitCode::SUCCESS)
}

/// Write each table to `<dir>/<signature>.dat` (a repeated signature gets
/// `-2`, `-3`, ...) plus `tables.txt`, one line per table.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(dir = %dir.display(), tables = tables.len()))]
pub fn write_set(dir: &Path, tables: &[DumpedTable]) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let mut used = HashSet::new();
    for table in tables {
        let base = table.file_name();
        let mut name = base.clone();
        let mut index = 2;
        while !used.insert(name.clone()) {
            name = base.replace(".dat", &format!("-{index}.dat"));
            index += 1;
        }
        let path = dir.join(&name);
        std::fs::write(&path, &table.bytes).with_context(|| format!("write {}", path.display()))?;
    }
    let path = dir.join("tables.txt");
    std::fs::write(&path, listing(tables)).with_context(|| format!("write {}", path.display()))?;
    tracing::info!(target: "ternvale::cli", dir = %dir.display(), tables = tables.len(), "acpi tables dumped");
    Ok(())
}

/// The tables in a QEMU RAM image whose first byte is at `ram_base`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn qemu_tables(path: &Path, ram_base: u64) -> Result<Vec<DumpedTable>> {
    let image = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    ternvale_acpi::walk_image(&image, ram_base)
        .with_context(|| format!("find acpi tables in {}", path.display()))
}

/// `signature  gpa  length  revision  checksum` per table.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn listing(tables: &[DumpedTable]) -> String {
    let mut out = String::new();
    for t in tables {
        let sum = if t.checksum_ok() { "ok" } else { "BAD" };
        out.push_str(&format!(
            "  {:<7}  {:#012x}  {:>6} B  rev {:<2}  checksum {sum}\n",
            t.signature.trim_end(),
            t.gpa,
            t.bytes.len(),
            t.revision()
        ));
    }
    out
}

/// Markdown: the per-signature table and the two one-sided signature lists.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn comparison(ours: &[DumpedTable], theirs: &[DumpedTable]) -> String {
    let rows = ternvale_acpi::compare(ours, theirs);
    let (only_ours, only_theirs) = ternvale_acpi::signature_diff(&rows);
    let list = |sigs: &[String]| {
        if sigs.is_empty() {
            "none".to_string()
        } else {
            sigs.join(", ")
        }
    };
    format!(
        "{}\nOnly in Ternvale: {}\nOnly in QEMU: {}\n",
        ternvale_acpi::render_markdown(&rows, "Ternvale", "QEMU -M virt"),
        list(&only_ours),
        list(&only_theirs)
    )
}

#[cfg(test)]
#[path = "acpi_tests.rs"]
mod tests;
