//! Parse and filter Ternvale host log lines for `ternvale logs`.
//!
//! Text lines look like
//! `2026-10-02T16:02:23.678146Z  INFO ThreadId(14) vm{vm=demo}:agent-conn{conn=1}: ternvale::agent: message k=v`
//! (span fields may carry ANSI escapes). JSON lines are objects with `level`
//! and `target`. A line that is neither (a multi-line message, a backtrace)
//! belongs to the record before it.

use std::str::FromStr;

/// Log severity, least severe first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl FromStr for Level {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.to_ascii_lowercase().as_str() {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" | "warning" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            other => Err(format!(
                "unknown level {other:?} (expected trace, debug, info, warn, or error)"
            )),
        }
    }
}

/// What a record's header says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub level: Level,
    pub target: String,
}

/// Which records to show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// Minimum severity.
    pub level: Option<Level>,
    /// Target prefixes on `::` boundaries; any one matching is enough.
    pub targets: Vec<String>,
}

impl Filter {
    /// No level and no target restriction.
    #[tracing::instrument(level = "trace", target = "ternvale::cli", skip_all)]
    pub fn is_empty(&self) -> bool {
        self.level.is_none() && self.targets.is_empty()
    }

    /// Whether `record` passes.
    #[tracing::instrument(level = "trace", target = "ternvale::cli", skip_all)]
    pub fn accepts(&self, record: &Record) -> bool {
        if self.level.is_some_and(|min| record.level < min) {
            return false;
        }
        self.targets.is_empty()
            || self
                .targets
                .iter()
                .any(|want| target_matches(&record.target, want))
    }
}

/// `target` is `want` or below it (`ternvale::virtio` matches
/// `ternvale::virtio::blk`, not `ternvale::virtiofs`). A `want` without the
/// `ternvale::` prefix is also tried with it, so `virtio::blk` works.
#[tracing::instrument(level = "trace", target = "ternvale::cli", skip_all)]
pub fn target_matches(target: &str, want: &str) -> bool {
    let under = |prefix: &str| {
        target == prefix
            || target
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with("::"))
    };
    under(want) || (!want.starts_with("ternvale::") && under(&format!("ternvale::{want}")))
}

/// Decides each line in order, carrying the decision across continuation lines.
#[derive(Debug, Clone)]
pub struct LineFilter {
    filter: Filter,
    showing: bool,
}

impl LineFilter {
    /// Lines before the first header show only when nothing is filtered.
    #[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
    pub fn new(filter: Filter) -> Self {
        let showing = filter.is_empty();
        Self { filter, showing }
    }

    /// Whether to print `line` (one line, without its newline).
    #[tracing::instrument(level = "trace", target = "ternvale::cli", skip_all)]
    pub fn accept(&mut self, line: &str) -> bool {
        if let Some(record) = parse(line) {
            self.showing = self.filter.accepts(&record);
        }
        self.showing
    }
}

/// The header of a record line; `None` for a continuation line.
#[tracing::instrument(level = "trace", target = "ternvale::cli", skip_all)]
pub fn parse(line: &str) -> Option<Record> {
    let line = line.trim_start();
    if line.starts_with('{') {
        return parse_json(line);
    }
    let clean = strip_ansi(line);
    let mut rest = clean.as_str();
    let stamp = next_token(&mut rest)?;
    if !stamp.starts_with(|c: char| c.is_ascii_digit()) || !stamp.contains('T') {
        return None;
    }
    let level = next_token(&mut rest)?.parse().ok()?;
    let mut probe = rest;
    if next_token(&mut probe).is_some_and(|t| t.starts_with("ThreadId(")) {
        rest = probe;
    }
    Some(Record {
        level,
        target: find_target(rest.trim_start())?,
    })
}

fn parse_json(line: &str) -> Option<Record> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    Some(Record {
        level: value.get("level")?.as_str()?.parse().ok()?,
        target: value.get("target")?.as_str()?.to_string(),
    })
}

fn next_token<'a>(rest: &mut &'a str) -> Option<&'a str> {
    let text = rest.trim_start();
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    if end == 0 {
        return None;
    }
    *rest = &text[end..];
    Some(&text[..end])
}

/// After the level: `span{..}:span2: target: message`. Segments end at a
/// `": "` outside braces. The target is the first plain path segment with a
/// `::`; failing that, the first plain segment (a span without fields before
/// a `::`-less target is misread as the target).
fn find_target(text: &str) -> Option<String> {
    let mut segments = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let bytes = text.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b':' if depth == 0 && bytes.get(i + 1) == Some(&b' ') => {
                segments.push(&text[start..i]);
                start = i + 2;
            }
            _ => {}
        }
    }
    let leading: Vec<&str> = segments
        .iter()
        .take_while(|s| is_path(s) || is_span_list(s))
        .copied()
        .collect();
    leading
        .iter()
        .find(|s| is_path(s) && s.contains("::"))
        .or_else(|| leading.iter().find(|s| is_path(s)))
        .map(|s| s.to_string())
}

fn is_path(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '-')
        && !segment.starts_with(':')
        && !segment.ends_with(':')
}

fn is_span_list(segment: &str) -> bool {
    (segment.contains('{') && segment.ends_with('}'))
        || segment
            .split(':')
            .all(|s| !s.is_empty() && (is_path(s) || s.contains('{')))
}

/// `text` without ANSI CSI escape sequences (`ESC [ … final-byte`).
#[tracing::instrument(level = "trace", target = "ternvale::cli", skip_all)]
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
#[path = "logfmt_tests.rs"]
mod tests;
