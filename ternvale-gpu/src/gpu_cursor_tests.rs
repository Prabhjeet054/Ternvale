use super::*;
use crate::gpu2d::Gpu2d;
use crate::gpu2d::GpuStats;
use crate::protocol::{self, CMD_MOVE_CURSOR, CMD_UPDATE_CURSOR, HDR_LEN, RESP_OK_NODATA};
use crate::resource::{PixelFormat, Resource};
use crate::scanout::GpuDisplay;
use std::sync::Arc;

fn gpu() -> Gpu2d {
    Gpu2d::new(
        GpuDisplay::new(&[(640, 480)]),
        Arc::new(GpuStats::default()),
    )
}

fn cursor_bytes(kind: u32, resource_id: u32, hot: (u32, u32), pos: (u32, u32)) -> Vec<u8> {
    let mut out = vec![0u8; HDR_LEN + 32];
    out[0..4].copy_from_slice(&kind.to_le_bytes());
    out[HDR_LEN..HDR_LEN + 4].copy_from_slice(&0u32.to_le_bytes());
    out[HDR_LEN + 4..HDR_LEN + 8].copy_from_slice(&pos.0.to_le_bytes());
    out[HDR_LEN + 8..HDR_LEN + 12].copy_from_slice(&pos.1.to_le_bytes());
    out[HDR_LEN + 16..HDR_LEN + 20].copy_from_slice(&resource_id.to_le_bytes());
    out[HDR_LEN + 20..HDR_LEN + 24].copy_from_slice(&hot.0.to_le_bytes());
    out[HDR_LEN + 24..HDR_LEN + 28].copy_from_slice(&hot.1.to_le_bytes());
    out
}

#[test]
fn update_cursor_copies_resource_pixels_into_overlay() {
    let mut gpu = gpu();
    let mut resource = Resource::new(PixelFormat::B8g8r8a8, 8, 8);
    resource.pixels = (0..256).map(|i| i as u8).collect();
    gpu.insert_resource_for_test(1, resource);
    let response = handle(
        &mut gpu,
        &cursor_bytes(CMD_UPDATE_CURSOR, 1, (2, 3), (10, 20)),
    );
    assert_eq!(&response[0..4], &RESP_OK_NODATA.to_le_bytes());
    let overlay = gpu.display().cursor();
    assert!(overlay.visible);
    assert_eq!(overlay.width, 8);
    assert_eq!(overlay.hot_x, 2);
    assert_eq!(overlay.hot_y, 3);
    assert_eq!(overlay.x, 10);
    assert_eq!(overlay.pixels.len(), 64 * 4);
    assert_eq!(overlay.pixels[0], 0);
}

#[test]
fn resource_zero_hides_the_overlay() {
    let mut gpu = gpu();
    handle(
        &mut gpu,
        &cursor_bytes(CMD_UPDATE_CURSOR, 0, (0, 0), (0, 0)),
    );
    let overlay = gpu.display().cursor();
    assert!(!overlay.visible);
    assert_eq!(overlay.width, 0);
}

#[test]
fn move_cursor_keeps_pixels() {
    let mut gpu = gpu();
    let resource = Resource::new(PixelFormat::B8g8r8a8, 4, 4);
    gpu.insert_resource_for_test(3, resource);
    handle(
        &mut gpu,
        &cursor_bytes(CMD_UPDATE_CURSOR, 3, (1, 1), (0, 0)),
    );
    handle(
        &mut gpu,
        &cursor_bytes(CMD_MOVE_CURSOR, 0, (0, 0), (40, 50)),
    );
    let overlay = gpu.display().cursor();
    assert!(overlay.visible);
    assert_eq!((overlay.x, overlay.y), (40, 50));
    assert_eq!(overlay.width, 4);
}

#[test]
fn missing_resource_is_warned_and_acked() {
    let mut gpu = gpu();
    let response = handle(
        &mut gpu,
        &cursor_bytes(CMD_UPDATE_CURSOR, 99, (0, 0), (0, 0)),
    );
    assert_eq!(&response[0..4], &RESP_OK_NODATA.to_le_bytes());
    assert!(!gpu.display().cursor().visible);
}

#[test]
fn parse_cursor_body() {
    let bytes = cursor_bytes(CMD_UPDATE_CURSOR, 7, (1, 2), (3, 4));
    let (_, command) = protocol::parse(&bytes).expect("parse");
    assert_eq!(
        command,
        Command::Cursor {
            scanout_id: 0,
            x: 3,
            y: 4,
            resource_id: 7,
            hot_x: 1,
            hot_y: 2,
            update: true,
        }
    );
}
