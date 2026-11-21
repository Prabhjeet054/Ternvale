//! IPP operation allowlist (RFC 8011 + PWG job ops). Administrative CUPS ops are blocked.

/// IPP `Print-Job` (RFC 8011 §4.2.1).
pub const OP_PRINT_JOB: u16 = 0x0002;
/// IPP `Print-URI` (RFC 8011 §4.2.2) — blocked (SSRF).
pub const OP_PRINT_URI: u16 = 0x0003;
/// IPP `Validate-Job` (RFC 8011 §4.2.3).
pub const OP_VALIDATE_JOB: u16 = 0x0004;
/// IPP `Create-Job` (RFC 8011 §4.2.4).
pub const OP_CREATE_JOB: u16 = 0x0005;
/// IPP `Send-Document` (RFC 8011 §4.2.6).
pub const OP_SEND_DOCUMENT: u16 = 0x0006;
/// IPP `Send-URI` (RFC 8011 §4.2.7) — blocked (SSRF).
pub const OP_SEND_URI: u16 = 0x0007;
/// IPP `Cancel-Job` (RFC 8011 §4.2.8).
pub const OP_CANCEL_JOB: u16 = 0x0008;
/// IPP `Get-Job-Attributes` (RFC 8011 §4.2.9).
pub const OP_GET_JOB_ATTRIBUTES: u16 = 0x0009;
/// IPP `Get-Jobs` (RFC 8011 §4.2.10).
pub const OP_GET_JOBS: u16 = 0x000A;
/// IPP `Get-Printer-Attributes` (RFC 8011 §4.2.5). Required by IPP Everywhere (PWG 5100.14).
pub const OP_GET_PRINTER_ATTRIBUTES: u16 = 0x000B;
/// IPP `Hold-Job` (RFC 8011 §4.3.1).
pub const OP_HOLD_JOB: u16 = 0x000C;
/// IPP `Release-Job` (RFC 8011 §4.3.2).
pub const OP_RELEASE_JOB: u16 = 0x000D;
/// PWG 5100.11 `Cancel-Current-Job`.
pub const OP_CANCEL_CURRENT_JOB: u16 = 0x0038;
/// PWG 5100.11 `Cancel-My-Jobs`.
pub const OP_CANCEL_MY_JOBS: u16 = 0x0039;
/// PWG 5100.11 `Close-Job`.
pub const OP_CLOSE_JOB: u16 = 0x003B;
/// IPP `Pause-Printer` (RFC 8011 §4.2.7 / admin).
pub const OP_PAUSE_PRINTER: u16 = 0x0010;
/// IPP `Resume-Printer`.
pub const OP_RESUME_PRINTER: u16 = 0x0011;
/// IPP `Purge-Jobs`.
pub const OP_PURGE_JOBS: u16 = 0x0012;
/// IPP `Set-Printer-Attributes`.
pub const OP_SET_PRINTER_ATTRIBUTES: u16 = 0x0013;
/// CUPS `CUPS-Delete-Printer` (extension 0x4004).
pub const OP_CUPS_DELETE_PRINTER: u16 = 0x4004;

/// Job / printer-status operations forwarded to host CUPS. Admin and URI-fetch ops are not listed.
///
/// TODO(verify): confirm a Windows 11 ARM64 IPP Everywhere client and a Linux
/// CUPS `everywhere` queue only need this set (plus HTTP POST to `/printers/id`).
pub const ALLOWED_OPS: &[u16] = &[
    OP_PRINT_JOB,
    OP_VALIDATE_JOB,
    OP_CREATE_JOB,
    OP_SEND_DOCUMENT,
    OP_CANCEL_JOB,
    OP_GET_JOB_ATTRIBUTES,
    OP_GET_JOBS,
    OP_GET_PRINTER_ATTRIBUTES,
    OP_HOLD_JOB,
    OP_RELEASE_JOB,
    OP_CANCEL_CURRENT_JOB,
    OP_CANCEL_MY_JOBS,
    OP_CLOSE_JOB,
];

/// Wire name for logs (never includes document bytes).
#[tracing::instrument(level = "trace", target = "ternvale::print", skip_all, fields(op = format!("{:#06x}", op)))]
pub fn op_name(op: u16) -> &'static str {
    match op {
        OP_PRINT_JOB => "Print-Job",
        OP_PRINT_URI => "Print-URI",
        OP_VALIDATE_JOB => "Validate-Job",
        OP_CREATE_JOB => "Create-Job",
        OP_SEND_DOCUMENT => "Send-Document",
        OP_SEND_URI => "Send-URI",
        OP_CANCEL_JOB => "Cancel-Job",
        OP_GET_JOB_ATTRIBUTES => "Get-Job-Attributes",
        OP_GET_JOBS => "Get-Jobs",
        OP_GET_PRINTER_ATTRIBUTES => "Get-Printer-Attributes",
        OP_HOLD_JOB => "Hold-Job",
        OP_RELEASE_JOB => "Release-Job",
        OP_PAUSE_PRINTER => "Pause-Printer",
        OP_RESUME_PRINTER => "Resume-Printer",
        OP_PURGE_JOBS => "Purge-Jobs",
        OP_SET_PRINTER_ATTRIBUTES => "Set-Printer-Attributes",
        OP_CANCEL_CURRENT_JOB => "Cancel-Current-Job",
        OP_CANCEL_MY_JOBS => "Cancel-My-Jobs",
        OP_CLOSE_JOB => "Close-Job",
        OP_CUPS_DELETE_PRINTER => "CUPS-Delete-Printer",
        _ if op >= 0x4000 => "CUPS-extension",
        _ => "unknown",
    }
}

/// True when the proxy may forward this operation-id to host CUPS.
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all, fields(op = format!("{:#06x}", op)))]
pub fn is_allowed(op: u16) -> bool {
    ALLOWED_OPS.contains(&op)
}

/// Parse version + operation-id + request-id from an IPP body (RFC 8011 §4.1.1).
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all, fields(len = body.len()))]
pub fn parse_header(body: &[u8]) -> Option<(u8, u8, u16, u32)> {
    if body.len() < 8 {
        return None;
    }
    let major = body[0];
    let minor = body[1];
    let op = u16::from_be_bytes([body[2], body[3]]);
    let request_id = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
    Some((major, minor, op, request_id))
}

/// Offset of the document after `end-of-attributes-tag` (0x03), if present.
#[tracing::instrument(level = "trace", target = "ternvale::print", skip_all)]
pub fn document_after_attributes(body: &[u8]) -> Option<&[u8]> {
    if body.len() < 9 {
        return None;
    }
    body[8..]
        .iter()
        .position(|&b| b == 0x03)
        .map(|i| &body[8 + i + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_is_job_ops_only() {
        assert!(is_allowed(OP_PRINT_JOB));
        assert!(is_allowed(OP_GET_PRINTER_ATTRIBUTES));
        assert!(!is_allowed(OP_PRINT_URI));
        assert!(!is_allowed(OP_SEND_URI));
        assert!(!is_allowed(OP_PAUSE_PRINTER));
        assert!(!is_allowed(OP_CUPS_DELETE_PRINTER));
        assert!(!is_allowed(0x4003));
    }

    #[test]
    fn parses_ipp_header() {
        let mut body = vec![1, 1, 0, 2, 0, 0, 0, 7, 0x03];
        body.extend_from_slice(b"%PDF-1.4\n");
        let (maj, min, op, rid) = parse_header(&body).expect("hdr");
        assert_eq!((maj, min, op, rid), (1, 1, OP_PRINT_JOB, 7));
        assert_eq!(document_after_attributes(&body), Some(&b"%PDF-1.4\n"[..]));
    }
}
