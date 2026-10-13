//! Where a backend's commands run: inline in the `START` write, or on a
//! worker thread for a backend that waits on host I/O (swtpm's socket).
//!
//! With a worker, `START` keeps reading 1 until [`Runner::poll`] returns the
//! response. Guests poll `START` for completion (PTP §6.5.3, "start" field;
//! EDK2 `PtpCrbTpmCommand`, Linux `crb_send` / `crb_req_canceled`), so the
//! CRB needs no interrupt: the response is collected on the guest's next
//! register access. TODO(verify): the PTP subsection number.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;

use crate::backend::{error_response, TpmBackend, RC_FAILURE, ST_NO_SESSIONS};
use crate::TpmError;

/// One command for the worker.
struct Job {
    locality: u8,
    command: Vec<u8>,
}

/// A backend and the thread its commands run on.
pub(crate) struct Worker {
    name: String,
    jobs: Option<Sender<Job>>,
    done: Receiver<Vec<u8>>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    /// Move `backend` to a new thread named `tpm-<backend name>`.
    fn spawn(mut backend: Box<dyn TpmBackend>) -> Result<Self, TpmError> {
        let name = backend.name().to_string();
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (outbox, done) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name(format!("tpm-{name}"))
            .spawn(move || {
                for job in inbox {
                    let response = backend.execute(job.locality, &job.command);
                    if outbox.send(response).is_err() {
                        break;
                    }
                }
                tracing::debug!(target: "ternvale::tpm", backend = backend.name(), "tpm backend worker stopped");
            })
            .map_err(|error| {
                tracing::error!(target: "ternvale::tpm", backend = %name, error = %error, "could not start the tpm backend worker");
                TpmError::Worker {
                    reason: error.to_string(),
                }
            })?;
        tracing::debug!(target: "ternvale::tpm", backend = %name, "tpm backend worker started");
        Ok(Self {
            name,
            jobs: Some(jobs),
            done,
            thread: Some(thread),
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Closing the queue ends the worker loop after any running command;
        // the backend is dropped on the worker thread.
        self.jobs.take();
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::error!(target: "ternvale::tpm", backend = %self.name, "tpm backend worker panicked");
            }
        }
    }
}

/// How the CRB runs commands.
pub(crate) enum Runner {
    /// On the caller's thread, inside the `START` write.
    Inline(Box<dyn TpmBackend>),
    /// On a worker thread.
    Worker(Worker),
}

impl Runner {
    /// A worker for a backend with host I/O, inline otherwise.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(backend = backend.name(), host_io = backend.host_io()))]
    pub(crate) fn new(backend: Box<dyn TpmBackend>) -> Result<Self, TpmError> {
        if backend.host_io() {
            Worker::spawn(backend).map(Self::Worker)
        } else {
            Ok(Self::Inline(backend))
        }
    }

    #[tracing::instrument(level = "trace", target = "ternvale::tpm", skip_all)]
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Inline(backend) => backend.name(),
            Self::Worker(worker) => &worker.name,
        }
    }

    #[tracing::instrument(level = "trace", target = "ternvale::tpm", skip_all)]
    pub(crate) fn is_worker(&self) -> bool {
        matches!(self, Self::Worker(_))
    }

    /// Run `command`: the response now (inline), or `None` while a worker
    /// runs it.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip(self, command), fields(len = command.len()))]
    pub(crate) fn run(&mut self, locality: u8, command: Vec<u8>) -> Option<Vec<u8>> {
        match self {
            Self::Inline(backend) => Some(backend.execute(locality, &command)),
            Self::Worker(worker) => {
                let queued = worker
                    .jobs
                    .as_ref()
                    .is_some_and(|jobs| jobs.send(Job { locality, command }).is_ok());
                if queued {
                    None
                } else {
                    tracing::error!(target: "ternvale::tpm", backend = %worker.name, "tpm backend worker is gone; answered TPM_RC_FAILURE");
                    Some(error_response(ST_NO_SESSIONS, RC_FAILURE))
                }
            }
        }
    }

    /// The finished response of the command a worker is running, if any.
    #[tracing::instrument(level = "trace", target = "ternvale::tpm", skip_all)]
    pub(crate) fn poll(&mut self) -> Option<Vec<u8>> {
        let Self::Worker(worker) = self else {
            return None;
        };
        match worker.done.try_recv() {
            Ok(response) => Some(response),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                tracing::error!(target: "ternvale::tpm", backend = %worker.name, "tpm backend worker stopped mid-command; answered TPM_RC_FAILURE");
                Some(error_response(ST_NO_SESSIONS, RC_FAILURE))
            }
        }
    }
}
