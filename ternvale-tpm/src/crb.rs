//! The CRB register window: five locality pages over one TPM.
//!
//! Register map in [`crate::regs`]; locality arbitration in
//! [`crate::locality`]. A command runs like this (PTP §6.5.3, Linux
//! `tpm_crb.c`): request a locality, write `cmdReady` to `TPM_CRB_CTRL_REQ`
//! (clears `tpmIdle`), write the command into the data buffer, write 1 to
//! `TPM_CRB_CTRL_START`, poll until `START` reads 0, read the response from
//! the same buffer, write `goIdle`, relinquish.
//!
//! Commands complete inside the `START` write, so `START` never reads 1 and
//! cancel has nothing to stop. Only the active locality's `START` runs a
//! command. One data buffer backs all five pages, and the buffer and
//! `CTRL_REQ` accept any locality's writes, as in QEMU.
//! TODO(verify): whether the PTP requires per-locality buffers or dropping
//! data-buffer writes from a locality that is not active.
//! No interrupt is wired (the DSDT `_CRS` has none), so `TPM_CRB_INT_ENABLE`
//! is stored and `TPM_CRB_INT_STS` stays 0: guests poll.
//!
//! Every access is checked: size 1, 2, 4 or 8, naturally aligned, inside the
//! window. Anything else logs WARN, reads 0 and writes nothing. Every command
//! and response byte stream is logged at TRACE on `ternvale::tpm`.

use crate::backend::{
    error_response, hex, CommandHeader, TpmBackend, HEADER_LEN, RC_COMMAND_SIZE, RC_FAILURE,
    ST_NO_SESSIONS,
};
use crate::locality::Localities;
use crate::regs::*;
use crate::TpmError;

/// The CRB interface of one TPM.
pub struct TpmCrb {
    base: u64,
    localities: Localities,
    idle: bool,
    cancel: u32,
    int_enable: u32,
    buffer: Vec<u8>,
    backend: Box<dyn TpmBackend>,
    commands: u64,
}

impl std::fmt::Debug for TpmCrb {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TpmCrb")
            .field("base", &format_args!("{:#x}", self.base))
            .field("active", &self.localities.active())
            .field("idle", &self.idle)
            .field("backend", &self.backend.name())
            .field("commands", &self.commands)
            .finish()
    }
}

impl TpmCrb {
    /// A CRB at guest physical `base` (page aligned) running commands on
    /// `backend`. Starts idle with no locality assigned.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::tpm",
        skip_all,
        fields(base = %format!("{base:#x}"), backend = backend.name())
    )]
    pub fn new(base: u64, backend: Box<dyn TpmBackend>) -> Result<Self, TpmError> {
        if base % LOCALITY_STRIDE != 0 {
            tracing::warn!(target: "ternvale::tpm", base = %format!("{base:#x}"), "tpm crb base is not 4 KiB aligned");
            return Err(TpmError::Misaligned { base });
        }
        if base.checked_add(CRB_WINDOW_SIZE).is_none() {
            tracing::warn!(target: "ternvale::tpm", base = %format!("{base:#x}"), "tpm crb window overflows");
            return Err(TpmError::Overflow { base });
        }
        tracing::info!(
            target: "ternvale::tpm",
            base = %format!("{base:#x}"),
            size = %format!("{CRB_WINDOW_SIZE:#x}"),
            control_area = %format!("{:#x}", base + CONTROL_AREA_OFFSET),
            buffer_bytes = DATA_BUFFER_SIZE,
            backend = backend.name(),
            "tpm crb ready"
        );
        Ok(Self {
            base,
            localities: Localities::new(),
            idle: true,
            cancel: 0,
            int_enable: 0,
            buffer: vec![0; DATA_BUFFER_SIZE],
            backend,
            commands: 0,
        })
    }

    /// Guest physical address of the window.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn base(&self) -> u64 {
        self.base
    }

    /// Commands run so far.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn commands(&self) -> u64 {
        self.commands
    }

    /// Read `size` bytes at `offset` from the window base.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip(self), fields(offset = %format!("{offset:#x}")))]
    pub fn read(&mut self, offset: u64, size: u8) -> u64 {
        let Some((locality, reg)) = check_access(offset, size, "read") else {
            return 0;
        };
        let len = usize::from(size);
        let value = if reg >= DATA_BUFFER {
            let at = (reg - DATA_BUFFER) as usize;
            self.buffer[at..at + len]
                .iter()
                .rev()
                .fold(0, |value, byte| (value << 8) | u64::from(*byte))
        } else {
            (0..len as u64).rev().fold(0, |value, i| {
                let at = reg + i;
                let byte = (self.dword(locality, at & !3) >> ((at & 3) * 8)) & 0xff;
                (value << 8) | u64::from(byte)
            })
        };
        tracing::trace!(target: "ternvale::tpm", locality, reg = %format!("{reg:#x}"), size, value = %format!("{value:#x}"), direction = "read", "tpm crb access");
        value
    }

    /// Write the low `size` bytes of `value` at `offset` from the window base.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::tpm",
        skip(self, value),
        fields(offset = %format!("{offset:#x}"))
    )]
    pub fn write(&mut self, offset: u64, size: u8, value: u64) {
        let Some((locality, reg)) = check_access(offset, size, "write") else {
            return;
        };
        tracing::trace!(target: "ternvale::tpm", locality, reg = %format!("{reg:#x}"), size, value = %format!("{value:#x}"), direction = "write", "tpm crb access");
        let len = usize::from(size);
        if reg >= DATA_BUFFER {
            let at = (reg - DATA_BUFFER) as usize;
            self.buffer[at..at + len].copy_from_slice(&value.to_le_bytes()[..len]);
            return;
        }
        let lane = (reg & 3) * 8;
        let mask = if size == 8 {
            u64::MAX
        } else {
            ((1u64 << (len * 8)) - 1) << lane
        };
        let shifted = value << lane;
        let dword = reg & !3;
        self.write_dword(locality, dword, shifted as u32, mask as u32);
        if size == 8 {
            self.write_dword(locality, dword + 4, (value >> 32) as u32, u32::MAX);
        }
    }

    fn buffer_gpa(&self, locality: u8) -> u64 {
        self.base + u64::from(locality) * LOCALITY_STRIDE + DATA_BUFFER
    }

    fn dword(&self, locality: u8, reg: u64) -> u32 {
        match reg {
            LOC_STATE => self.localities.state(),
            LOC_STS => self.localities.status(locality),
            INTF_ID_LO => INTF_ID_LOW,
            INTF_ID_HI => INTF_ID_HIGH,
            CTRL_STS if self.idle => CTRL_STS_IDLE,
            CTRL_CANCEL => self.cancel,
            INT_ENABLE => self.int_enable,
            CMD_SIZE | RSP_SIZE => DATA_BUFFER_SIZE as u32,
            CMD_LADDR | RSP_ADDR_LO => self.buffer_gpa(locality) as u32,
            CMD_HADDR | RSP_ADDR_HI => (self.buffer_gpa(locality) >> 32) as u32,
            // LOC_CTRL, CTRL_EXT, CTRL_REQ, CTRL_START, INT_STS, busy
            // CTRL_STS, and reserved offsets read 0.
            _ => 0,
        }
    }

    fn write_dword(&mut self, locality: u8, reg: u64, value: u32, mask: u32) {
        let value = value & mask;
        match reg {
            LOC_CTRL => self.localities.control(locality, value),
            CTRL_REQ => self.request(locality, value),
            CTRL_CANCEL => {
                self.cancel = (self.cancel & !mask) | (value & START_INVOKE);
                tracing::debug!(target: "ternvale::tpm", locality, cancel = self.cancel, "tpm cancel written; commands complete synchronously, nothing to cancel");
            }
            CTRL_START if value & START_INVOKE != 0 => self.start(locality),
            CTRL_START => {}
            INT_ENABLE => {
                self.int_enable = (self.int_enable & !mask) | value;
                tracing::debug!(target: "ternvale::tpm", int_enable = %format!("{:#x}", self.int_enable), "tpm interrupt enable stored; no interrupt is wired");
            }
            INT_STS | CTRL_EXT_LO | CTRL_EXT_HI => {
                tracing::debug!(target: "ternvale::tpm", reg = %format!("{reg:#x}"), value = %format!("{value:#x}"), "tpm register write has no effect");
            }
            // EDK2's `PtpCrbTpmCommand` (Tpm2Ptp.c) writes the buffer address
            // and size before every START. The buffer is fixed, so only the
            // advertised values are accepted. TODO(verify): whether the PTP
            // lets software relocate the buffer.
            CMD_SIZE | CMD_LADDR | CMD_HADDR | RSP_SIZE | RSP_ADDR_LO | RSP_ADDR_HI => {
                let current = self.dword(locality, reg) & mask;
                if value == current {
                    tracing::debug!(target: "ternvale::tpm", locality, reg = %format!("{reg:#x}"), value = %format!("{value:#x}"), "tpm buffer address/size write matches the fixed buffer");
                } else {
                    tracing::warn!(target: "ternvale::tpm", locality, reg = %format!("{reg:#x}"), value = %format!("{value:#x}"), fixed = %format!("{current:#x}"), "tpm buffer address/size write differs from the fixed buffer; ignored");
                }
            }
            _ => tracing::warn!(
                target: "ternvale::tpm",
                locality,
                reg = %format!("{reg:#x}"),
                value = %format!("{value:#x}"),
                "write to read-only or reserved tpm crb register ignored"
            ),
        }
    }

    fn request(&mut self, locality: u8, value: u32) {
        match (
            value & CTRL_REQ_CMD_READY != 0,
            value & CTRL_REQ_GO_IDLE != 0,
        ) {
            (true, true) => {
                tracing::warn!(target: "ternvale::tpm", locality, "cmdReady and goIdle together ignored");
            }
            (true, false) => {
                self.idle = false;
                tracing::debug!(target: "ternvale::tpm", locality, "tpm ready (cmdReady)");
            }
            (false, true) => {
                self.idle = true;
                tracing::debug!(target: "ternvale::tpm", locality, "tpm idle (goIdle)");
            }
            (false, false) => {}
        }
    }

    fn start(&mut self, locality: u8) {
        if self.localities.active() != Some(locality) {
            tracing::warn!(
                target: "ternvale::tpm",
                locality,
                active = ?self.localities.active(),
                "tpm start from a locality that is not active ignored"
            );
            return;
        }
        if self.idle {
            tracing::warn!(target: "ternvale::tpm", locality, "tpm start while idle (no cmdReady); running it anyway, as QEMU does");
        }
        let header = CommandHeader::parse(&self.buffer);
        let size = header
            .map(|h| h.size as usize)
            .filter(|size| (HEADER_LEN..=DATA_BUFFER_SIZE).contains(size));
        let shown = size.unwrap_or(HEADER_LEN);
        tracing::trace!(
            target: "ternvale::tpm",
            locality,
            len = shown,
            code = %header.map_or("none".into(), |h| format!("{:#x}", h.code)),
            bytes = %hex(&self.buffer[..shown]),
            "tpm command"
        );
        let mut response = match size {
            Some(size) => self.backend.execute(locality, &self.buffer[..size]),
            None => {
                tracing::warn!(
                    target: "ternvale::tpm",
                    locality,
                    size = header.map(|h| h.size),
                    max = DATA_BUFFER_SIZE,
                    "tpm commandSize outside 10..=buffer size; answered TPM_RC_COMMAND_SIZE"
                );
                error_response(ST_NO_SESSIONS, RC_COMMAND_SIZE)
            }
        };
        let declared = response
            .get(2..6)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
        if !(HEADER_LEN..=DATA_BUFFER_SIZE).contains(&response.len())
            || declared != Some(response.len())
        {
            tracing::error!(
                target: "ternvale::tpm",
                backend = self.backend.name(),
                len = response.len(),
                declared = ?declared,
                "tpm backend returned a malformed response; answered TPM_RC_FAILURE"
            );
            response = error_response(ST_NO_SESSIONS, RC_FAILURE);
        }
        self.buffer[..response.len()].copy_from_slice(&response);
        self.commands += 1;
        let code = header.map_or("none".into(), |h| format!("{:#x}", h.code));
        if self.commands == 1 {
            tracing::info!(target: "ternvale::tpm", locality, code = %code, "tpm first command from the guest");
        }
        tracing::trace!(
            target: "ternvale::tpm",
            locality,
            len = response.len(),
            bytes = %hex(&response),
            "tpm response"
        );
        tracing::debug!(
            target: "ternvale::tpm",
            locality,
            code = %code,
            rc = %format!("{:#x}", u32::from_be_bytes([response[6], response[7], response[8], response[9]])),
            response_len = response.len(),
            commands = self.commands,
            "tpm command completed"
        );
    }
}

/// `(locality, register offset in its page)` for a valid access.
fn check_access(offset: u64, size: u8, direction: &str) -> Option<(u8, u64)> {
    let in_window = offset
        .checked_add(u64::from(size))
        .is_some_and(|end| end <= CRB_WINDOW_SIZE);
    if !matches!(size, 1 | 2 | 4 | 8) || offset % u64::from(size) != 0 || !in_window {
        tracing::warn!(
            target: "ternvale::tpm",
            offset = %format!("{offset:#x}"),
            size,
            direction,
            "tpm crb access is not 1/2/4/8 bytes, naturally aligned, inside the window; ignored"
        );
        return None;
    }
    Some(((offset / LOCALITY_STRIDE) as u8, offset % LOCALITY_STRIDE))
}

#[cfg(test)]
#[path = "crb_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "crb_sequence_tests.rs"]
mod sequence_tests;
