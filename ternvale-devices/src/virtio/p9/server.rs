//! Minimal 9P2000.L file server over a host directory (virtio-9p export).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

const TVERSION: u8 = 100;
const RVERSION: u8 = 101;
const RLERROR: u8 = 9;
const TATTACH: u8 = 104;
const RATTACH: u8 = 105;
const TWALK: u8 = 176;
const RWALK: u8 = 177;
const TLOPEN: u8 = 12;
const RLOPEN: u8 = 13;
const TREAD: u8 = 16;
const RREAD: u8 = 17;
const TWRITE: u8 = 18;
const RWRITE: u8 = 19;
const TGETATTR: u8 = 24;
const RGETATTR: u8 = 25;
const TREADDIR: u8 = 40;
const RREADDIR: u8 = 41;
const TCLUNK: u8 = 22;
const RCLUNK: u8 = 23;

const QTDIR: u8 = 0x80;
const QTFILE: u8 = 0x00;

const ENOENT: u32 = 2;
const EINVAL: u32 = 22;
const EISDIR: u32 = 21;
const ENOTDIR: u32 = 20;
const EPERM: u32 = 1;

/// Host-side 9p session state.
#[derive(Debug)]
pub struct Server {
    root: PathBuf,
    msize: u32,
    fids: HashMap<u32, Fid>,
}

#[derive(Debug, Clone)]
struct Fid {
    path: PathBuf,
    offset: u64,
}

impl Server {
    /// Export `root` (must be an existing directory).
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::p9", skip_all, fields(root = %root.display()))]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            msize: 8192,
            fids: HashMap::new(),
        }
    }

    /// Handle one 9p request; returns the full reply frame (size + body).
    #[tracing::instrument(level = "trace", target = "ternvale::virtio::p9", skip_all, fields(req_len = request.len()))]
    pub fn handle(&mut self, request: &[u8]) -> Vec<u8> {
        if request.len() < 7 {
            return self.rlerror(0, EINVAL);
        }
        let size = u32::from_le_bytes(request[0..4].try_into().expect("size"));
        if size as usize != request.len() {
            tracing::warn!(target: "ternvale::virtio::p9", size, len = request.len(), "9p frame size mismatch");
            return self.rlerror(0, EINVAL);
        }
        let msg_type = request[4];
        let tag = u16::from_le_bytes(request[5..7].try_into().expect("tag"));
        let body = &request[7..];
        match msg_type {
            TVERSION => self.tversion(tag, body),
            TATTACH => self.tattach(tag, body),
            TWALK => self.twalk(tag, body),
            TLOPEN => self.tlopen(tag, body),
            TREAD => self.tread(tag, body),
            TWRITE => self.twrite(tag, body),
            TGETATTR => self.tgetattr(tag, body),
            TREADDIR => self.treaddir(tag, body),
            TCLUNK => self.tclunk(tag, body),
            other => {
                tracing::warn!(target: "ternvale::virtio::p9", msg_type = other, "unsupported 9p message");
                self.rlerror(tag, EINVAL)
            }
        }
    }

    fn tversion(&mut self, tag: u16, body: &[u8]) -> Vec<u8> {
        let (msize, version) = match parse_version(body) {
            Ok(v) => v,
            Err(errno) => return self.rlerror(tag, errno),
        };
        self.msize = msize.clamp(512, 128 * 1024);
        self.fids.clear();
        tracing::debug!(target: "ternvale::virtio::p9", msize = self.msize, version, "9p version negotiated");
        let version = "9P2000.L";
        let mut out = Vec::new();
        push_header(&mut out, RVERSION, tag);
        out.extend_from_slice(&self.msize.to_le_bytes());
        push_string(&mut out, version);
        finalize(out)
    }

    fn tattach(&mut self, tag: u16, body: &[u8]) -> Vec<u8> {
        let (fid, _afid, _uname, _aname) = match parse_attach(body) {
            Ok(v) => v,
            Err(errno) => return self.rlerror(tag, errno),
        };
        if self.fids.contains_key(&fid) {
            return self.rlerror(tag, EINVAL);
        }
        self.fids.insert(
            fid,
            Fid {
                path: PathBuf::new(),
                offset: 0,
            },
        );
        let qid = self.qid_for_path(&PathBuf::new()).unwrap_or(qid_dir(0));
        let mut out = Vec::new();
        push_header(&mut out, RATTACH, tag);
        push_qid(&mut out, &qid);
        finalize(out)
    }

    fn twalk(&mut self, tag: u16, body: &[u8]) -> Vec<u8> {
        let (fid, newfid, names) = match parse_walk(body) {
            Ok(v) => v,
            Err(errno) => return self.rlerror(tag, errno),
        };
        let base = match self.fids.get(&fid) {
            Some(f) => f.path.clone(),
            None => return self.rlerror(tag, EINVAL),
        };
        let mut path = base;
        let mut qids = Vec::new();
        for name in names {
            if name.is_empty() {
                return self.rlerror(tag, ENOENT);
            }
            if name == ".." {
                if let Some(parent) = path.parent() {
                    path = parent.to_path_buf();
                }
            } else {
                path.push(name);
            }
            let host = match self.resolve(&path) {
                Ok(p) => p,
                Err(errno) => return self.rlerror(tag, errno),
            };
            if !host.exists() {
                return self.rlerror(tag, ENOENT);
            }
            qids.push(self.qid_for_path(&path).unwrap_or(qid_dir(0)));
        }
        if fid != newfid {
            self.fids.insert(newfid, Fid { path, offset: 0 });
        } else if !qids.is_empty() {
            if let Some(f) = self.fids.get_mut(&fid) {
                f.path = path;
                f.offset = 0;
            }
        }
        let mut out = Vec::new();
        push_header(&mut out, RWALK, tag);
        out.extend_from_slice(&(qids.len() as u16).to_le_bytes());
        for qid in &qids {
            push_qid(&mut out, qid);
        }
        finalize(out)
    }

    fn tlopen(&mut self, tag: u16, body: &[u8]) -> Vec<u8> {
        let (fid, _flags) = match parse_lopen(body) {
            Ok(v) => v,
            Err(errno) => return self.rlerror(tag, errno),
        };
        let path = match self.fids.get(&fid) {
            Some(f) => f.path.clone(),
            None => return self.rlerror(tag, EINVAL),
        };
        let host = match self.resolve(&path) {
            Ok(p) => p,
            Err(errno) => return self.rlerror(tag, errno),
        };
        if !host.exists() {
            return self.rlerror(tag, ENOENT);
        }
        if let Some(f) = self.fids.get_mut(&fid) {
            f.offset = 0;
        }
        let qid = self.qid_for_path(&path).unwrap_or(qid_dir(0));
        let mut out = Vec::new();
        push_header(&mut out, RLOPEN, tag);
        push_qid(&mut out, &qid);
        let iounit = (self.msize - 24).max(512);
        out.extend_from_slice(&iounit.to_le_bytes());
        finalize(out)
    }

    fn tread(&mut self, tag: u16, body: &[u8]) -> Vec<u8> {
        let (fid, offset, count) = match parse_read(body) {
            Ok(v) => v,
            Err(errno) => return self.rlerror(tag, errno),
        };
        let path = match self.fids.get(&fid) {
            Some(f) => f.path.clone(),
            None => return self.rlerror(tag, EINVAL),
        };
        let host = match self.resolve(&path) {
            Ok(p) => p,
            Err(errno) => return self.rlerror(tag, errno),
        };
        if host.is_dir() {
            return self.rlerror(tag, EISDIR);
        }
        let mut file = match File::open(&host) {
            Ok(f) => f,
            Err(_) => return self.rlerror(tag, ENOENT),
        };
        let max = count.min(self.msize - 24);
        let mut buf = vec![0u8; max as usize];
        if file.seek(SeekFrom::Start(offset)).is_err() {
            return self.rlerror(tag, EINVAL);
        }
        let n = match file.read(&mut buf) {
            Ok(n) => n,
            Err(_) => return self.rlerror(tag, EINVAL),
        };
        buf.truncate(n);
        if let Some(f) = self.fids.get_mut(&fid) {
            f.offset = offset + n as u64;
        }
        let mut out = Vec::new();
        push_header(&mut out, RREAD, tag);
        out.extend_from_slice(&(n as u32).to_le_bytes());
        out.extend_from_slice(&buf);
        finalize(out)
    }

    fn twrite(&mut self, tag: u16, body: &[u8]) -> Vec<u8> {
        let (fid, offset, data) = match parse_write(body) {
            Ok(v) => v,
            Err(errno) => return self.rlerror(tag, errno),
        };
        let path = match self.fids.get(&fid) {
            Some(f) => f.path.clone(),
            None => return self.rlerror(tag, EINVAL),
        };
        let host = match self.resolve(&path) {
            Ok(p) => p,
            Err(errno) => return self.rlerror(tag, errno),
        };
        if host.is_dir() {
            return self.rlerror(tag, EISDIR);
        }
        let mut file = match fs::OpenOptions::new().write(true).open(&host) {
            Ok(f) => f,
            Err(_) => return self.rlerror(tag, ENOENT),
        };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            return self.rlerror(tag, EINVAL);
        }
        let n = match file.write_all(data) {
            Ok(()) => data.len(),
            Err(_) => return self.rlerror(tag, EINVAL),
        };
        let mut out = Vec::new();
        push_header(&mut out, RWRITE, tag);
        out.extend_from_slice(&(n as u32).to_le_bytes());
        finalize(out)
    }

    fn tgetattr(&mut self, tag: u16, body: &[u8]) -> Vec<u8> {
        let (fid, _mask) = match parse_getattr(body) {
            Ok(v) => v,
            Err(errno) => return self.rlerror(tag, errno),
        };
        let path = match self.fids.get(&fid) {
            Some(f) => f.path.clone(),
            None => return self.rlerror(tag, EINVAL),
        };
        let host = match self.resolve(&path) {
            Ok(p) => p,
            Err(errno) => return self.rlerror(tag, errno),
        };
        let meta = match fs::metadata(&host) {
            Ok(m) => m,
            Err(_) => return self.rlerror(tag, ENOENT),
        };
        let qid = self.qid_for_path(&path).unwrap_or(qid_dir(0));
        let mut out = Vec::new();
        push_header(&mut out, RGETATTR, tag);
        push_qid(&mut out, &qid);
        out.extend_from_slice(&(0u32).to_le_bytes()); // valid mask placeholder
        out.extend_from_slice(&(qid.qtype as u32).to_le_bytes());
        out.extend_from_slice(&(0u32).to_le_bytes()); // mode stub
        out.extend_from_slice(&(1u32).to_le_bytes()); // nlink
        out.extend_from_slice(&(0u64).to_le_bytes()); // uid
        out.extend_from_slice(&(0u64).to_le_bytes()); // gid
        out.extend_from_slice(&(meta.len()).to_le_bytes());
        out.extend_from_slice(&(512u64).to_le_bytes()); // blksize
        out.extend_from_slice(&(meta.len().div_ceil(512)).to_le_bytes());
        out.extend_from_slice(&(0u64).to_le_bytes()); // atime
        out.extend_from_slice(&(0u64).to_le_bytes());
        out.extend_from_slice(&(0u64).to_le_bytes());
        out.extend_from_slice(&(0u64).to_le_bytes());
        finalize(out)
    }

    fn treaddir(&mut self, tag: u16, body: &[u8]) -> Vec<u8> {
        let (fid, offset, count) = match parse_readdir(body) {
            Ok(v) => v,
            Err(errno) => return self.rlerror(tag, errno),
        };
        let path = match self.fids.get(&fid) {
            Some(f) => f.path.clone(),
            None => return self.rlerror(tag, EINVAL),
        };
        let host = match self.resolve(&path) {
            Ok(p) => p,
            Err(errno) => return self.rlerror(tag, errno),
        };
        if !host.is_dir() {
            return self.rlerror(tag, ENOTDIR);
        }
        let entries = match fs::read_dir(&host) {
            Ok(e) => e,
            Err(_) => return self.rlerror(tag, ENOENT),
        };
        let mut payload = Vec::new();
        let mut index = 0u64;
        for entry in entries.flatten() {
            if index < offset {
                index += 1;
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let child = path.join(name.as_ref());
            let qid = self.qid_for_path(&child).unwrap_or(qid_dir(0));
            let stat = encode_dirent(&qid, &name, 0);
            if payload.len() + stat.len() > count as usize {
                break;
            }
            payload.extend_from_slice(&stat);
            index += 1;
        }
        let mut out = Vec::new();
        push_header(&mut out, RREADDIR, tag);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&payload);
        finalize(out)
    }

    fn tclunk(&mut self, tag: u16, body: &[u8]) -> Vec<u8> {
        let fid = match parse_u32(body, 0) {
            Ok(v) => v,
            Err(errno) => return self.rlerror(tag, errno),
        };
        self.fids.remove(&fid);
        let mut out = Vec::new();
        push_header(&mut out, RCLUNK, tag);
        finalize(out)
    }

    fn rlerror(&self, tag: u16, errno: u32) -> Vec<u8> {
        let mut out = Vec::new();
        push_header(&mut out, RLERROR, tag);
        out.extend_from_slice(&errno.to_le_bytes());
        push_string(&mut out, "error");
        finalize(out)
    }

    fn resolve(&self, rel: &Path) -> Result<PathBuf, u32> {
        for component in rel.components() {
            match component {
                Component::ParentDir | Component::RootDir | Component::CurDir => {}
                Component::Normal(name) if name == ".." || name == "." => {}
                Component::Normal(_) => {}
                Component::Prefix(_) => return Err(EINVAL),
            }
        }
        let joined = self.root.join(rel);
        let canon = joined.canonicalize().map_err(|_| ENOENT)?;
        let root = self.root.canonicalize().map_err(|_| ENOENT)?;
        if !canon.starts_with(&root) {
            return Err(EPERM);
        }
        Ok(canon)
    }

    fn qid_for_path(&self, rel: &Path) -> Option<Qid> {
        let host = self.resolve(rel).ok()?;
        let qtype = if host.is_dir() { QTDIR } else { QTFILE };
        let path_hash = hash_path(rel);
        Some(Qid {
            qtype,
            version: 0,
            path: path_hash,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct Qid {
    qtype: u8,
    version: u32,
    path: u64,
}

fn qid_dir(path: u64) -> Qid {
    Qid {
        qtype: QTDIR,
        version: 0,
        path,
    }
}

fn hash_path(path: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

fn push_header(out: &mut Vec<u8>, msg_type: u8, tag: u16) {
    out.extend_from_slice(&0u32.to_le_bytes());
    out.push(msg_type);
    out.extend_from_slice(&tag.to_le_bytes());
}

fn finalize(mut body: Vec<u8>) -> Vec<u8> {
    let size = body.len() as u32;
    body[0..4].copy_from_slice(&size.to_le_bytes());
    body
}

fn push_string(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn push_qid(out: &mut Vec<u8>, qid: &Qid) {
    out.push(qid.qtype);
    out.extend_from_slice(&qid.version.to_le_bytes());
    out.extend_from_slice(&qid.path.to_le_bytes());
}

fn read_string(body: &[u8], at: usize) -> Result<(String, usize), u32> {
    if at + 2 > body.len() {
        return Err(EINVAL);
    }
    let len = u16::from_le_bytes(body[at..at + 2].try_into().expect("len")) as usize;
    let start = at + 2;
    let end = start + len;
    if end > body.len() {
        return Err(EINVAL);
    }
    let s = String::from_utf8_lossy(&body[start..end]).into_owned();
    Ok((s, end))
}

fn parse_u32(body: &[u8], at: usize) -> Result<u32, u32> {
    if at + 4 > body.len() {
        return Err(EINVAL);
    }
    Ok(u32::from_le_bytes(
        body[at..at + 4].try_into().expect("u32"),
    ))
}

fn parse_version(body: &[u8]) -> Result<(u32, String), u32> {
    let msize = parse_u32(body, 0)?;
    let (version, _) = read_string(body, 4)?;
    Ok((msize, version))
}

fn parse_attach(body: &[u8]) -> Result<(u32, u32, String, String), u32> {
    let fid = parse_u32(body, 0)?;
    let afid = parse_u32(body, 4)?;
    let (uname, at) = read_string(body, 8)?;
    let (aname, _) = read_string(body, at)?;
    Ok((fid, afid, uname, aname))
}

fn parse_walk(body: &[u8]) -> Result<(u32, u32, Vec<String>), u32> {
    let fid = parse_u32(body, 0)?;
    let newfid = parse_u32(body, 4)?;
    if body.len() < 10 {
        return Err(EINVAL);
    }
    let nwname = u16::from_le_bytes(body[8..10].try_into().expect("nw")) as usize;
    let mut at = 10;
    let mut names = Vec::new();
    for _ in 0..nwname {
        let (name, next) = read_string(body, at)?;
        names.push(name);
        at = next;
    }
    Ok((fid, newfid, names))
}

fn parse_lopen(body: &[u8]) -> Result<(u32, u32), u32> {
    let fid = parse_u32(body, 0)?;
    let flags = parse_u32(body, 4)?;
    Ok((fid, flags))
}

fn parse_read(body: &[u8]) -> Result<(u32, u64, u32), u32> {
    let fid = parse_u32(body, 0)?;
    let offset = u64::from_le_bytes(body[4..12].try_into().expect("off"));
    let count = parse_u32(body, 12)?;
    Ok((fid, offset, count))
}

fn parse_write(body: &[u8]) -> Result<(u32, u64, &[u8]), u32> {
    let fid = parse_u32(body, 0)?;
    let offset = u64::from_le_bytes(body[4..12].try_into().expect("off"));
    let count = parse_u32(body, 12)?;
    let start = 16;
    let end = start + count as usize;
    if end > body.len() {
        return Err(EINVAL);
    }
    Ok((fid, offset, &body[start..end]))
}

fn parse_getattr(body: &[u8]) -> Result<(u32, u64), u32> {
    let fid = parse_u32(body, 0)?;
    let mask = u64::from_le_bytes(body[4..12].try_into().expect("mask"));
    Ok((fid, mask))
}

fn parse_readdir(body: &[u8]) -> Result<(u32, u64, u32), u32> {
    let fid = parse_u32(body, 0)?;
    let offset = u64::from_le_bytes(body[4..12].try_into().expect("off"));
    let count = parse_u32(body, 12)?;
    Ok((fid, offset, count))
}

fn encode_dirent(qid: &Qid, name: &str, entry_offset: u64) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&entry_offset.to_le_bytes());
    push_qid(&mut out, qid);
    push_string(&mut out, name);
    let stat_len = (out.len() - 8) as u16;
    let mut frame = Vec::new();
    frame.extend_from_slice(&stat_len.to_le_bytes());
    frame.extend_from_slice(&out[8..]);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn large_host_file_round_trip() {
        let dir = std::env::temp_dir().join(format!("tv9p-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("from_host.bin");
        let size = 2 * 1024 * 1024usize;
        let mut data = Vec::with_capacity(size);
        for i in 0..size {
            data.push((i % 251) as u8);
        }
        std::fs::write(&file, &data).expect("write");
        let mut srv = Server::new(dir.clone());
        let _ = srv.handle(&build_tversion(8192, "9P2000.L"));
        let _ = srv.handle(&build_tattach(0, "linux", ""));
        let _ = srv.handle(&build_twalk(0, 1, &["from_host.bin"]));
        let _ = srv.handle(&build_tlopen(1, 0));
        let mut offset = 0u64;
        let mut got = Vec::new();
        while got.len() < size {
            let read = build_tread(1, offset, (size - got.len()) as u32);
            let rsp = srv.handle(&read);
            assert_eq!(rsp[4], RREAD);
            let n = u32::from_le_bytes(rsp[7..11].try_into().expect("n")) as usize;
            if n == 0 {
                break;
            }
            got.extend_from_slice(&rsp[11..11 + n]);
            offset += n as u64;
        }
        assert_eq!(got.len(), size);
        assert_eq!(&got, &data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_and_attach_root() {
        let dir = std::env::temp_dir().join(format!("tv9p-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("hello.txt");
        std::fs::File::create(&file)
            .expect("file")
            .write_all(b"hi")
            .expect("write");
        let mut srv = Server::new(dir.clone());
        let req = build_tversion(8192, "9P2000.L");
        let rsp = srv.handle(&req);
        assert_eq!(rsp[4], RVERSION);
        let attach = build_tattach(0, "linux", "");
        let rsp = srv.handle(&attach);
        assert_eq!(rsp[4], RATTACH);
        let walk = build_twalk(0, 1, &["hello.txt"]);
        let rsp = srv.handle(&walk);
        assert_eq!(rsp[4], RWALK);
        let open = build_tlopen(1, 0);
        let rsp = srv.handle(&open);
        assert_eq!(rsp[4], RLOPEN);
        let read = build_tread(1, 0, 64);
        let rsp = srv.handle(&read);
        assert_eq!(rsp[4], RREAD);
        let n = u32::from_le_bytes(rsp[7..11].try_into().expect("n"));
        assert_eq!(&rsp[11..11 + n as usize], b"hi");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn build_tversion(msize: u32, version: &str) -> Vec<u8> {
        let mut body = Vec::new();
        push_header(&mut body, TVERSION, 1);
        body.extend_from_slice(&msize.to_le_bytes());
        push_string(&mut body, version);
        finalize(body)
    }

    fn build_tattach(fid: u32, uname: &str, aname: &str) -> Vec<u8> {
        let mut body = Vec::new();
        push_header(&mut body, TATTACH, 2);
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&0xffffffffu32.to_le_bytes());
        push_string(&mut body, uname);
        push_string(&mut body, aname);
        finalize(body)
    }

    fn build_twalk(fid: u32, newfid: u32, names: &[&str]) -> Vec<u8> {
        let mut body = Vec::new();
        push_header(&mut body, TWALK, 3);
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&newfid.to_le_bytes());
        body.extend_from_slice(&(names.len() as u16).to_le_bytes());
        for name in names {
            push_string(&mut body, name);
        }
        finalize(body)
    }

    fn build_tlopen(fid: u32, flags: u32) -> Vec<u8> {
        let mut body = Vec::new();
        push_header(&mut body, TLOPEN, 4);
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&flags.to_le_bytes());
        finalize(body)
    }

    fn build_tread(fid: u32, offset: u64, count: u32) -> Vec<u8> {
        let mut body = Vec::new();
        push_header(&mut body, TREAD, 5);
        body.extend_from_slice(&fid.to_le_bytes());
        body.extend_from_slice(&offset.to_le_bytes());
        body.extend_from_slice(&count.to_le_bytes());
        finalize(body)
    }
}
