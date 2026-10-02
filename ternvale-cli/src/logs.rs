//! `ternvale logs <name>`: print (or follow) a VM's newest host log, filtered
//! by level and target.
//!
//! Host logs are `ternvale-<name>-<YYYYMMDD-HHMMSS>.log` in the log
//! directory; the name match is exact, so `demo` never picks up `demo-2`.

use std::fs::File;
use std::io::{BufRead, BufReader, IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ternvale_log::LogConfig;

use crate::logfmt::{strip_ansi, Filter, LineFilter};

/// How often `--follow` checks for new lines.
pub const FOLLOW_POLL: Duration = Duration::from_millis(250);

/// `ternvale logs` arguments.
#[derive(Debug, Clone, Default)]
pub struct LogsArgs {
    pub name: String,
    pub follow: bool,
    pub filter: Filter,
    pub list: bool,
    pub file: Option<PathBuf>,
}

/// The `YYYYMMDD-HHMMSS` of a host log file name for `vm`, or `None` if
/// the name is not exactly `ternvale-<vm>-<stamp>.log`.
#[tracing::instrument(level = "trace", target = "ternvale::cli", skip_all)]
pub fn stamp_of<'a>(file_name: &'a str, vm: &str) -> Option<&'a str> {
    let stamp = file_name
        .strip_prefix("ternvale-")?
        .strip_prefix(vm)?
        .strip_prefix('-')?
        .strip_suffix(".log")?;
    let (day, time) = stamp.split_once('-')?;
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    (digits(day, 8) && digits(time, 6)).then_some(stamp)
}

/// Host logs of `vm` in `dir`, oldest first.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(dir = %dir.display(), vm))]
pub fn find_logs(dir: &Path, vm: &str) -> Result<Vec<PathBuf>> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("list log directory {}", dir.display()))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("list log directory {}", dir.display()))?;
        let name = entry.file_name();
        if let Some(stamp) = name.to_str().and_then(|n| stamp_of(n, vm)) {
            found.push((stamp.to_string(), entry.path()));
        }
    }
    found.sort();
    tracing::debug!(target: "ternvale::cli", count = found.len(), "host logs found");
    Ok(found.into_iter().map(|(_, path)| path).collect())
}

/// The newest host log of `vm` in `dir`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm))]
pub fn newest(dir: &Path, vm: &str) -> Result<Option<PathBuf>> {
    Ok(find_logs(dir, vm)?.pop())
}

/// Run `ternvale logs`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = %args.name, follow = args.follow))]
pub fn logs(args: &LogsArgs) -> Result<ExitCode> {
    let dir = LogConfig::default_log_dir().context("find the log directory")?;
    if args.list {
        for path in find_logs(&dir, &args.name)? {
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            println!("{}  {}", path.display(), crate::doctor::human(bytes));
        }
        return Ok(ExitCode::SUCCESS);
    }
    let path = match &args.file {
        Some(path) => path.clone(),
        None => newest(&dir, &args.name)?.with_context(|| {
            format!(
                "no host logs for vm {:?} in {} (expected ternvale-{}-YYYYMMDD-HHMMSS.log)",
                args.name,
                dir.display(),
                args.name
            )
        })?,
    };
    tracing::info!(target: "ternvale::cli", path = %path.display(), "showing host log");
    let color = std::io::stdout().is_terminal();
    let mut out = std::io::stdout().lock();
    let filter = LineFilter::new(args.filter.clone());
    let result = if args.follow {
        let rotate = args.file.is_none().then(|| (dir, args.name.clone()));
        follow(&path, rotate, filter, &mut out, color, &|| false)
    } else {
        print_file(&path, filter, &mut out, color)
    };
    match result {
        Err(error) if is_broken_pipe(&error) => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS),
    }
}

/// Print every line of `path` that `filter` accepts.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn print_file(
    path: &Path,
    mut filter: LineFilter,
    out: &mut dyn Write,
    color: bool,
) -> Result<()> {
    let file = File::open(path).with_context(|| format!("open host log {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let read = reader
            .read_until(b'\n', &mut buf)
            .with_context(|| format!("read host log {}", path.display()))?;
        if read == 0 {
            return Ok(());
        }
        emit(&buf, &mut filter, out, color)?;
    }
}

/// Print `path` like [`print_file`], then keep printing lines as they are
/// appended until `stop` returns true. With `rotate = (dir, vm)`, switch to
/// a newer host log of `vm` when one appears (the VM was restarted).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn follow(
    path: &Path,
    rotate: Option<(PathBuf, String)>,
    mut filter: LineFilter,
    out: &mut dyn Write,
    color: bool,
    stop: &dyn Fn() -> bool,
) -> Result<()> {
    let mut tail = Tail::open(path)?;
    loop {
        tail.poll(&mut filter, out, color)?;
        out.flush().context("flush log output")?;
        if stop() {
            return Ok(());
        }
        if let Some((dir, vm)) = &rotate {
            match newest(dir, vm) {
                Ok(Some(next)) if next != tail.path => {
                    tail.poll(&mut filter, out, color)?;
                    eprintln!("ternvale: switching to newer log {}", next.display());
                    tracing::info!(target: "ternvale::cli", from = %tail.path.display(), to = %next.display(), "following newer host log");
                    tail = Tail::open(&next)?;
                    continue;
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(target: "ternvale::cli", error = %format!("{error:#}"), "could not look for a newer log")
                }
            }
        }
        std::thread::sleep(FOLLOW_POLL);
    }
}

struct Tail {
    path: PathBuf,
    file: File,
    pos: u64,
    partial: Vec<u8>,
}

impl Tail {
    fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            file: File::open(path).with_context(|| format!("open host log {}", path.display()))?,
            pos: 0,
            partial: Vec::new(),
        })
    }

    /// Emit complete lines appended since the last poll; keep a partial one.
    fn poll(&mut self, filter: &mut LineFilter, out: &mut dyn Write, color: bool) -> Result<()> {
        let len = self
            .file
            .metadata()
            .with_context(|| format!("stat host log {}", self.path.display()))?
            .len();
        if len < self.pos {
            eprintln!(
                "ternvale: {} was truncated; reading from the start",
                self.path.display()
            );
            tracing::warn!(target: "ternvale::cli", path = %self.path.display(), len, pos = self.pos, "host log truncated");
            self.pos = 0;
            self.partial.clear();
        }
        if len == self.pos {
            return Ok(());
        }
        self.file
            .seek(SeekFrom::Start(self.pos))
            .with_context(|| format!("seek host log {}", self.path.display()))?;
        let mut chunk = Vec::new();
        let read = (&self.file)
            .take(len - self.pos)
            .read_to_end(&mut chunk)
            .with_context(|| format!("read host log {}", self.path.display()))?;
        self.pos += read as u64;
        self.partial.extend_from_slice(&chunk);
        let Some(last) = self.partial.iter().rposition(|&b| b == b'\n') else {
            return Ok(());
        };
        let rest = self.partial.split_off(last + 1);
        for line in self.partial.split_inclusive(|&b| b == b'\n') {
            emit(line, filter, out, color)?;
        }
        self.partial = rest;
        Ok(())
    }
}

fn emit(raw: &[u8], filter: &mut LineFilter, out: &mut dyn Write, color: bool) -> Result<()> {
    let text = String::from_utf8_lossy(raw);
    let line = text.trim_end_matches(['\n', '\r']);
    if !filter.accept(line) {
        return Ok(());
    }
    let shown = if color {
        line.to_string()
    } else {
        strip_ansi(line)
    };
    writeln!(out, "{shown}").context("write log line")
}

fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
    })
}

/// Fail early on a bad `--target` (empty, or with spaces).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn check_targets(targets: &[String]) -> Result<()> {
    for target in targets {
        if target.is_empty() || target.contains(char::is_whitespace) {
            bail!("bad --target {target:?}: expected a path like ternvale::virtio::blk");
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "logs_tests.rs"]
mod tests;
