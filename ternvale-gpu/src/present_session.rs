//! One viewer connection: hello, then rate-capped frames and their acks.
//!
//! At most one frame is in flight: a new frame is sent on an FPS tick only
//! once the viewer acknowledged the previous one, so a slow viewer costs
//! dropped frames, never a growing backlog. Acks are read on a helper thread
//! (`ternvale-gpu-viewer-rx`) and timestamped on arrival.

use std::io::{ErrorKind, Read};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::present::{InputTarget, PresentConfig, PresentStats, Serving};
use crate::present_protocol::{ViewerMessage, WireError, MESSAGE_LEN, VERSION};
use crate::present_send::{next_dirty_scanout, send_capture_hint, send_cursor, send_frame};
use crate::present_window::LatencyWindow;
use crate::scanout::GpuDisplay;

/// How long a new viewer has to say hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
/// How long one frame write may block before the viewer counts as hung.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// When an unacknowledged frame earns a WARN.
const ACK_WARN_AFTER: Duration = Duration::from_secs(2);

/// Why a session ended badly.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionError {
    /// Socket I/O failed.
    #[error("{op}: {source}")]
    Io {
        op: &'static str,
        source: std::io::Error,
    },
    /// The viewer's hello was malformed or from another version.
    #[error("viewer hello: {0}")]
    Hello(WireError),
    /// The viewer spoke before saying hello.
    #[error("viewer sent {0:?} before hello")]
    NoHello(ViewerMessage),
    /// A later message was malformed.
    #[error("viewer message: {0}")]
    Wire(WireError),
    /// The frame does not fit the wire header.
    #[error("frame {width}x{height} with {bytes} pixel bytes does not fit the wire format")]
    Frame {
        width: u32,
        height: u32,
        bytes: usize,
    },
}

pub(crate) fn io(op: &'static str) -> impl FnOnce(std::io::Error) -> SessionError {
    move |source| SessionError::Io { op, source }
}

enum Event {
    Message(Result<ViewerMessage, WireError>, Instant),
    Closed(Option<std::io::Error>),
}

pub(crate) struct InFlight {
    pub(crate) sequence: u64,
    pub(crate) sent_at: Instant,
    pub(crate) updated: Option<Instant>,
    pub(crate) warned: bool,
    pub(crate) upload_bytes: u64,
}

/// Serve one viewer until it leaves, misbehaves or `stop` is set.
#[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
pub(crate) fn serve(
    mut stream: UnixStream,
    serving: &Serving<'_>,
) -> Result<&'static str, SessionError> {
    let Serving {
        display,
        config,
        stats,
        stop,
        input,
        metrics,
    } = *serving;
    stream
        .set_nonblocking(false)
        .map_err(io("make viewer socket blocking"))?;
    set_timeout("hello read", stream.set_read_timeout(Some(HELLO_TIMEOUT)));
    let mut hello = [0u8; MESSAGE_LEN];
    stream
        .read_exact(&mut hello)
        .map_err(io("read viewer hello"))?;
    match ViewerMessage::decode(&hello) {
        Ok(ViewerMessage::Hello { version }) if version == VERSION => {}
        Ok(ViewerMessage::Hello { version }) => {
            return Err(SessionError::Hello(WireError::Version { found: version }))
        }
        Ok(other) => return Err(SessionError::NoHello(other)),
        Err(e) => return Err(SessionError::Hello(e)),
    }
    stats.viewers.fetch_add(1, Ordering::Relaxed);
    tracing::debug!(target: "ternvale::gpu", version = VERSION, "display viewer hello accepted");
    set_timeout("clear read", stream.set_read_timeout(None));
    set_timeout("frame write", stream.set_write_timeout(Some(WRITE_TIMEOUT)));
    let reader = stream.try_clone().map_err(io("clone viewer socket"))?;
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let acks = std::thread::Builder::new()
            .name("ternvale-gpu-viewer-rx".into())
            .spawn_scoped(scope, move || {
                read_messages(reader, &tx, input, stats, display, config.scanout)
            })
            .map_err(io("spawn viewer reader thread"))?;
        let result = stream_frames(
            &mut stream,
            &rx,
            display,
            config,
            stats,
            stop,
            metrics,
            input,
        );
        if let Err(e) = stream.shutdown(Shutdown::Both) {
            if e.kind() != ErrorKind::NotConnected {
                tracing::debug!(target: "ternvale::gpu", error = %e, "shutdown of viewer socket failed");
            }
        }
        if acks.join().is_err() {
            tracing::error!(target: "ternvale::gpu", "display viewer reader thread panicked");
        }
        result
    })
}

/// macOS answers `setsockopt` with `EINVAL` once the peer has closed; the
/// next read or write then reports the close, so carry on.
fn set_timeout(which: &'static str, result: std::io::Result<()>) {
    if let Err(e) = result {
        tracing::debug!(target: "ternvale::gpu", which, error = %e,
            "could not set viewer socket timeout (peer already closed?)");
    }
}

/// Read viewer messages; input goes straight to `input` (not through the
/// frame loop, which may be waiting out an FPS tick), the rest to `tx`.
fn read_messages(
    mut stream: UnixStream,
    tx: &Sender<Event>,
    input: Option<&dyn InputTarget>,
    stats: &PresentStats,
    display: &GpuDisplay,
    scanout: usize,
) {
    let mut buf = [0u8; MESSAGE_LEN];
    loop {
        let event = match stream.read_exact(&mut buf) {
            Ok(()) => match ViewerMessage::decode(&buf) {
                Ok(ViewerMessage::Input(event)) => {
                    stats.inputs.fetch_add(1, Ordering::Relaxed);
                    tracing::trace!(target: "ternvale::gpu", input = ?event,
                        forwarded = input.is_some(), "viewer input");
                    if let Some(target) = input {
                        target.deliver(event);
                    }
                    continue;
                }
                Ok(ViewerMessage::SetSize { width, height }) => {
                    if let Err(error) = display.request_host_mode(scanout, width, height) {
                        tracing::warn!(target: "ternvale::gpu", %error, width, height,
                            scanout, "viewer set_size rejected");
                    }
                    continue;
                }
                Ok(ViewerMessage::SetSizeEx {
                    scanout: id,
                    width,
                    height,
                }) => {
                    if let Err(error) = display.request_host_mode(id as usize, width, height) {
                        tracing::warn!(target: "ternvale::gpu", %error, width, height,
                            scanout = id, "viewer set_size_ex rejected");
                    }
                    continue;
                }
                decoded => Event::Message(decoded, Instant::now()),
            },
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => Event::Closed(None),
            Err(e) => Event::Closed(Some(e)),
        };
        let last = matches!(event, Event::Closed(_));
        if tx.send(event).is_err() || last {
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn stream_frames(
    stream: &mut UnixStream,
    rx: &Receiver<Event>,
    display: &GpuDisplay,
    config: PresentConfig,
    stats: &PresentStats,
    stop: &AtomicBool,
    metrics: Option<&ternvale_metrics::Metrics>,
    input: Option<&dyn InputTarget>,
) -> Result<&'static str, SessionError> {
    let interval = config.interval();
    let mut next_tick = Instant::now();
    let n = display.scanouts();
    let mut last_sent: Vec<Option<u64>> = vec![None; n];
    let mut last_cursor: Option<u64> = None;
    let mut last_relative: Option<bool> = None;
    let mut in_flight: Option<InFlight> = None;
    let mut waited_on_viewer = false;
    let mut window = LatencyWindow::new(next_tick);
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok("presenter stopped");
        }
        let now = Instant::now();
        window.report_if_due(now, config.report_every);
        if now >= next_tick {
            next_tick = if now - next_tick > interval {
                now + interval
            } else {
                next_tick + interval
            };
            let cursor_seq = display.cursor_sequence();
            if last_cursor != Some(cursor_seq) && cursor_seq > 0 {
                match send_cursor(stream, display) {
                    Ok(seq) => last_cursor = Some(seq),
                    Err(e) => break Err(e),
                }
            }
            let relative = input.is_some_and(InputTarget::relative_wanted);
            if last_relative != Some(relative) {
                if last_relative.is_some() || relative {
                    if let Err(e) = send_capture_hint(stream, relative) {
                        break Err(e);
                    }
                }
                last_relative = Some(relative);
            }
            if let Some(scanout) = next_dirty_scanout(display, config, &last_sent) {
                if in_flight.is_some() {
                    waited_on_viewer = true;
                } else {
                    let sent = match send_frame(
                        stream,
                        display,
                        config,
                        scanout,
                        last_sent[scanout],
                        waited_on_viewer,
                        stats,
                        &mut window,
                        metrics,
                    ) {
                        Ok(sent) => sent,
                        Err(e) => break Err(e),
                    };
                    last_sent[scanout] = Some(sent.sequence);
                    in_flight = Some(sent);
                    waited_on_viewer = false;
                }
            }
        }
        if let Some(pending) = in_flight.as_mut() {
            let waited = pending.sent_at.elapsed();
            if !pending.warned && waited > ACK_WARN_AFTER {
                pending.warned = true;
                tracing::warn!(target: "ternvale::gpu", sequence = pending.sequence,
                    waited_ms = waited.as_millis() as u64,
                    "display viewer has not presented its frame; newer frames are held back");
            }
        }
        match rx.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
            Ok(Event::Message(Ok(message), at)) => {
                on_message(message, at, &mut in_flight, stats, &mut window, metrics)
            }
            Ok(Event::Message(Err(e), _)) => break Err(SessionError::Wire(e)),
            Ok(Event::Closed(None)) => break Ok("viewer closed the connection"),
            Ok(Event::Closed(Some(e))) => break Err(io("read from viewer")(e)),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Ok("viewer reader stopped"),
        }
    };
    window.report(Instant::now(), "session end");
    let had_in_flight = in_flight.is_some();
    if let Some(pending) = in_flight {
        tracing::warn!(
            target: "ternvale::gpu",
            sequence = pending.sequence,
            waited_ms = pending.sent_at.elapsed().as_millis() as u64,
            "display session ended with one frame still awaiting a presented ack"
        );
    }
    let sent = stats.sent.load(Ordering::Relaxed);
    let presented = stats.presented.load(Ordering::Relaxed);
    let unacked = sent.saturating_sub(presented);
    if unacked > 1 {
        tracing::warn!(
            target: "ternvale::gpu",
            sent,
            presented,
            unacked,
            "display IPC frame imbalance (sent minus presented acks)"
        );
    } else {
        tracing::debug!(
            target: "ternvale::gpu",
            sent,
            presented,
            in_flight = had_in_flight,
            "display session frame accounting"
        );
    }
    result
}

fn on_message(
    message: ViewerMessage,
    at: Instant,
    in_flight: &mut Option<InFlight>,
    stats: &PresentStats,
    window: &mut LatencyWindow,
    metrics: Option<&ternvale_metrics::Metrics>,
) {
    match message {
        ViewerMessage::Presented { sequence } => {
            let Some(frame) = in_flight.take_if(|f| f.sequence == sequence) else {
                tracing::warn!(target: "ternvale::gpu", sequence,
                    in_flight = ?in_flight.as_ref().map(|f| f.sequence),
                    "display viewer acknowledged a frame that is not in flight; ignoring");
                return;
            };
            let send_to_present_us = micros(at.saturating_duration_since(frame.sent_at));
            let flush_to_present_us = frame
                .updated
                .map(|updated| micros(at.saturating_duration_since(updated)));
            stats.presented.fetch_add(1, Ordering::Relaxed);
            window.presented(flush_to_present_us);
            if let Some(us) = flush_to_present_us {
                stats.record_latency(us);
                if let Some(metrics) = metrics {
                    metrics.record_present(0, 0, 0, Some(std::time::Duration::from_micros(us)));
                }
            }
            tracing::debug!(
                target: "ternvale::gpu",
                sequence,
                flush_to_present_us,
                send_to_present_us,
                upload_bytes = frame.upload_bytes,
                "frame presented"
            );
        }
        ViewerMessage::Hello { version } => {
            tracing::warn!(target: "ternvale::gpu", version, "display viewer repeated its hello; ignoring")
        }
        ViewerMessage::Input(_) => {}
        ViewerMessage::SetSize { .. } | ViewerMessage::SetSizeEx { .. } => {}
    }
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}
