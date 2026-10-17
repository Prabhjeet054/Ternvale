//! Viewer input through a session: delivered to the [`InputTarget`] as it
//! arrives, malformed input ends the session.

use std::io::Write;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::tests::{display, session, session_input, update, FakeViewer};
use super::*;
use crate::present_protocol::{PointerButton, ViewerInput, ViewerMessage};

#[derive(Default)]
pub(super) struct Recorder(pub(super) std::sync::Mutex<Vec<ViewerInput>>);

impl InputTarget for Recorder {
    fn deliver(&self, input: ViewerInput) {
        self.0.lock().expect("recorder").push(input);
    }
}

#[test]
fn viewer_input_reaches_target_without_an_ack() {
    let display = display();
    update(&display, 1, 1);
    let recorder = Recorder::default();
    let sent = [
        ViewerInput::Key {
            mac_keycode: 0,
            pressed: true,
        },
        ViewerInput::Pointer { x: 7, y: 32767 },
        ViewerInput::Button {
            button: PointerButton::Right,
            pressed: false,
        },
        ViewerInput::Scroll { dx: -2, dy: 1 },
    ];
    let (result, logs, stats) = session_input(
        PresentConfig::default(),
        &display,
        Some(&recorder),
        move |stream| {
            let mut viewer = FakeViewer::hello(stream);
            let (first, _) = viewer.frame();
            for input in sent {
                viewer.send(ViewerMessage::Input(input));
            }
            viewer.ack(&first);
            std::thread::sleep(Duration::from_millis(30));
        },
    );
    assert_eq!(result, Ok("viewer closed the connection"));
    assert_eq!(*recorder.0.lock().expect("recorder"), sent);
    assert_eq!(stats.inputs.load(Ordering::Relaxed), 4);
    assert_eq!(
        stats.presented.load(Ordering::Relaxed),
        1,
        "acks still flow"
    );
    assert!(logs.contains("frame presented"), "{logs}");
}

#[test]
fn malformed_input_ends_the_session() {
    let display = display();
    let (result, _, _) = session(30, &display, |stream| {
        let mut viewer = FakeViewer::hello(stream);
        let mut bad = ViewerMessage::Input(ViewerInput::Pointer { x: 0, y: 0 }).encode();
        bad[8..10].copy_from_slice(&0xffffu16.to_le_bytes());
        viewer.0.write_all(&bad).expect("send");
        std::thread::sleep(Duration::from_millis(30));
    });
    let err = result.expect_err("refused");
    assert!(err.contains("invalid value"), "{err}");
}
