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
    loop {
        match next(shared, &mut reader, stream, None)? {
            None => return Ok(()),
            Some(Message::Pong { seq }) => pong(shared, id, seq),
            Some(Message::Ping { seq }) => shared
                .write(id, &Message::Pong { seq })
                .map_err(|error| error.to_string())?,
            Some(other) => {
                tracing::warn!(target: "ternvale::agent", kind = other.kind(), "unexpected message from agent ignored");
            }
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
