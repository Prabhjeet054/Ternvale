//! Parse and execute one virtio-blk request chain.

use ternvale_vmm::GuestMemory;

use super::backend::FileBackend;
use super::{
    BlkStats, Buffer, ID_BYTES, SECTOR, VIRTIO_BLK_S_IOERR, VIRTIO_BLK_S_OK, VIRTIO_BLK_S_UNSUPP,
    VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_GET_ID, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT,
};
use crate::virtio::queue::Chain;

struct Header {
    ty: u32,
    sector: u64,
}

/// Run one chain. Returns the used-ring length (writable bytes touched).
pub fn process(
    mem: &mut GuestMemory,
    backend: &FileBackend,
    chain: &Chain,
    serial: &[u8; ID_BYTES],
    stats: &BlkStats,
) -> u32 {
    let Some((header, status_addr)) = parse(mem, chain) else {
        stats.error();
        return write_status(mem, chain_status(chain), VIRTIO_BLK_S_IOERR);
    };
    let data_len = data_bytes(chain, header.ty);
    tracing::trace!(
        target: "ternvale::virtio::blk",
        req_type = header.ty,
        sector = header.sector,
        len = data_len,
        head = chain.head,
        "virtio-blk request"
    );
    let (status, written) = match header.ty {
        VIRTIO_BLK_T_IN => {
            let status = read_req(mem, backend, chain, header.sector, data_len, stats);
            (
                status,
                if status == VIRTIO_BLK_S_OK {
                    data_len
                } else {
                    0
                },
            )
        }
        VIRTIO_BLK_T_OUT => (
            write_req(mem, backend, chain, header.sector, data_len, stats),
            0,
        ),
        VIRTIO_BLK_T_FLUSH => (flush_req(backend, stats), 0),
        VIRTIO_BLK_T_GET_ID => {
            let status = get_id(mem, chain, serial, stats);
            (
                status,
                if status == VIRTIO_BLK_S_OK {
                    ID_BYTES as u32
                } else {
                    0
                },
            )
        }
        other => {
            tracing::error!(
                target: "ternvale::virtio::blk",
                req_type = other,
                "unsupported virtio-blk request"
            );
            stats.error();
            (VIRTIO_BLK_S_UNSUPP, 0)
        }
    };
    write_status(mem, Some(status_addr), status) + written
}

fn parse(mem: &GuestMemory, chain: &Chain) -> Option<(Header, u64)> {
    let mut hdr = [0u8; 16];
    if copy_from(mem, &chain.readable, &mut hdr).is_err() {
        tracing::error!(target: "ternvale::virtio::blk", "virtio-blk header is short");
        return None;
    }
    let ty = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    let sector = u64::from_le_bytes([
        hdr[8], hdr[9], hdr[10], hdr[11], hdr[12], hdr[13], hdr[14], hdr[15],
    ]);
    let status_addr = chain_status(chain)?;
    Some((Header { ty, sector }, status_addr))
}

fn chain_status(chain: &Chain) -> Option<u64> {
    let last = chain.writable.last()?;
    if last.len == 0 {
        return None;
    }
    Some(last.addr + u64::from(last.len) - 1)
}

fn data_bytes(chain: &Chain, ty: u32) -> u32 {
    match ty {
        VIRTIO_BLK_T_IN | VIRTIO_BLK_T_GET_ID => {
            let total: u32 = chain.writable.iter().map(|b| b.len).sum();
            total.saturating_sub(1)
        }
        VIRTIO_BLK_T_OUT => {
            let total: u32 = chain.readable.iter().map(|b| b.len).sum();
            total.saturating_sub(16)
        }
        _ => 0,
    }
}

fn read_req(
    mem: &mut GuestMemory,
    backend: &FileBackend,
    chain: &Chain,
    sector: u64,
    len: u32,
    stats: &BlkStats,
) -> u8 {
    if !in_range(backend, sector, len) {
        tracing::error!(
            target: "ternvale::virtio::blk",
            sector,
            len,
            capacity = backend.capacity(),
            "virtio-blk read out of range"
        );
        stats.error();
        return VIRTIO_BLK_S_IOERR;
    }
    let mut buf = vec![0u8; len as usize];
    if let Err(error) = backend.read_at(&mut buf, sector * SECTOR) {
        tracing::error!(
            target: "ternvale::virtio::blk",
            sector,
            len,
            error = %error,
            "virtio-blk read failed"
        );
        stats.error();
        return VIRTIO_BLK_S_IOERR;
    }
    if copy_to(mem, &data_writable(chain), &buf).is_err() {
        stats.error();
        return VIRTIO_BLK_S_IOERR;
    }
    stats.ok(len);
    VIRTIO_BLK_S_OK
}

fn write_req(
    mem: &GuestMemory,
    backend: &FileBackend,
    chain: &Chain,
    sector: u64,
    len: u32,
    stats: &BlkStats,
) -> u8 {
    if backend.read_only() {
        tracing::error!(
            target: "ternvale::virtio::blk",
            sector,
            len,
            "virtio-blk write to read-only disk"
        );
        stats.error();
        return VIRTIO_BLK_S_IOERR;
    }
    if !in_range(backend, sector, len) {
        tracing::error!(
            target: "ternvale::virtio::blk",
            sector,
            len,
            capacity = backend.capacity(),
            "virtio-blk write out of range"
        );
        stats.error();
        return VIRTIO_BLK_S_IOERR;
    }
    let mut buf = vec![0u8; len as usize];
    if copy_from(mem, &out_readable(chain), &mut buf).is_err() {
        stats.error();
        return VIRTIO_BLK_S_IOERR;
    }
    if let Err(error) = backend.write_at(&buf, sector * SECTOR) {
        tracing::error!(
            target: "ternvale::virtio::blk",
            sector,
            len,
            error = %error,
            "virtio-blk write failed"
        );
        stats.error();
        return VIRTIO_BLK_S_IOERR;
    }
    stats.ok(len);
    VIRTIO_BLK_S_OK
}

fn flush_req(backend: &FileBackend, stats: &BlkStats) -> u8 {
    if let Err(error) = backend.flush() {
        tracing::error!(
            target: "ternvale::virtio::blk",
            error = %error,
            "virtio-blk flush failed"
        );
        stats.error();
        return VIRTIO_BLK_S_IOERR;
    }
    stats.ok(0);
    VIRTIO_BLK_S_OK
}

fn get_id(mem: &mut GuestMemory, chain: &Chain, serial: &[u8; ID_BYTES], stats: &BlkStats) -> u8 {
    if copy_to(mem, &data_writable(chain), serial).is_err() {
        stats.error();
        return VIRTIO_BLK_S_IOERR;
    }
    stats.ok(ID_BYTES as u32);
    VIRTIO_BLK_S_OK
}

fn in_range(backend: &FileBackend, sector: u64, len: u32) -> bool {
    if len == 0 {
        return sector <= backend.capacity();
    }
    let sectors = u64::from(len.div_ceil(SECTOR as u32));
    sector
        .checked_add(sectors)
        .is_some_and(|end| end <= backend.capacity())
}

fn data_writable(chain: &Chain) -> Vec<Buffer> {
    let mut bufs = chain.writable.clone();
    if let Some(last) = bufs.last_mut() {
        if last.len > 0 {
            last.len -= 1;
        }
    }
    while bufs.last().is_some_and(|b| b.len == 0) {
        bufs.pop();
    }
    bufs
}

fn out_readable(chain: &Chain) -> Vec<Buffer> {
    let mut skip = 16u32;
    let mut out = Vec::new();
    for buf in &chain.readable {
        if skip >= buf.len {
            skip -= buf.len;
            continue;
        }
        out.push(Buffer {
            addr: buf.addr + u64::from(skip),
            len: buf.len - skip,
        });
        skip = 0;
    }
    out
}

fn copy_from(mem: &GuestMemory, bufs: &[Buffer], dst: &mut [u8]) -> Result<(), ()> {
    let mut off = 0usize;
    for buf in bufs {
        if off >= dst.len() {
            break;
        }
        let take = (buf.len as usize).min(dst.len() - off);
        if take == 0 {
            continue;
        }
        mem.read_bytes(buf.addr, &mut dst[off..off + take])
            .map_err(|_| ())?;
        off += take;
    }
    if off < dst.len() {
        Err(())
    } else {
        Ok(())
    }
}

fn copy_to(mem: &mut GuestMemory, bufs: &[Buffer], src: &[u8]) -> Result<(), ()> {
    let mut off = 0usize;
    for buf in bufs {
        if off >= src.len() {
            break;
        }
        let take = (buf.len as usize).min(src.len() - off);
        if take == 0 {
            continue;
        }
        mem.write_bytes(buf.addr, &src[off..off + take])
            .map_err(|_| ())?;
        off += take;
    }
    Ok(())
}

fn write_status(mem: &mut GuestMemory, addr: Option<u64>, status: u8) -> u32 {
    let Some(addr) = addr else {
        return 0;
    };
    if mem.write_u8(addr, status).is_err() {
        tracing::error!(
            target: "ternvale::virtio::blk",
            addr = format!("{addr:#x}"),
            "virtio-blk status write failed"
        );
        return 0;
    }
    1
}
