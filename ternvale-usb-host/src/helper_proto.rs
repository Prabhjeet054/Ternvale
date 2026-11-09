//! Minimal USB-claim protocol on the privileged helper socket (`TVUC` / `TVUX`).

use std::io::{self, Read, Write};

use ternvale_usb::UsbControlSetup;

/// Client: claim device.
pub const CLAIM_MAGIC: &[u8; 4] = b"TVUC";
/// Helper: claim result.
pub const ACK_MAGIC: &[u8; 4] = b"TVUA";
/// Client: control transfer.
pub const XFER_MAGIC: &[u8; 4] = b"TVUX";
/// Helper: transfer result.
pub const REPLY_MAGIC: &[u8; 4] = b"TVUR";
const MAX: usize = 65_535;

/// Claim `vendor_id`:`product_id`.
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
pub fn write_claim(w: &mut impl Write, vendor_id: u16, product_id: u16) -> io::Result<()> {
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(CLAIM_MAGIC);
    buf[4..6].copy_from_slice(&vendor_id.to_le_bytes());
    buf[6..8].copy_from_slice(&product_id.to_le_bytes());
    w.write_all(&buf)
}

/// Read a claim request.
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
pub fn read_claim(r: &mut impl Read) -> io::Result<(u16, u16)> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    if &buf[..4] != CLAIM_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad usb claim magic",
        ));
    }
    Ok((
        u16::from_le_bytes([buf[4], buf[5]]),
        u16::from_le_bytes([buf[6], buf[7]]),
    ))
}

/// Claim acknowledgement (`status == 0` is success).
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
pub fn write_ack(w: &mut impl Write, status: i32, label: &str) -> io::Result<()> {
    let label = label.as_bytes();
    let n = label.len().min(1024) as u16;
    w.write_all(ACK_MAGIC)?;
    w.write_all(&status.to_le_bytes())?;
    w.write_all(&n.to_le_bytes())?;
    w.write_all(&label[..n as usize])
}

/// Read [`write_ack`].
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
pub fn read_ack(r: &mut impl Read) -> io::Result<(i32, String)> {
    let mut hdr = [0u8; 10];
    r.read_exact(&mut hdr)?;
    if &hdr[..4] != ACK_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad usb ack magic",
        ));
    }
    let status = i32::from_le_bytes(hdr[4..8].try_into().unwrap_or([0; 4]));
    let n = u16::from_le_bytes([hdr[8], hdr[9]]) as usize;
    if n > 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "usb ack label too long",
        ));
    }
    let mut label = vec![0u8; n];
    r.read_exact(&mut label)?;
    Ok((status, String::from_utf8_lossy(&label).into_owned()))
}

/// Control transfer request.
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
pub fn write_xfer(
    w: &mut impl Write,
    setup: UsbControlSetup,
    data_out: Option<&[u8]>,
    in_max: u32,
) -> io::Result<()> {
    let out = data_out.unwrap_or(&[]);
    if out.len() > MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usb xfer too large",
        ));
    }
    w.write_all(XFER_MAGIC)?;
    w.write_all(&setup.to_bytes())?;
    w.write_all(&(out.len() as u32).to_le_bytes())?;
    w.write_all(out)?;
    w.write_all(&in_max.to_le_bytes())
}

/// Read [`write_xfer`].
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
pub fn read_xfer(r: &mut impl Read) -> io::Result<(UsbControlSetup, Vec<u8>, u32)> {
    let mut hdr = [0u8; 16];
    r.read_exact(&mut hdr)?;
    if &hdr[..4] != XFER_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad usb xfer magic",
        ));
    }
    let setup = UsbControlSetup::from_bytes(&hdr[4..12])
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "usb setup"))?;
    let out_len = u32::from_le_bytes(hdr[12..16].try_into().unwrap_or([0; 4])) as usize;
    if out_len > MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "usb xfer too large",
        ));
    }
    let mut out = vec![0u8; out_len];
    r.read_exact(&mut out)?;
    let mut in_max = [0u8; 4];
    r.read_exact(&mut in_max)?;
    Ok((setup, out, u32::from_le_bytes(in_max)))
}

/// Transfer reply.
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
pub fn write_reply(w: &mut impl Write, status: i32, data_in: &[u8]) -> io::Result<()> {
    let n = data_in.len().min(MAX) as u32;
    w.write_all(REPLY_MAGIC)?;
    w.write_all(&status.to_le_bytes())?;
    w.write_all(&n.to_le_bytes())?;
    w.write_all(&data_in[..n as usize])
}

/// Read [`write_reply`].
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
pub fn read_reply(r: &mut impl Read) -> io::Result<(i32, Vec<u8>)> {
    let mut hdr = [0u8; 12];
    r.read_exact(&mut hdr)?;
    if &hdr[..4] != REPLY_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad usb reply magic",
        ));
    }
    let status = i32::from_le_bytes(hdr[4..8].try_into().unwrap_or([0; 4]));
    let n = u32::from_le_bytes(hdr[8..12].try_into().unwrap_or([0; 4])) as usize;
    if n > MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "usb reply too large",
        ));
    }
    let mut data = vec![0u8; n];
    r.read_exact(&mut data)?;
    Ok((status, data))
}

#[cfg(test)]
mod tests {
    use super::{read_ack, read_claim, write_ack, write_claim};
    use std::io::Cursor;

    #[test]
    fn claim_roundtrip() {
        let mut buf = Vec::new();
        write_claim(&mut buf, 0x046d, 0xc52b).unwrap();
        let (vid, pid) = read_claim(&mut Cursor::new(buf)).unwrap();
        assert_eq!((vid, pid), (0x046d, 0xc52b));
    }

    #[test]
    fn ack_roundtrip() {
        let mut buf = Vec::new();
        write_ack(&mut buf, 0, "usb:1").unwrap();
        let (st, label) = read_ack(&mut Cursor::new(buf)).unwrap();
        assert_eq!(st, 0);
        assert_eq!(label, "usb:1");
    }
}
