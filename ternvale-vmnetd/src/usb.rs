//! USB claim + control-transfer session on the privileged helper.

use std::io::{self, Write};
use std::os::unix::net::UnixStream;

use ternvale_usb::HostUsbBackend;
use ternvale_usb_host::{claim_by_vid_pid, read_claim, read_xfer, write_ack, write_reply};

/// Claim a device and serve control transfers until the client disconnects.
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
pub fn serve_usb(mut stream: UnixStream) -> io::Result<()> {
    let (vid, pid) = read_claim(&mut stream)?;
    tracing::info!(
        target: "ternvale::usb",
        vendor_id = format!("{vid:#06x}"),
        product_id = format!("{pid:#06x}"),
        "privileged helper usb claim"
    );
    let device = match claim_by_vid_pid(vid, pid) {
        Ok(mut device) => {
            let (tx, _rx) = std::sync::mpsc::channel();
            device.wire_disconnect_notifier(3, tx);
            write_ack(&mut stream, 0, device.label())?;
            stream.flush()?;
            device
        }
        Err(error) => {
            write_ack(&mut stream, 1, &error.to_string())?;
            stream.flush()?;
            return Err(io::Error::other(error.to_string()));
        }
    };
    loop {
        let (setup, data_out, in_max) = match read_xfer(&mut stream) {
            Ok(req) => req,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        };
        let mut data_in = vec![0u8; in_max.min(65_535) as usize];
        let out = if data_out.is_empty() {
            None
        } else {
            Some(data_out.as_slice())
        };
        match device.control_transfer(setup, out, &mut data_in) {
            Ok(n) => write_reply(&mut stream, 0, &data_in[..n])?,
            Err(error) => {
                tracing::warn!(target: "ternvale::usb", error = %error, "helper usb transfer failed");
                write_reply(&mut stream, 1, &[])?;
            }
        }
        stream.flush()?;
    }
}
