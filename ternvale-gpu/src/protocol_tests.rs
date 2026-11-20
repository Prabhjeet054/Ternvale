use super::*;
use crate::testutil::{self, rect};

#[test]
fn every_2d_opcode_parses_from_wire_bytes() {
    let cases = [
        (testutil::get_display_info(), Command::GetDisplayInfo),
        (testutil::get_edid(1), Command::GetEdid { scanout_id: 1 }),
        (
            testutil::create_2d(7, 2, 640, 480),
            Command::ResourceCreate2d {
                resource_id: 7,
                format: 2,
                width: 640,
                height: 480,
            },
        ),
        (
            testutil::unref(7),
            Command::ResourceUnref { resource_id: 7 },
        ),
        (
            testutil::set_scanout(rect(1, 2, 3, 4), 1, 7),
            Command::SetScanout {
                rect: rect(1, 2, 3, 4),
                scanout_id: 1,
                resource_id: 7,
            },
        ),
        (
            testutil::flush(rect(0, 0, 640, 480), 7),
            Command::ResourceFlush {
                rect: rect(0, 0, 640, 480),
                resource_id: 7,
            },
        ),
        (
            testutil::transfer(rect(8, 9, 10, 11), 0x1_0000_0040, 7),
            Command::TransferToHost2d {
                rect: rect(8, 9, 10, 11),
                offset: 0x1_0000_0040,
                resource_id: 7,
            },
        ),
        (
            testutil::attach_backing(7, &[(0x4000_4000, 4096), (0x1_2345_6000, 512)]),
            Command::ResourceAttachBacking {
                resource_id: 7,
                entries: vec![
                    MemEntry {
                        addr: 0x4000_4000,
                        len: 4096,
                    },
                    MemEntry {
                        addr: 0x1_2345_6000,
                        len: 512,
                    },
                ],
            },
        ),
        (
            testutil::detach_backing(7),
            Command::ResourceDetachBacking { resource_id: 7 },
        ),
    ];
    for (bytes, want) in cases {
        let (hdr, command) = parse(&bytes).expect("parse");
        assert_eq!(command, want, "{}", type_name(hdr.kind));
    }
}

#[test]
fn header_fields_parse_and_unknown_types_are_unsupported() {
    let mut bytes = testutil::hdr(0x0111, FLAG_FENCE, 0xdead_beef_0102);
    bytes[16..20].copy_from_slice(&5u32.to_le_bytes());
    bytes[20] = 3;
    let (hdr, command) = parse(&bytes).expect("parse");
    assert_eq!(
        hdr,
        CtrlHdr {
            kind: 0x0111,
            flags: FLAG_FENCE,
            fence_id: 0xdead_beef_0102,
            ctx_id: 5,
            ring_idx: 3
        }
    );
    assert_eq!(command, Command::Unsupported);
    assert_eq!(type_name(0x0109), "GET_CAPSET");
    assert_eq!(type_name(0x0204), "3D command");
    assert_eq!(type_name(0xffff), "unknown");
}

#[test]
fn short_requests_are_rejected_not_read_past() {
    assert_eq!(parse(&[0u8; 10]), Err(ParseError::NoHeader { len: 10 }));
    let mut bytes = testutil::create_2d(1, 2, 3, 4);
    bytes.truncate(HDR_LEN + 15);
    assert_eq!(
        parse(&bytes),
        Err(ParseError::Short {
            name: "RESOURCE_CREATE_2D",
            need: HDR_LEN + 16,
            len: HDR_LEN + 15
        })
    );
    // nr_entries says 2, only one entry follows.
    let mut bytes = testutil::attach_backing(1, &[(0x1000, 16), (0x2000, 16)]);
    bytes.truncate(bytes.len() - MEM_ENTRY_LEN);
    assert!(matches!(
        parse(&bytes),
        Err(ParseError::Short {
            name: "RESOURCE_ATTACH_BACKING",
            ..
        })
    ));
}

#[test]
fn attach_backing_entry_count_is_capped_before_allocating() {
    let mut bytes = testutil::attach_backing(1, &[]);
    bytes[HDR_LEN + 4..HDR_LEN + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        parse(&bytes),
        Err(ParseError::TooManyEntries {
            entries: u32::MAX,
            max: MAX_BACKING_ENTRIES
        })
    );
}

#[test]
fn responses_echo_the_fence_only_when_asked() {
    let fenced = CtrlHdr {
        kind: CMD_RESOURCE_FLUSH,
        flags: FLAG_FENCE,
        fence_id: 42,
        ctx_id: 9,
        ring_idx: 0,
    };
    let out = response_hdr(&fenced, RESP_OK_NODATA);
    let back = parse_hdr(&out).expect("hdr");
    assert_eq!(
        back,
        CtrlHdr {
            kind: RESP_OK_NODATA,
            flags: FLAG_FENCE,
            fence_id: 42,
            ctx_id: 9,
            ring_idx: 0
        }
    );
    let plain = CtrlHdr {
        fence_id: 42,
        ..CtrlHdr::default()
    };
    let back = parse_hdr(&response_hdr(&plain, RESP_ERR_UNSPEC)).expect("hdr");
    assert_eq!(
        back,
        CtrlHdr {
            kind: RESP_ERR_UNSPEC,
            ..CtrlHdr::default()
        }
    );
}

#[test]
fn rect_union_covers_both_regions() {
    let a = rect(10, 5, 4, 3);
    let b = rect(12, 6, 8, 2);
    let u = a.union(b);
    assert_eq!(u, rect(10, 5, 10, 3));
    assert_eq!(Rect::default().union(a), a);
}

#[test]
fn display_info_lists_each_mode_enabled_and_the_rest_empty() {
    let out = display_info(&CtrlHdr::default(), &[(1280, 800), (640, 480)]);
    assert_eq!(out.len(), DISPLAY_INFO_LEN);
    assert_eq!(parse_hdr(&out).expect("hdr").kind, RESP_OK_DISPLAY_INFO);
    let word = |at: usize| u32::from_le_bytes(out[at..at + 4].try_into().expect("word"));
    // pmodes[i] = { rect r (x, y, width, height), enabled, flags }.
    assert_eq!(
        [word(24), word(28), word(32), word(36), word(40)],
        [0, 0, 1280, 800, 1]
    );
    assert_eq!([word(56), word(60), word(64)], [640, 480, 1]);
    assert!(
        out[HDR_LEN + 2 * 24..].iter().all(|&b| b == 0),
        "scanouts 2..16 disabled"
    );
}

#[test]
fn rect_bounds_use_checked_arithmetic() {
    assert!(rect(0, 0, 640, 480).fits_in(640, 480));
    assert!(rect(639, 479, 1, 1).fits_in(640, 480));
    assert!(!rect(639, 0, 2, 1).fits_in(640, 480));
    assert!(!rect(0, 0, 0, 1).fits_in(640, 480), "empty");
    assert!(
        !rect(u32::MAX, 0, 2, 1).fits_in(u32::MAX, 1),
        "x + width wraps"
    );
    assert!(
        !rect(0, 1, 1, u32::MAX).fits_in(1, u32::MAX),
        "y + height wraps"
    );
    assert_eq!(
        rect(0, 0, 10, 10).intersect(&rect(5, 5, 10, 10)),
        Some(rect(5, 5, 5, 5))
    );
    assert_eq!(
        rect(0, 0, 4, 4).intersect(&rect(4, 0, 4, 4)),
        None,
        "touching edges"
    );
    assert_eq!(
        rect(u32::MAX - 1, 0, 1, 1).intersect(&rect(0, 0, u32::MAX, 1)),
        Some(rect(u32::MAX - 1, 0, 1, 1))
    );
    assert_eq!(rect(3, 4, 5, 6).to_string(), "5x6+3+4");
}

fn c_define_u32(header: &str, name: &str) -> u32 {
    let needle = format!("#define {name} ");
    let line = header
        .lines()
        .find(|l| l.starts_with(&needle))
        .unwrap_or_else(|| panic!("missing {name}"));
    let value = line[needle.len()..].trim().trim_end_matches('u');
    let value = value
        .split_whitespace()
        .next()
        .unwrap_or(value)
        .trim_end_matches('u');
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).unwrap_or_else(|_| panic!("{name}={value}"))
    } else {
        value.parse().unwrap_or_else(|_| panic!("{name}={value}"))
    }
}

#[test]
fn tvgpu_dod_c_header_matches_protocol() {
    let header = include_str!("../../drivers/tvgpu-dod/include/virtio_gpu_abi.h");
    let cases = [
        ("VIRTIO_GPU_CMD_GET_DISPLAY_INFO", CMD_GET_DISPLAY_INFO),
        ("VIRTIO_GPU_CMD_RESOURCE_CREATE_2D", CMD_RESOURCE_CREATE_2D),
        ("VIRTIO_GPU_CMD_RESOURCE_UNREF", CMD_RESOURCE_UNREF),
        ("VIRTIO_GPU_CMD_SET_SCANOUT", CMD_SET_SCANOUT),
        ("VIRTIO_GPU_CMD_RESOURCE_FLUSH", CMD_RESOURCE_FLUSH),
        (
            "VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D",
            CMD_TRANSFER_TO_HOST_2D,
        ),
        (
            "VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING",
            CMD_RESOURCE_ATTACH_BACKING,
        ),
        ("VIRTIO_GPU_CMD_GET_EDID", CMD_GET_EDID),
        ("VIRTIO_GPU_CMD_UPDATE_CURSOR", CMD_UPDATE_CURSOR),
        ("VIRTIO_GPU_CMD_MOVE_CURSOR", CMD_MOVE_CURSOR),
        ("VIRTIO_GPU_RESP_OK_NODATA", RESP_OK_NODATA),
        ("VIRTIO_GPU_RESP_OK_DISPLAY_INFO", RESP_OK_DISPLAY_INFO),
        ("VIRTIO_GPU_RESP_OK_EDID", RESP_OK_EDID),
        ("VIRTIO_GPU_FLAG_FENCE", FLAG_FENCE),
        ("VIRTIO_GPU_EVENT_DISPLAY", crate::device::EVENT_DISPLAY),
        ("VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM", 2),
        ("VIRTIO_GPU_PCI_VENDOR", 0x1AF4),
        ("VIRTIO_GPU_PCI_DEVICE", 0x1050),
    ];
    for (name, want) in cases {
        assert_eq!(c_define_u32(header, name), want, "{name}");
    }
    assert_eq!(
        c_define_u32(header, "VIRTIO_GPU_MAX_SCANOUTS"),
        MAX_SCANOUTS as u32
    );
    assert_eq!(
        c_define_u32(header, "VIRTIO_GPU_CTRL_HDR_SIZE"),
        HDR_LEN as u32
    );
    assert_eq!(
        c_define_u32(header, "VIRTIO_GPU_EDID_WIRE_LEN"),
        crate::edid::WIRE_EDID_LEN as u32
    );
    assert_eq!(HDR_LEN + MAX_SCANOUTS * 24, DISPLAY_INFO_LEN);
}
