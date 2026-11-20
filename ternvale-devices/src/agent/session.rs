//! Session thread: handshake with one agent connection, then read its messages.

use std::os::unix::net::UnixStream;
use std::time::Instant;

use ternvale_agent_proto::{
    negotiate, FrameError, FrameReader, Message, MAX_NAME, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION,
};

use super::status::{millis, Book, Conn};
use super::Shared;

pub(super) fn run(shared: &Shared, id: u64, mut stream: UnixStream) {
    let span =
        tracing::info_span!(target: "ternvale::agent", "agent-conn", vm = %shared.vm, conn = id);
    let _entered = span.enter();
    let reason = match serve(shared, id, &mut stream) {
        Ok(()) => "agent server stopped".to_string(),
        Err(reason) => reason,
    };
    let mut book = shared.book();
    if let Some(mut conn) = book.conn.take_if(|conn| conn.id == id) {
        conn.fail(reason.clone());
        record_end(&mut book, conn, &reason);
    }
}

/// Book-keeping when connection `conn` ends.
pub(super) fn record_end(book: &mut Book, conn: Conn, why: &str) {
    if let Some(wait) = book.window_wait.take() {
        let _ = wait.send(Err(why.to_string()));
    }
    if let Some(wait) = book.driver_wait.take() {
        let _ = wait.send(Err(why.to_string()));
    }
    if let Some((_, wait)) = book.exec_wait.take() {
        let _ = wait.send(super::xfer::ExecEvent::Failed {
            reason: why.to_string(),
        });
    }
    if let Some((_, wait)) = book.copy_wait.take() {
        let _ = wait.send(super::xfer::CopyEvent::Failed {
            reason: why.to_string(),
        });
    }
    let reason = conn.failure.unwrap_or_else(|| why.to_string());
    let connected_ms = millis(conn.since.elapsed());
    if conn.hello.is_some() {
        book.disconnects += 1;
        tracing::info!(target: "ternvale::agent", conn = conn.id, reason = %reason, connected_ms, "agent disconnected");
    } else {
        tracing::info!(target: "ternvale::agent", conn = conn.id, reason = %reason, "agent connection closed before handshake");
    }
    book.last_error = Some(reason);
}

/// `Ok` when the server is stopping; `Err(reason)` when the connection ends.
fn serve(shared: &Shared, id: u64, stream: &mut UnixStream) -> Result<(), String> {
    stream
        .set_read_timeout(Some(shared.config.poll))
        .map_err(|error| format!("set read timeout: {error}"))?;
    let mut reader = FrameReader::new("agent");
    let deadline = Instant::now() + shared.config.hello_timeout;
    let (version, os, agent) = loop {
        match next(shared, &mut reader, stream, Some(deadline))? {
            None => return Ok(()),
            Some(Message::Hello { version, os, agent }) => break (version, os, agent),
            Some(other) => {
                tracing::warn!(target: "ternvale::agent", kind = other.kind(), "agent message before hello ignored");
            }
        }
    };
    let version = match negotiate(version) {
        Ok(version) => version,
        Err(error) => {
            let reject = Message::Reject {
                reason: truncate(error.to_string()),
                min_version: MIN_PROTOCOL_VERSION,
                max_version: PROTOCOL_VERSION,
            };
            if let Err(send) = shared.write(id, &reject) {
                tracing::warn!(target: "ternvale::agent", error = %send, "could not send reject");
            }
            return Err(format!("rejected: {error}"));
        }
    };
    shared
        .write(id, &Message::Welcome { version })
        .map_err(|error| error.to_string())?;
    {
        let mut book = shared.book();
        let Some(conn) = book.conn(id) else {
            return Err("replaced during handshake".into());
        };
        conn.hello = Some((version, os.clone(), agent.clone()));
        conn.since = Instant::now();
        conn.last_ping = conn.since;
        book.connects += 1;
        book.ever_connected = true;
    }
    tracing::info!(target: "ternvale::agent", version, os = %os, agent = %agent, "agent connected");
    if version >= 2 {
        for (tag, mount_point) in shared.shares.clone() {
            tracing::info!(target: "ternvale::agent", tag = %tag, mount_point = %mount_point, "sending share_mount to agent");
            let message = Message::ShareMount { tag, mount_point };
            if let Err(error) = message.validate() {
                tracing::warn!(target: "ternvale::agent", error = %error, "share_mount skipped");
                continue;
            }
            shared
                .write(id, &message)
                .map_err(|error| error.to_string())?;
        }
    }
    loop {
        match next(shared, &mut reader, stream, None)? {
            None => return Ok(()),
            Some(Message::Pong { seq }) => pong(shared, id, seq),
            Some(Message::Ping { seq }) => shared
                .write(id, &Message::Pong { seq })
                .map_err(|error| error.to_string())?,
            Some(Message::PointerMode { relative }) => {
                shared
                    .relative_pointer
                    .store(relative, std::sync::atomic::Ordering::Relaxed);
                tracing::info!(
                    target: "ternvale::agent",
                    relative,
                    "guest agent pointer mode"
                );
            }
            Some(Message::ClipboardSet { text }) => {
                let sink =
                    ternvale_vmm::lockwatch::lock(&shared.clipboard_sink, "agent-clipboard-sink")
                        .clone();
                if let Some(sink) = sink {
                    tracing::debug!(
                        target: "ternvale::agent",
                        len = text.0.len(),
                        "guest clipboard pushed to host"
                    );
                    sink(text.0);
                } else {
                    tracing::debug!(
                        target: "ternvale::agent",
                        len = text.0.len(),
                        "guest clipboard_set ignored (no host sink)"
                    );
                }
            }
            Some(Message::ClipboardGetOk { text }) => {
                let mut book = shared.book();
                if let Some(wait) = book.clipboard_wait.take() {
                    let _ = wait.send(Ok(text));
                } else {
                    tracing::debug!(target: "ternvale::agent", len = text.0.len(), "clipboard_get_ok without a waiter");
                }
            }
            Some(Message::WindowListOk { windows }) => {
                let mut book = shared.book();
                if let Some(wait) = book.window_wait.take() {
                    let _ = wait.send(Ok(windows));
                } else {
                    tracing::debug!(target: "ternvale::agent", count = windows.len(), "window_list_ok without a waiter");
                }
            }
            Some(Message::AppListOk { apps }) => {
                let mut book = shared.book();
                if let Some(wait) = book.app_wait.take() {
                    let _ = wait.send(Ok(apps));
                } else {
                    tracing::debug!(target: "ternvale::agent", count = apps.len(), "app_list_ok without a waiter");
                }
            }
            Some(Message::AppLaunchOk { id }) => {
                let mut book = shared.book();
                if let Some(wait) = book.launch_wait.take() {
                    let _ = wait.send(Ok(()));
                } else {
                    tracing::debug!(target: "ternvale::agent", id = %id, "app_launch_ok without a waiter");
                }
            }
            Some(Message::AppLaunchErr { id, reason }) => {
                let mut book = shared.book();
                if let Some(wait) = book.launch_wait.take() {
                    let _ = wait.send(Err(reason));
                } else {
                    tracing::debug!(target: "ternvale::agent", id = %id, "app_launch_err without a waiter");
                }
            }
            Some(Message::HostOpen { kind, target }) => {
                let sink =
                    ternvale_vmm::lockwatch::lock(&shared.host_open_sink, "agent-host-open-sink")
                        .clone();
                let result = if let Some(sink) = sink {
                    sink(kind.clone(), target)
                } else {
                    Err("host open is not configured".into())
                };
                let reply = match result {
                    Ok(()) => Message::HostOpenOk { kind },
                    Err(reason) => {
                        tracing::warn!(
                            target: "ternvale::agent",
                            kind = %kind,
                            reason = %reason,
                            "host_open refused"
                        );
                        Message::HostOpenErr { kind, reason }
                    }
                };
                if let Err(error) = shared.write(id, &reply) {
                    tracing::debug!(target: "ternvale::agent", error = %error, "host_open reply failed");
                }
            }
            Some(Message::WindowFrame {
                id: window_id,
                seq,
                offset,
                last,
                width,
                height,
                format,
                data,
            }) => {
                let written = data.0.len() as u64;
                {
                    let mut book = shared.book();
                    book.window_frames
                        .push(window_id, seq, offset, last, width, height, format, data.0);
                }
                if let Err(error) = shared.write(
                    id,
                    &Message::WindowFrameOk {
                        id: window_id,
                        seq,
                        written,
                    },
                ) {
                    tracing::debug!(target: "ternvale::agent", error = %error, "window_frame_ok send failed");
                }
            }
            Some(Message::WindowOpOk { id, op }) => {
                tracing::debug!(target: "ternvale::agent", id, op = %op, "window_op_ok");
            }
            Some(Message::WindowOpErr { id, reason }) => {
                tracing::warn!(target: "ternvale::agent", id, reason = %reason, "window_op_err");
            }
            Some(Message::DriverLogsOk { text, truncated }) => {
                let mut book = shared.book();
                if let Some(wait) = book.driver_wait.take() {
                    let _ = wait.send(Ok((text, truncated)));
                } else {
                    tracing::debug!(
                        target: "ternvale::agent",
                        bytes = text.len(),
                        truncated,
                        "driver_logs_ok without a waiter"
                    );
                }
            }
            Some(Message::ExecOutput { id, stream, data }) => {
                let book = shared.book();
                if let Some((want, wait)) = book.exec_wait.as_ref() {
                    if *want == id {
                        let _ = wait.send(super::xfer::ExecEvent::Output {
                            stream,
                            data: data.0,
                        });
                    }
                }
            }
            Some(Message::ExecExit { id, code }) => {
                let mut book = shared.book();
                if let Some((want, wait)) = book.exec_wait.take() {
                    if want == id {
                        let _ = wait.send(super::xfer::ExecEvent::Exit { code });
                    } else {
                        book.exec_wait = Some((want, wait));
                    }
                }
            }
            Some(Message::CopyPutOk { id, written }) => {
                deliver_copy(shared, id, super::xfer::CopyEvent::PutOk { written });
            }
            Some(Message::CopyGetOk { id, data, eof }) => {
                deliver_copy(
                    shared,
                    id,
                    super::xfer::CopyEvent::GetOk { data: data.0, eof },
                );
            }
            Some(Message::CopyHashOk { id, size, sha256 }) => {
                deliver_copy(shared, id, super::xfer::CopyEvent::HashOk { size, sha256 });
            }
            Some(Message::CopyErr { id, reason }) => {
                deliver_copy(shared, id, super::xfer::CopyEvent::Failed { reason });
            }
            Some(Message::CameraFrameOk { seq, written }) => {
                tracing::debug!(
                    target: "ternvale::agent",
                    seq,
                    written,
                    "camera_frame_ok"
                );
            }
            Some(Message::ClipboardOffer {
                seq, formats, echo, ..
            }) => {
                tracing::info!(
                    target: "ternvale::agent",
                    seq,
                    echo,
                    count = formats.len(),
                    "guest clipboard offer"
                );
            }
            Some(Message::ClipboardTake { seq, format }) => {
                tracing::debug!(target: "ternvale::agent", seq, format = %format, "guest clipboard take");
            }
            Some(Message::ClipboardPayload {
                seq,
                format,
                last,
                data,
                echo,
                ..
            }) => {
                tracing::debug!(
                    target: "ternvale::agent",
                    seq,
                    format = %format,
                    last,
                    echo,
                    n = data.0.len(),
                    "guest clipboard payload"
                );
                if last && format == "text" {
                    if let Ok(text) = String::from_utf8(data.0) {
                        let sink = ternvale_vmm::lockwatch::lock(
                            &shared.clipboard_sink,
                            "agent-clipboard-sink",
                        )
                        .clone();
                        if let Some(sink) = sink {
                            sink(text);
                        }
                    }
                }
            }
            Some(Message::ClipboardPayloadOk { seq, written }) => {
                tracing::trace!(target: "ternvale::agent", seq, written, "clipboard_payload_ok");
            }
            Some(Message::DragOffer {
                id,
                names,
                outbound,
                ..
            }) => {
                tracing::info!(
                    target: "ternvale::agent",
                    id = %id,
                    count = names.len(),
                    outbound,
                    "guest drag offer"
                );
            }
            Some(Message::DragAccept { id, dest }) => {
                tracing::info!(target: "ternvale::agent", id = %id, dest = %dest, "guest drag accept");
            }
            Some(Message::DragCancel { id, reason }) => {
                tracing::info!(target: "ternvale::agent", id = %id, reason = %reason, "guest drag cancel");
            }
            Some(Message::DragProgress { id, done, total }) => {
                tracing::debug!(target: "ternvale::agent", id = %id, done, total, "guest drag progress");
            }
            Some(Message::DragDone { id }) => {
                tracing::info!(target: "ternvale::agent", id = %id, "guest drag done");
            }
            Some(other) => {
                tracing::warn!(target: "ternvale::agent", kind = other.kind(), "unexpected message from agent ignored");
            }
        }
    }
}

fn deliver_copy(shared: &Shared, id: u64, event: super::xfer::CopyEvent) {
    let mut book = shared.book();
    if let Some((want, wait)) = book.copy_wait.take() {
        if want == id {
            let _ = wait.send(event);
        } else {
            book.copy_wait = Some((want, wait));
        }
    }
}

fn pong(shared: &Shared, id: u64, seq: u64) {
    let mut book = shared.book();
    let Some(conn) = book.conn(id) else {
        return;
    };
    match conn.outstanding {
        Some((expected, sent)) if expected == seq => {
            let now = Instant::now();
            let rtt = now.saturating_duration_since(sent);
            conn.outstanding = None;
            conn.last_pong = Some(now);
            conn.rtt = Some(rtt);
            book.pongs += 1;
            tracing::trace!(target: "ternvale::agent", seq, rtt_us = u64::try_from(rtt.as_micros()).unwrap_or(u64::MAX), "agent heartbeat answered");
        }
        outstanding => {
            tracing::warn!(target: "ternvale::agent", seq, expected = ?outstanding.map(|(seq, _)| seq), "unexpected pong ignored");
        }
    }
}

/// Next message, skipping unknown/malformed frames. `Ok(None)` when the
/// server is stopping; `Err(reason)` when the connection is over.
fn next(
    shared: &Shared,
    reader: &mut FrameReader,
    stream: &mut UnixStream,
    deadline: Option<Instant>,
) -> Result<Option<Message>, String> {
    loop {
        if shared.stopping() {
            return Ok(None);
        }
        match reader.read(stream) {
            Ok(message) => return Ok(Some(message)),
            Err(error) if error.is_skippable() => {}
            Err(error) if error.is_timeout() => {
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    tracing::warn!(target: "ternvale::agent", timeout_ms = millis(shared.config.hello_timeout), "agent sent no hello");
                    return Err(format!(
                        "no hello within {} ms",
                        millis(shared.config.hello_timeout)
                    ));
                }
            }
            Err(FrameError::Closed) => return Err("agent closed the connection".into()),
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn truncate(mut text: String) -> String {
    if text.len() > MAX_NAME {
        let mut end = MAX_NAME;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}
