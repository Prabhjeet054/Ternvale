//! Guest window enumeration (X11 `wmctrl -lG` when available).

use ternvale_agent_proto::GuestWindow;

use crate::AgentError;

/// List mapped top-level windows for seamless mode.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
pub fn list_guest_windows() -> Result<Vec<GuestWindow>, AgentError> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(windows) = list_via_wmctrl() {
            tracing::info!(target: "ternvale::agent", count = windows.len(), "window list from wmctrl");
            return Ok(windows);
        }
        tracing::debug!(target: "ternvale::agent", "wmctrl unavailable; returning empty window list");
        return Ok(Vec::new());
    }
    #[cfg(not(target_os = "linux"))]
    {
        tracing::warn!(target: "ternvale::agent", "window_list ignored on non-Linux guest");
        Ok(Vec::new())
    }
}

#[cfg(target_os = "linux")]
fn list_via_wmctrl() -> Result<Vec<GuestWindow>, AgentError> {
    let out = std::process::Command::new("wmctrl")
        .args(["-lG"])
        .output()
        .map_err(|source| AgentError::Action {
            action: "wmctrl -lG",
            source,
        })?;
    if !out.status.success() {
        tracing::warn!(
            target: "ternvale::agent",
            code = ?out.status.code(),
            "wmctrl -lG failed"
        );
        return Ok(Vec::new());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(parse_wmctrl_lg(&text))
}

/// Parse `wmctrl -lG` lines: id desktop x y width height host title...
#[cfg(any(target_os = "linux", test))]
#[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all, fields(bytes = text.len()))]
pub fn parse_wmctrl_lg(text: &str) -> Vec<GuestWindow> {
    let mut windows = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let Some(window) = parse_wmctrl_line(line) else {
            tracing::trace!(target: "ternvale::agent", line, "skipped wmctrl line");
            continue;
        };
        windows.push(window);
    }
    windows
}

#[cfg(any(target_os = "linux", test))]
fn parse_wmctrl_line(line: &str) -> Option<GuestWindow> {
    let mut parts = line.split_whitespace();
    let id_hex = parts.next()?;
    let id = u32::from_str_radix(id_hex.trim_start_matches("0x"), 16).ok()?;
    let _desktop = parts.next()?;
    let x: i32 = parts.next()?.parse().ok()?;
    let y: i32 = parts.next()?.parse().ok()?;
    let width: u32 = parts.next()?.parse().ok()?;
    let height: u32 = parts.next()?.parse().ok()?;
    let _host = parts.next()?;
    let title = parts.collect::<Vec<_>>().join(" ");
    if width == 0 || height == 0 {
        return None;
    }
    Some(GuestWindow {
        id,
        title,
        x,
        y,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wmctrl_sample() {
        let sample = "0x01200003  0 10 20 640 480 host xterm\n";
        let list = parse_wmctrl_lg(sample);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, 0x0120_0003);
        assert_eq!(list[0].title, "xterm");
        assert_eq!(
            (list[0].x, list[0].y, list[0].width, list[0].height),
            (10, 20, 640, 480)
        );
    }

    #[test]
    fn parses_two_wmctrl_lines_as_separate_windows() {
        let sample = "\
0x00000001  0 0 0 640 480 host xterm\n\
0x00000002  0 700 0 800 600 host gedit\n";
        let list = parse_wmctrl_lg(sample);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].title, "xterm");
        assert_eq!(list[1].title, "gedit");
        assert_ne!(list[0].id, list[1].id);
    }
}
