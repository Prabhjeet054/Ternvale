//! Log capture for tests: one process-wide TRACE subscriber whose writer
//! appends to the calling thread's buffer, if it has one (as in ternvale-tpm).
//!
//! Scoped subscribers are unreliable under the parallel test runner: tracing
//! caches each callsite's interest globally.

use std::cell::RefCell;
use std::io::Write;
use std::sync::Once;

thread_local! {
    static SINK: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

struct ThreadSink;

impl Write for ThreadSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        SINK.with(|sink| {
            if let Some(sink) = sink.borrow_mut().as_mut() {
                sink.extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run `f` and return what this thread logged meanwhile, every level, no ANSI colours.
pub(crate) fn capture<T>(f: impl FnOnce() -> T) -> (T, String) {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(|| ThreadSink)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("one global test subscriber");
    });
    SINK.with(|sink| *sink.borrow_mut() = Some(Vec::new()));
    let value = f();
    let bytes = SINK
        .with(|sink| sink.borrow_mut().take())
        .unwrap_or_default();
    (value, String::from_utf8(bytes).expect("utf8 log"))
}
