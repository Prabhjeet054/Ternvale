//! Step 105 test analog of `tvgpu-dod`: host resize storm, two scanouts,
//! present after mode change, disable/enable. The KMDOD cannot run on macOS.

use super::tests::Fx;
use super::*;
use crate::edid::{self, BLOCK_LEN};
use crate::protocol::*;
use crate::testutil::{self, rect, FORMAT_BGRX, HEAP};

const MAX_SCANOUTS_HOST: u32 = MAX_SCANOUTS as u32;
const MAX_DIM: u32 = MAX_DIMENSION;

/// Same checks as `TvgpuEdidLooksValid` / `TvgpuValidateHostU32` in the KMDOD.
fn guest_accepts_edid(resp: &[u8]) -> [u8; BLOCK_LEN] {
    assert_eq!(testutil::kind(resp), RESP_OK_EDID);
    assert!(resp.len() >= HDR_LEN + 8 + BLOCK_LEN);
    let size = u32::from_le_bytes(resp[HDR_LEN..HDR_LEN + 4].try_into().expect("size"));
    assert!(
        (128..=edid::WIRE_EDID_LEN as u32).contains(&size),
        "edid.size {size}"
    );
    let mut block = [0u8; BLOCK_LEN];
    block.copy_from_slice(&resp[HDR_LEN + 8..HDR_LEN + 8 + BLOCK_LEN]);
    assert_eq!(
        &block[0..8],
        &[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]
    );
    let sum: u8 = block.iter().fold(0u8, |a, b| a.wrapping_add(*b));
    assert_eq!(sum, 0, "EDID checksum");
    block
}

fn guest_modes(info: &[u8], num_scanouts: u32) -> Vec<(u32, u32, bool)> {
    assert_eq!(testutil::kind(info), RESP_OK_DISPLAY_INFO);
    assert_eq!(info.len(), DISPLAY_INFO_LEN);
    assert!(num_scanouts > 0 && num_scanouts <= MAX_SCANOUTS_HOST);
    let mut out = Vec::new();
    for i in 0..MAX_SCANOUTS {
        let at = HDR_LEN + i * 24;
        let width = u32::from_le_bytes(info[at + 8..at + 12].try_into().expect("w"));
        let height = u32::from_le_bytes(info[at + 12..at + 16].try_into().expect("h"));
        let enabled = u32::from_le_bytes(info[at + 16..at + 20].try_into().expect("en")) != 0;
        if i as u32 >= num_scanouts {
            assert!(!enabled, "host enabled scanout {i} beyond num_scanouts");
            continue;
        }
        if enabled {
            assert!(width > 0 && height > 0 && width <= MAX_DIM && height <= MAX_DIM);
        }
        out.push((width, height, enabled));
    }
    out
}

fn refresh(fx: &mut Fx, num_scanouts: u32) -> Vec<(u32, u32)> {
    let info = fx.answer(&testutil::get_display_info());
    let modes = guest_modes(&info, num_scanouts);
    let mut enabled = Vec::new();
    for (id, (w, h, on)) in modes.into_iter().enumerate() {
        if !on {
            continue;
        }
        let edid = fx.answer(&testutil::get_edid(id as u32));
        let block = guest_accepts_edid(&edid);
        assert_eq!(block, edid::block_for_mode(w, h));
        enabled.push((w, h));
    }
    enabled
}

fn present(fx: &mut Fx, scanout: u32, rid: u32, w: u32, h: u32) {
    let bytes = (w * h * 4) as usize;
    let pixels = vec![0x40u8; bytes];
    testutil::fill_backing(&fx.memory, &[(HEAP, bytes as u32)], &pixels);
    assert_eq!(
        fx.send(&testutil::create_2d(rid, FORMAT_BGRX, w, h)),
        RESP_OK_NODATA
    );
    assert_eq!(
        fx.send(&testutil::attach_backing(rid, &[(HEAP, bytes as u32)])),
        RESP_OK_NODATA
    );
    assert_eq!(
        fx.send(&testutil::transfer(rect(0, 0, w, h), 0, rid)),
        RESP_OK_NODATA
    );
    assert_eq!(
        fx.send(&testutil::set_scanout(rect(0, 0, w, h), scanout, rid)),
        RESP_OK_NODATA
    );
    assert_eq!(
        fx.send(&testutil::flush(rect(0, 0, w, h), rid)),
        RESP_OK_NODATA
    );
}

fn disable(fx: &mut Fx, scanout: u32, rid: u32) {
    assert_eq!(
        fx.send(&testutil::set_scanout(rect(0, 0, 0, 0), scanout, 0)),
        RESP_OK_NODATA
    );
    assert_eq!(fx.send(&testutil::unref(rid)), RESP_OK_NODATA);
}

#[test]
fn hundreds_of_host_resizes_update_edid_without_panic() {
    let mut fx = Fx::new(&[(64, 32)]);
    let cycle = [(64, 32), (80, 48), (48, 48), (96, 16)];
    for i in 0..400 {
        let (w, h) = cycle[i % cycle.len()];
        fx.display.request_host_mode(0, w, h).expect("resize");
        let enabled = refresh(&mut fx, 1);
        assert_eq!(enabled, vec![(w, h)]);
        present(&mut fx, 0, 1, w, h);
        disable(&mut fx, 0, 1);
    }
    tracing::info!(
        target: "ternvale::gpu",
        iterations = 400,
        "tvgpu-dod resize storm analog finished"
    );
}

#[test]
fn two_scanouts_resize_independently_and_present() {
    let mut fx = Fx::new(&[(32, 16), (48, 24)]);
    let enabled = refresh(&mut fx, 2);
    assert_eq!(enabled, vec![(32, 16), (48, 24)]);
    present(&mut fx, 0, 1, 32, 16);
    present(&mut fx, 1, 2, 48, 24);
    fx.display.request_host_mode(1, 40, 20).expect("scanout 1");
    let after = refresh(&mut fx, 2);
    assert_eq!(after[0], (32, 16), "scanout 0 unchanged");
    assert_eq!(after[1], (40, 20));
    disable(&mut fx, 1, 2);
    present(&mut fx, 1, 3, 40, 20);
    disable(&mut fx, 0, 1);
    disable(&mut fx, 1, 3);
}

#[test]
fn disable_enable_is_d3_d0_analog() {
    let mut fx = Fx::new(&[(32, 16)]);
    present(&mut fx, 0, 1, 32, 16);
    disable(&mut fx, 0, 1);
    present(&mut fx, 0, 2, 32, 16);
    disable(&mut fx, 0, 2);
}

#[test]
fn host_rejects_edid_for_scanout_past_num_scanouts() {
    let mut fx = Fx::new(&[(32, 16)]);
    let out = fx.answer(&testutil::get_edid(7));
    assert_eq!(testutil::kind(&out), RESP_ERR_INVALID_SCANOUT_ID);
}
