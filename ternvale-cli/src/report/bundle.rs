//! Write a crash-report zip: named items under one top-level folder, plus a
//! README listing what went in, what was cut, and what was missing.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{Datelike, Timelike};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

/// Where one item's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A file; only its last `cap` bytes (from a line start) if it is larger.
    File {
        path: PathBuf,
        cap: u64,
    },
    Bytes(Vec<u8>),
    /// Not available, and why.
    Missing(String),
}

/// One file in the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub source: Source,
}

impl Item {
    /// The whole of `path` up to `cap` bytes.
    #[tracing::instrument(level = "trace", target = "ternvale::cli", skip_all)]
    pub fn file(name: &str, path: &Path, cap: u64) -> Self {
        Self {
            name: name.to_string(),
            source: Source::File {
                path: path.to_path_buf(),
                cap,
            },
        }
    }

    /// In-memory content.
    #[tracing::instrument(level = "trace", target = "ternvale::cli", skip_all)]
    pub fn bytes(name: &str, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.to_string(),
            source: Source::Bytes(bytes.into()),
        }
    }

    /// A gap the README should explain.
    #[tracing::instrument(level = "trace", target = "ternvale::cli", skip_all)]
    pub fn missing(name: &str, why: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            source: Source::Missing(why.into()),
        }
    }
}

/// Write `items` to `out` under `top/`, then `top/README.txt` starting with
/// `header`. Returns the README text.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(out = %out.display(), items = items.len()))]
pub fn write_zip(out: &Path, top: &str, header: &[String], items: &[Item]) -> Result<String> {
    let file = File::create(out).with_context(|| format!("create report {}", out.display()))?;
    let mut zip = ZipWriter::new(file);
    let now = chrono::Local::now();
    let stamp = DateTime::from_date_and_time(
        u16::try_from(now.year()).unwrap_or(1980),
        now.month() as u8,
        now.day() as u8,
        now.hour() as u8,
        now.minute() as u8,
        now.second().min(58) as u8,
    )
    .unwrap_or_default();
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644)
        .last_modified_time(stamp)
        .large_file(true);
    let mut included = Vec::new();
    let mut missing = Vec::new();
    for item in items {
        let entry = format!("{top}/{}", item.name);
        let note = match &item.source {
            Source::Missing(why) => {
                missing.push(format!("  {}: {why}", item.name));
                continue;
            }
            Source::Bytes(bytes) => {
                zip.start_file(&entry, options)
                    .with_context(|| format!("start {entry} in the report"))?;
                zip.write_all(bytes)
                    .with_context(|| format!("write {entry} in the report"))?;
                format!("{} bytes", bytes.len())
            }
            Source::File { path, cap } => match open_tail(path, *cap) {
                Ok((mut reader, len, kept)) => {
                    zip.start_file(&entry, options)
                        .with_context(|| format!("start {entry} in the report"))?;
                    std::io::copy(&mut reader, &mut zip)
                        .with_context(|| format!("copy {} into the report", path.display()))?;
                    if kept < len {
                        format!("last {kept} of {len} bytes of {}", path.display())
                    } else {
                        format!("{len} bytes from {}", path.display())
                    }
                }
                Err(error) => {
                    tracing::warn!(target: "ternvale::cli", path = %path.display(), error = %format!("{error:#}"), "report item unreadable");
                    missing.push(format!("  {}: {error:#}", item.name));
                    continue;
                }
            },
        };
        included.push(format!("  {}: {note}", item.name));
    }
    let mut readme = header.to_vec();
    readme.push(String::new());
    readme.push("Contents:".to_string());
    readme.extend(included);
    if !missing.is_empty() {
        readme.push(String::new());
        readme.push("Missing:".to_string());
        readme.extend(missing);
    }
    let readme = readme.join("\n") + "\n";
    zip.start_file(format!("{top}/README.txt"), options)
        .context("start README.txt in the report")?;
    zip.write_all(readme.as_bytes())
        .context("write README.txt in the report")?;
    zip.finish()
        .with_context(|| format!("finish report {}", out.display()))?;
    tracing::info!(target: "ternvale::cli", out = %out.display(), "report written");
    Ok(readme)
}

/// A reader over `path`, or over its last `cap` bytes starting at a line
/// boundary; also the file length and the bytes kept.
fn open_tail(path: &Path, cap: u64) -> Result<(Box<dyn Read>, u64, u64)> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let len = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    if len <= cap {
        return Ok((Box::new(file.take(len)), len, len));
    }
    file.seek(SeekFrom::Start(len - cap))
        .with_context(|| format!("seek {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut skipped = Vec::new();
    reader
        .read_until(b'\n', &mut skipped)
        .with_context(|| format!("read {}", path.display()))?;
    let kept = cap.saturating_sub(skipped.len() as u64);
    Ok((Box::new(reader.take(kept)), len, kept))
}
