//! Service thread: accept agent connections and run the heartbeat.

use std::io::ErrorKind;
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ternvale_agent_proto::{write_message, Message};

use super::status::{millis, Conn};
use super::{session, Shared};

const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

pub(super) fn run(shared: &std::sync::Arc<Shared>, listener: &UnixListener) {
    let span = tracing::info_span!(target: "ternvale::agent", "agent-server", vm = %shared.vm);
    let _entered = span.enter();
    let mut sessions: Vec<JoinHandle<()>> = Vec::new();
    let mut next_id = 0u64;
    while !shared.stopping() {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    next_id += 1;
                    if let Some(handle) = adopt(shared, next_id, stream) {
                        sessions.push(handle);
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => {
                    tracing::warn!(target: "ternvale::agent", error = %error, "agent accept failed");
                    break;
                }
            }
        }
        heartbeat(shared, Instant::now());
        reap(&mut sessions, false);
        std::thread::sleep(shared.config.poll);
    }
    if let Some(conn) = shared.book().conn.as_mut() {
        conn.fail("agent server stopped".into());
    }
    reap(&mut sessions, true);
    tracing::debug!(target: "ternvale::agent", "agent server loop stopped");
}

/// Make `stream` the current connection and start its session thread.
fn adopt(shared: &std::sync::Arc<Shared>, id: u64, stream: UnixStream) -> Option<JoinHandle<()>> {
    let writer = match prepare(&stream) {
        Ok(writer) => writer,
        Err(error) => {
            tracing::warn!(target: "ternvale::agent", conn = id, error = %error, "agent connection setup failed; dropped");
            return None;
        }
    };
    {
        let mut book = shared.book();
        if let Some(old) = book.conn.as_mut() {
            tracing::warn!(target: "ternvale::agent", old = old.id, new = id, "new agent connection replaces the current one");
            old.fail(format!("replaced by connection {id}"));
        }
        if let Some(old) = book.conn.take() {
            super::session::record_end(&mut book, old, "replaced");
        }
        book.conn = Some(Conn::new(id, writer));
    }
    tracing::info!(target: "ternvale::agent", conn = id, "agent connection accepted; waiting for hello");
    let session_shared = std::sync::Arc::clone(shared);
    let spawned = std::thread::Builder::new()
        .name(format!("agent-{id}"))
        .spawn(move || session::run(&session_shared, id, stream));
    match spawned {
        Ok(handle) => Some(handle),
        Err(error) => {
            tracing::error!(target: "ternvale::agent", conn = id, error = %error, "agent session thread spawn failed");
            let mut book = shared.book();
            if let Some(mut conn) = book.conn.take().filter(|conn| conn.id == id) {
                conn.fail(format!("spawn session thread: {error}"));
                super::session::record_end(&mut book, conn, "spawn failed");
            }
            None
        }
    }
}

fn prepare(stream: &UnixStream) -> std::io::Result<UnixStream> {
    stream.set_nonblocking(false)?;
    let writer = stream.try_clone()?;
    writer.set_write_timeout(Some(WRITE_TIMEOUT))?;
    Ok(writer)
}

/// Ping a connected agent every `heartbeat`; drop it if a pong is overdue.
fn heartbeat(shared: &Shared, now: Instant) {
    let mut book = shared.book();
    let Some(conn) = book.conn.as_mut().filter(|conn| conn.hello.is_some()) else {
        return;
    };
    if let Some((seq, sent)) = conn.outstanding {
        let waited = now.saturating_duration_since(sent);
        if waited >= shared.config.pong_timeout {
            tracing::warn!(target: "ternvale::agent", conn = conn.id, seq, waited_ms = millis(waited), "agent pong overdue; dropping connection");
            conn.fail(format!(
                "no pong within {} ms",
                millis(shared.config.pong_timeout)
            ));
        }
        return;
    }
    if now.saturating_duration_since(conn.last_ping) < shared.config.heartbeat {
        return;
    }
    let seq = conn.next_seq;
    conn.next_seq += 1;
    conn.last_ping = now;
    match write_message(&mut conn.writer, &Message::Ping { seq }, "agent") {
        Ok(()) => {
            conn.outstanding = Some((seq, now));
            book.pings += 1;
        }
        Err(error) => {
            tracing::warn!(target: "ternvale::agent", conn = conn.id, seq, error = %error, "agent ping failed; dropping connection");
            conn.fail(format!("send ping: {error}"));
        }
    }
}

/// Join finished session threads (all of them when `all`).
fn reap(sessions: &mut Vec<JoinHandle<()>>, all: bool) {
    let mut index = 0;
    while index < sessions.len() {
        if all || sessions[index].is_finished() {
            if sessions.swap_remove(index).join().is_err() {
                tracing::error!(target: "ternvale::agent", "agent session thread panicked");
            }
        } else {
            index += 1;
        }
    }
}
