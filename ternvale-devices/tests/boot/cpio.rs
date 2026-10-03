//! A minimal `newc` cpio writer, to append files to an existing initramfs.
//!
//! The kernel unpacks concatenated archives in order and a later entry
//! replaces an earlier one with the same name
//! (Documentation/driver-api/early-userspace/buffer-format.rst), so
//! `base + archive(files)` overrides files in `base`. Each header is the
//! `070701` magic plus 13 eight-digit hex fields; names and data are padded
//! to 4 bytes, and the archive ends with a `TRAILER!!!` entry.

const MAGIC: &str = "070701";
const REGULAR: u32 = 0o100_000;

/// `base` (padded to 4 bytes) followed by a new archive holding `files`
/// (`name`, permission bits, contents).
pub fn append(base: &[u8], files: &[(&str, u32, &[u8])]) -> Result<Vec<u8>, String> {
    let mut out = base.to_vec();
    pad(&mut out);
    for (index, (name, perm, data)) in files.iter().enumerate() {
        let ino = 0x7e00_0000 + index as u32;
        entry(&mut out, ino, REGULAR | perm, name, data)?;
    }
    entry(&mut out, 0, 0, "TRAILER!!!", &[])?;
    tracing::debug!(
        target: "ternvale::boot",
        base = base.len(),
        files = files.len(),
        bytes = out.len(),
        "initramfs extended"
    );
    Ok(out)
}

fn entry(out: &mut Vec<u8>, ino: u32, mode: u32, name: &str, data: &[u8]) -> Result<(), String> {
    let size = u32::try_from(data.len()).map_err(|_| format!("{name} is over 4 GiB"))?;
    let name_size = name.len() as u32 + 1;
    let nlink = 1;
    let fields = [ino, mode, 0, 0, nlink, 0, size, 0, 0, 0, 0, name_size, 0];
    out.extend_from_slice(MAGIC.as_bytes());
    for field in fields {
        out.extend_from_slice(format!("{field:08x}").as_bytes());
    }
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    pad(out);
    out.extend_from_slice(data);
    pad(out);
    Ok(())
}

fn pad(out: &mut Vec<u8>) {
    out.resize(out.len().next_multiple_of(4), 0);
}
