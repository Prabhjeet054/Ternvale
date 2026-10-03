//! A minimal FAT16 image holding a few files in the root directory, for UEFI
//! to load from a virtio-blk disk: either without a partition table (a
//! "superfloppy", which EDK2's `FatPkg/EnhancedFatDxe` mounts directly) or
//! as the one partition of an MBR disk. Linux prints ` vda: vda1` only for the
//! partitioned form, after reading the MBR, which needs a completed request.
//!
//! Layout per Microsoft "FAT: General Overview of On-Disk Format" v1.03
//! (fatgen103): BPB in sector 0, two FATs, a 512-entry root directory, then
//! 4 KiB clusters. Each file takes a contiguous run of clusters. FAT16 needs
//! 4085 to 65524 clusters, so the volume is padded up to the minimum. The
//! timestamps are fixed, so the same files always give the same image.

use std::path::Path;

const SECTOR: usize = 512;
const SECTORS_PER_CLUSTER: usize = 8;
const CLUSTER: usize = SECTOR * SECTORS_PER_CLUSTER;
const RESERVED_SECTORS: usize = 1;
const FATS: usize = 2;
const ROOT_ENTRIES: usize = 512;
const DIR_ENTRY: usize = 32;
const MIN_CLUSTERS: usize = 4200;
const MAX_CLUSTERS: usize = 65_524;
/// 2026-01-01 in the FAT date format (years since 1980, month, day).
const DATE: u16 = ((2026 - 1980) << 9) | (1 << 5) | 1;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_ARCHIVE: u8 = 0x20;
const END_OF_CHAIN: u16 = 0xffff;
/// First sector of the partition in the MBR form (1 MiB, the usual alignment).
const PARTITION_LBA: usize = 2048;
/// MBR partition type: FAT16 with LBA addressing.
const MBR_FAT16_LBA: u8 = 0x0e;

/// Write `files` (`8.3` name, contents) as an unpartitioned FAT16 image at `path`.
pub fn write_fat16(path: &Path, files: &[(&str, &[u8])]) -> Result<(), String> {
    let image = volume(files, 0)?;
    write(path, &image, files, false)
}

/// Write `files` as a FAT16 partition starting at sector 2048 of an MBR disk.
pub fn write_fat16_mbr(path: &Path, files: &[(&str, &[u8])]) -> Result<(), String> {
    let volume = volume(files, PARTITION_LBA)?;
    let sectors = u32::try_from(volume.len() / SECTOR).map_err(|_| "volume too large")?;
    let mut image = vec![0u8; PARTITION_LBA * SECTOR];
    // Classic MBR: disk signature at 440, four 16-byte entries from 446, 55 AA.
    image[440..444].copy_from_slice(&0x5445_524eu32.to_le_bytes());
    let entry = &mut image[446..462];
    entry[1..4].copy_from_slice(&[0xfe, 0xff, 0xff]);
    entry[4] = MBR_FAT16_LBA;
    entry[5..8].copy_from_slice(&[0xfe, 0xff, 0xff]);
    entry[8..12].copy_from_slice(&(PARTITION_LBA as u32).to_le_bytes());
    entry[12..16].copy_from_slice(&sectors.to_le_bytes());
    image[510] = 0x55;
    image[511] = 0xaa;
    image.extend_from_slice(&volume);
    write(path, &image, files, true)
}

fn write(path: &Path, image: &[u8], files: &[(&str, &[u8])], mbr: bool) -> Result<(), String> {
    std::fs::write(path, image).map_err(|err| format!("write {}: {err}", path.display()))?;
    tracing::info!(
        target: "ternvale::boot",
        path = %path.display(),
        bytes = image.len(),
        mbr,
        files = %files.iter().map(|(name, bytes)| format!("{name}={}", bytes.len())).collect::<Vec<_>>().join(","),
        "fat16 esp image written"
    );
    Ok(())
}

/// The FAT16 volume; `hidden` is its first sector on the disk (BPB_HiddSec).
fn volume(files: &[(&str, &[u8])], hidden: usize) -> Result<Vec<u8>, String> {
    if files.len() + 1 > ROOT_ENTRIES {
        return Err(format!(
            "{} files do not fit the root directory",
            files.len()
        ));
    }
    let used: usize = files
        .iter()
        .map(|(_, bytes)| bytes.len().div_ceil(CLUSTER))
        .sum();
    let clusters = (used + 64).max(MIN_CLUSTERS);
    if clusters > MAX_CLUSTERS {
        return Err(format!("{used} clusters of files are too many for FAT16"));
    }
    let fat_sectors = ((clusters + 2) * 2).div_ceil(SECTOR);
    let root_sectors = ROOT_ENTRIES * DIR_ENTRY / SECTOR;
    let data_start = RESERVED_SECTORS + FATS * fat_sectors + root_sectors;
    let total_sectors = data_start + clusters * SECTORS_PER_CLUSTER;
    let mut image = vec![0u8; total_sectors * SECTOR];
    boot_sector(&mut image[..SECTOR], total_sectors, fat_sectors, hidden)?;

    let mut fat = vec![0u16; clusters + 2];
    fat[0] = 0xfff8;
    fat[1] = END_OF_CHAIN;
    let mut root = Vec::with_capacity(ROOT_ENTRIES * DIR_ENTRY);
    root.extend_from_slice(&dir_entry(*b"TERNVALE   ", ATTR_VOLUME_ID, 0, 0));
    let mut next = 2usize;
    for (name, bytes) in files {
        let count = bytes.len().div_ceil(CLUSTER);
        let first = if count == 0 { 0 } else { next };
        let end = next + count;
        for (cluster, entry) in fat.iter_mut().enumerate().take(end).skip(next) {
            *entry = if cluster + 1 == end {
                END_OF_CHAIN
            } else {
                (cluster + 1) as u16
            };
        }
        let offset = (data_start + (next - 2) * SECTORS_PER_CLUSTER) * SECTOR;
        image[offset..offset + bytes.len()].copy_from_slice(bytes);
        let size = u32::try_from(bytes.len()).map_err(|_| format!("{name} is over 4 GiB"))?;
        root.extend_from_slice(&dir_entry(
            short_name(name)?,
            ATTR_ARCHIVE,
            first as u16,
            size,
        ));
        next += count;
    }
    for copy in 0..FATS {
        let start = (RESERVED_SECTORS + copy * fat_sectors) * SECTOR;
        for (index, entry) in fat.iter().enumerate() {
            image[start + index * 2..start + index * 2 + 2].copy_from_slice(&entry.to_le_bytes());
        }
    }
    let root_start = (RESERVED_SECTORS + FATS * fat_sectors) * SECTOR;
    image[root_start..root_start + root.len()].copy_from_slice(&root);
    tracing::debug!(target: "ternvale::boot", clusters, hidden, "fat16 volume built");
    Ok(image)
}

/// The BIOS parameter block and FAT16 extended boot record (fatgen103 §3).
fn boot_sector(
    sector: &mut [u8],
    total_sectors: usize,
    fat_sectors: usize,
    hidden: usize,
) -> Result<(), String> {
    let fat_sectors = u16::try_from(fat_sectors).map_err(|_| "FAT too large".to_string())?;
    let hidden = u32::try_from(hidden).map_err(|_| "partition offset too large".to_string())?;
    sector[28..32].copy_from_slice(&hidden.to_le_bytes());
    sector[..3].copy_from_slice(&[0xeb, 0x3c, 0x90]);
    sector[3..11].copy_from_slice(b"TERNVALE");
    sector[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    sector[13] = SECTORS_PER_CLUSTER as u8;
    sector[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
    sector[16] = FATS as u8;
    sector[17..19].copy_from_slice(&(ROOT_ENTRIES as u16).to_le_bytes());
    match u16::try_from(total_sectors) {
        Ok(small) => sector[19..21].copy_from_slice(&small.to_le_bytes()),
        Err(_) => sector[32..36].copy_from_slice(&(total_sectors as u32).to_le_bytes()),
    }
    sector[21] = 0xf8;
    sector[22..24].copy_from_slice(&fat_sectors.to_le_bytes());
    sector[24..26].copy_from_slice(&32u16.to_le_bytes());
    sector[26..28].copy_from_slice(&64u16.to_le_bytes());
    sector[36] = 0x80;
    sector[38] = 0x29;
    sector[39..43].copy_from_slice(&0x5445_524eu32.to_le_bytes());
    sector[43..54].copy_from_slice(b"TERNVALE   ");
    sector[54..62].copy_from_slice(b"FAT16   ");
    sector[510] = 0x55;
    sector[511] = 0xaa;
    Ok(())
}

fn dir_entry(name: [u8; 11], attr: u8, first_cluster: u16, size: u32) -> [u8; DIR_ENTRY] {
    let mut entry = [0u8; DIR_ENTRY];
    entry[..11].copy_from_slice(&name);
    entry[11] = attr;
    for at in [16, 18, 24] {
        entry[at..at + 2].copy_from_slice(&DATE.to_le_bytes());
    }
    entry[26..28].copy_from_slice(&first_cluster.to_le_bytes());
    entry[28..32].copy_from_slice(&size.to_le_bytes());
    entry
}

/// `NAME.EXT` as the space-padded 11-byte directory name. Upper-case letters,
/// digits, and `_` only, so no long-name entry is needed.
fn short_name(name: &str) -> Result<[u8; 11], String> {
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    let allowed = |b: u8| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_';
    if base.is_empty()
        || base.len() > 8
        || ext.len() > 3
        || !base.bytes().chain(ext.bytes()).all(allowed)
    {
        return Err(format!("{name:?} is not an upper-case 8.3 name"));
    }
    let mut out = [b' '; 11];
    out[..base.len()].copy_from_slice(base.as_bytes());
    out[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    Ok(out)
}
