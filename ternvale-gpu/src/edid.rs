//! EDID 1.3 block for one virtio-gpu scanout (virtio 1.2 §5.7.6.8 `GET_EDID`).
//!
//! The 128-byte base block advertises a single preferred detailed timing for
//! the requested size at 60 Hz. Checksum is byte 127. TODO(verify): virtio-win
//! `viogpudo` honours `VIRTIO_GPU_F_EDID` / `VIRTIO_GPU_EVENT_DISPLAY` on the
//! ARM64 build shipped with this tree (0.1.302).

/// Bytes in an EDID 1.3 base block.
pub const BLOCK_LEN: usize = 128;
/// `struct virtio_gpu_resp_edid` payload after the control header: `size`,
/// padding, then `edid[1024]` (Linux `virtio_gpu.h`).
pub const WIRE_EDID_LEN: usize = 1024;

/// Build a 128-byte EDID 1.3 for `width` × `height` at 60 Hz.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::gpu",
    skip_all,
    fields(width, height)
)]
pub fn block_for_mode(width: u32, height: u32) -> [u8; BLOCK_LEN] {
    let mut edid = [0u8; BLOCK_LEN];
    edid[0..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
    // Manufacturer "TVL" packed as three 5-bit letters (A=1).
    edid[8..10].copy_from_slice(&0x4d_b5u16.to_be_bytes());
    edid[10..12].copy_from_slice(&0x0001u16.to_le_bytes());
    edid[16] = 1;
    edid[17] = 36; // year 2026
    edid[18] = 1;
    edid[19] = 3;
    edid[20] = 0x80; // digital input
    edid[23] = 0x78; // gamma 2.2
    edid[24] = 0x0a; // sRGB + preferred timing
                     // sRGB chromaticity (common template).
    edid[25..35].copy_from_slice(&[0xee, 0x91, 0xa3, 0x54, 0x4c, 0x99, 0x26, 0x0f, 0x50, 0x54]);
    for i in 0..8 {
        edid[38 + i * 2] = 0x01;
        edid[39 + i * 2] = 0x01;
    }
    write_dtd(&mut edid[54..72], width.max(1), height.max(1));
    write_name(&mut edid[72..90]);
    edid[127] = checksum(&edid[..127]);
    tracing::debug!(
        target: "ternvale::gpu",
        width,
        height,
        checksum = format!("{:#04x}", edid[127]),
        "generated EDID 1.3 block"
    );
    edid
}

/// Pad `block` into the 1024-byte virtio-gpu EDID buffer.
#[tracing::instrument(level = "trace", target = "ternvale::gpu", skip_all)]
pub fn wire_edid(block: &[u8; BLOCK_LEN]) -> [u8; WIRE_EDID_LEN] {
    let mut out = [0u8; WIRE_EDID_LEN];
    out[..BLOCK_LEN].copy_from_slice(block);
    out
}

fn write_dtd(out: &mut [u8], width: u32, height: u32) {
    let hactive = width.min(4095);
    let vactive = height.min(4095);
    let hblank = 160u32;
    let vblank = 45u32;
    let htotal = hactive.saturating_add(hblank);
    let vtotal = vactive.saturating_add(vblank);
    let clock = ((u64::from(htotal) * u64::from(vtotal) * 60) / 10_000).min(u64::from(u16::MAX));
    let clock = clock as u16;
    out[0..2].copy_from_slice(&clock.to_le_bytes());
    out[2] = (hactive & 0xff) as u8;
    out[3] = (hblank & 0xff) as u8;
    out[4] = (((hactive >> 8) & 0xf) << 4 | ((hblank >> 8) & 0xf)) as u8;
    out[5] = (vactive & 0xff) as u8;
    out[6] = (vblank & 0xff) as u8;
    out[7] = (((vactive >> 8) & 0xf) << 4 | ((vblank >> 8) & 0xf)) as u8;
    out[8] = 48; // hsync offset
    out[9] = 32; // hsync width
    out[10] = (10 << 4) | 5; // vsync offset / width (low)
    out[11] = 0;
    let hmm = (hactive * 16 / 96).min(4095); // ~96 dpi mm
    let vmm = (vactive * 16 / 96).min(4095);
    out[12] = (hmm & 0xff) as u8;
    out[13] = (vmm & 0xff) as u8;
    out[14] = (((hmm >> 8) & 0xf) << 4 | ((vmm >> 8) & 0xf)) as u8;
    out[17] = 0x1e; // digital separate, +hsync +vsync
}

fn write_name(out: &mut [u8]) {
    out[0..5].copy_from_slice(&[0x00, 0x00, 0x00, 0xfc, 0x00]);
    let name = b"Ternvale";
    out[5..5 + name.len()].copy_from_slice(name);
    out[5 + name.len()] = 0x0a;
    for b in &mut out[6 + name.len()..] {
        *b = 0x20;
    }
}

fn checksum(bytes: &[u8]) -> u8 {
    let sum: u8 = bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b));
    0u8.wrapping_sub(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_checksum_and_preferred_timing_match_the_mode() {
        let edid = block_for_mode(1920, 1080);
        assert_eq!(
            &edid[0..8],
            &[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]
        );
        let sum: u8 = edid.iter().fold(0u8, |a, b| a.wrapping_add(*b));
        assert_eq!(sum, 0, "EDID checksum");
        let h = u32::from(edid[54 + 2]) | (u32::from(edid[54 + 4] >> 4) << 8);
        let v = u32::from(edid[54 + 5]) | (u32::from(edid[54 + 7] >> 4) << 8);
        assert_eq!((h, v), (1920, 1080));
        assert_eq!(edid[18], 1);
        assert_eq!(edid[19], 3);
    }

    #[test]
    fn wire_buffer_is_1024_bytes_with_block_at_the_front() {
        let block = block_for_mode(1280, 800);
        let wire = wire_edid(&block);
        assert_eq!(wire.len(), WIRE_EDID_LEN);
        assert_eq!(&wire[..BLOCK_LEN], &block);
        assert!(wire[BLOCK_LEN..].iter().all(|b| *b == 0));
    }
}
