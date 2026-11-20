//! Step 112 host analogs: Notepad/Calculator overlapping, ops, dialogs,
//! 30-minute leak bound, toggle-off (no live Windows guest).

use std::collections::HashMap;
use std::path::PathBuf;

use ternvale_agent_proto::{GuestWindow, WindowKind, WindowState};
use ternvale_devices::WindowPixels;

use super::{persist_frames, HostLine};

const NOTEPAD: u32 = 0x100;
const CALC: u32 = 0x200;
const DIALOG: u32 = 0x101;
/// 30 minutes at the 500 ms publisher poll.
const POLL_CYCLES_30MIN: usize = 3_600;

fn win(
    id: u32,
    title: &str,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    z: u32,
    focused: bool,
) -> GuestWindow {
    GuestWindow {
        id,
        title: title.into(),
        x,
        y,
        width,
        height,
        z_order: z,
        focused,
        ..GuestWindow::default()
    }
}

fn notepad() -> GuestWindow {
    win(NOTEPAD, "Untitled - Notepad", 10, 10, 400, 300, 0, true)
}

fn calculator() -> GuestWindow {
    // Overlaps Notepad in guest coordinates (covered by z_order).
    win(CALC, "Calculator", 80, 60, 320, 280, 1, false)
}

fn solid(r: u8, g: u8, b: u8, w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 3) as usize);
    for _ in 0..(w * h) {
        out.push(r);
        out.push(g);
        out.push(b);
    }
    out
}

fn fingerprint(pixels: &[u8]) -> (u8, u8, u8, usize) {
    (
        pixels.first().copied().unwrap_or(0),
        pixels.get(1).copied().unwrap_or(0),
        pixels.get(2).copied().unwrap_or(0),
        pixels.len(),
    )
}

/// Per-window capture vs a shared framebuffer crop of the overlap.
struct HostSession {
    hosts: HashMap<u32, HostWin>,
    typed: HashMap<u32, u32>,
    embedded: bool,
}

struct HostWin {
    rec: GuestWindow,
    pixels: Vec<u8>,
    parent: Option<u32>,
}

impl HostSession {
    fn new() -> Self {
        Self {
            hosts: HashMap::new(),
            typed: HashMap::new(),
            embedded: true,
        }
    }

    fn apply(&mut self, windows: &[GuestWindow], frames: &HashMap<u32, Vec<u8>>) {
        let mapped: Vec<&GuestWindow> = windows
            .iter()
            .filter(|w| !matches!(w.kind, WindowKind::Taskbar | WindowKind::Start))
            .collect();
        let incoming: std::collections::HashSet<u32> = mapped.iter().map(|w| w.id).collect();
        self.hosts.retain(|id, _| incoming.contains(id));
        for rec in mapped {
            let pixels = frames.get(&rec.id).cloned().unwrap_or_default();
            self.hosts.insert(
                rec.id,
                HostWin {
                    parent: rec.parent_id,
                    rec: rec.clone(),
                    pixels,
                },
            );
        }
        self.embedded = self.hosts.is_empty();
    }

    fn apply_host_line(&mut self, line: &str, guest: &mut Vec<GuestWindow>) {
        let cmd: HostLine = serde_json::from_str(line).expect("host line");
        apply_guest_op(guest, &cmd);
        if cmd.op == "close" {
            guest.retain(|w| w.id != cmd.id);
        }
        if matches!(cmd.kind.as_deref().unwrap_or(cmd.op.as_str()), "key") {
            *self.typed.entry(cmd.id).or_insert(0) += 1;
        }
        self.apply(guest, &HashMap::new());
        for host in self.hosts.values_mut() {
            if let Some(g) = guest.iter().find(|w| w.id == host.rec.id) {
                host.rec = g.clone();
            }
        }
    }

    fn stop(&mut self) {
        self.hosts.clear();
        self.typed.clear();
        self.embedded = true;
    }
}

fn apply_guest_op(windows: &mut [GuestWindow], cmd: &HostLine) {
    for w in windows.iter_mut() {
        if w.id != cmd.id {
            if cmd.op == "focus" {
                w.focused = false;
            }
            continue;
        }
        match cmd.op.as_str() {
            "move" | "resize" => {
                if let Some(x) = cmd.x {
                    w.x = x;
                }
                if let Some(y) = cmd.y {
                    w.y = y;
                }
                if let Some(width) = cmd.width {
                    w.width = width;
                }
                if let Some(height) = cmd.height {
                    w.height = height;
                }
            }
            "minimize" => w.state = WindowState::Minimized,
            "maximize" => w.state = WindowState::Maximized,
            "restore" => w.state = WindowState::Normal,
            "focus" => w.focused = true,
            "close" => {}
            _ => {}
        }
    }
}

#[test]
fn notepad_and_calculator_are_two_host_windows_with_distinct_pixels_when_overlapping() {
    let mut session = HostSession::new();
    let windows = vec![notepad(), calculator()];
    assert!(
        windows[0].x < windows[1].x + windows[1].width as i32
            && windows[1].x < windows[0].x + windows[0].width as i32,
        "guest windows overlap"
    );
    let mut frames = HashMap::new();
    frames.insert(NOTEPAD, solid(200, 40, 40, 8, 8));
    frames.insert(CALC, solid(40, 40, 200, 8, 8));
    session.apply(&windows, &frames);
    assert!(!session.embedded);
    assert_eq!(session.hosts.len(), 2);
    let np = fingerprint(&session.hosts[&NOTEPAD].pixels);
    let calc = fingerprint(&session.hosts[&CALC].pixels);
    assert_ne!(np, calc, "occluded Calculator still has its own capture");
    assert_eq!(np, (200, 40, 40, 192));
    assert_eq!(calc, (40, 40, 200, 192));
    assert_eq!(session.hosts[&NOTEPAD].rec.title, "Untitled - Notepad");
    assert_eq!(session.hosts[&CALC].rec.title, "Calculator");
}

#[test]
fn host_move_resize_and_close_sync_to_the_guest() {
    let mut session = HostSession::new();
    let mut guest = vec![notepad(), calculator()];
    session.apply(&guest, &HashMap::new());
    session.apply_host_line(
        r#"{"op":"move","id":256,"x":120,"y":40,"width":410,"height":310}"#,
        &mut guest,
    );
    let np = guest.iter().find(|w| w.id == NOTEPAD).expect("notepad");
    assert_eq!((np.x, np.y, np.width, np.height), (120, 40, 410, 310));
    assert_eq!(session.hosts[&NOTEPAD].rec.x, 120);
    session.apply_host_line(r#"{"op":"close","id":512}"#, &mut guest);
    assert!(guest.iter().all(|w| w.id != CALC));
    assert!(!session.hosts.contains_key(&CALC));
    guest.clear();
    session.apply(&guest, &HashMap::new());
    assert!(session.hosts.is_empty());
    assert!(
        session.embedded,
        "guest close of last window restores single-window"
    );
}

#[test]
fn typing_follows_focus_to_the_correct_app() {
    let mut session = HostSession::new();
    let mut guest = vec![notepad(), calculator()];
    session.apply(&guest, &HashMap::new());
    session.apply_host_line(r#"{"op":"focus","id":256}"#, &mut guest);
    session.apply_host_line(
        r#"{"op":"key","id":256,"kind":"key","code":30,"value":1}"#,
        &mut guest,
    );
    session.apply_host_line(r#"{"op":"focus","id":512}"#, &mut guest);
    session.apply_host_line(
        r#"{"op":"key","id":512,"kind":"key","code":16,"value":1}"#,
        &mut guest,
    );
    assert_eq!(session.typed.get(&NOTEPAD).copied().unwrap_or(0), 1);
    assert_eq!(session.typed.get(&CALC).copied().unwrap_or(0), 1);
    assert!(guest.iter().find(|w| w.id == CALC).expect("calc").focused);
    assert!(!guest.iter().find(|w| w.id == NOTEPAD).expect("np").focused);
}

#[test]
fn modal_dialog_attaches_to_its_parent() {
    let mut dlg = win(DIALOG, "Open", 40, 40, 280, 180, 2, true);
    dlg.parent_id = Some(NOTEPAD);
    dlg.kind = WindowKind::Dialog;
    let mut session = HostSession::new();
    session.apply(&[notepad(), dlg.clone()], &HashMap::new());
    assert_eq!(session.hosts[&DIALOG].parent, Some(NOTEPAD));
    assert!(session.hosts.contains_key(&NOTEPAD));
    assert_eq!(session.hosts[&DIALOG].rec.kind, WindowKind::Dialog);
}

#[test]
fn thirty_minute_poll_does_not_leak_host_windows_or_frame_files() {
    let dir = std::env::temp_dir().join(format!(
        "ternvale-seamless-leak-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("dir");
    let mut session = HostSession::new();
    let windows = vec![notepad(), calculator()];
    let frames = vec![
        WindowPixels {
            id: NOTEPAD,
            seq: 1,
            width: 2,
            height: 2,
            format: "rgb24".into(),
            data: solid(1, 2, 3, 2, 2),
        },
        WindowPixels {
            id: CALC,
            seq: 1,
            width: 2,
            height: 2,
            format: "rgb24".into(),
            data: solid(4, 5, 6, 2, 2),
        },
    ];
    let mut peak = 0usize;
    for seq in 0..POLL_CYCLES_30MIN {
        let mut round = frames.clone();
        for f in &mut round {
            f.seq = seq as u64;
        }
        let _meta = persist_frames(&dir, round);
        session.apply(&windows, &HashMap::new());
        peak = peak.max(session.hosts.len());
    }
    assert_eq!(peak, 2);
    assert_eq!(session.hosts.len(), 2);
    let files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    assert_eq!(
        files.len(),
        2,
        "frames overwrite per id, not one file per poll"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn toggling_seamless_off_returns_to_single_window_mode() {
    let mut session = HostSession::new();
    session.apply(&[notepad(), calculator()], &HashMap::new());
    assert_eq!(session.hosts.len(), 2);
    assert!(!session.embedded);
    session.stop();
    assert!(session.hosts.is_empty());
    assert!(session.embedded);
    assert!(session.typed.is_empty());
}
