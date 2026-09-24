//! `tracing-subscriber` layers for stderr and the log file.

use tracing::Subscriber;
use tracing_appender::non_blocking::NonBlocking;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;

use crate::error::LogError;

pub(crate) fn build(
    json_file: bool,
    directive: &str,
    file_writer: NonBlocking,
) -> Result<Box<dyn Subscriber + Send + Sync>, LogError> {
    let filter = EnvFilter::try_new(directive).map_err(|source| {
        tracing::error!(
            target: "ternvale::log",
            directive,
            error = %source,
            "invalid log filter directive"
        );
        LogError::InvalidFilter {
            directive: directive.to_string(),
            source,
        }
    })?;
    let stderr = fmt::layer()
        .with_ansi(true)
        .with_target(true)
        .with_thread_ids(true)
        .with_timer(fmt::time::SystemTime)
        .with_writer(std::io::stderr)
        .with_filter(filter.clone())
        .boxed();
    let file = file_layer(json_file, file_writer, filter);
    Ok(Box::new(Registry::default().with(stderr).with(file)))
}

fn file_layer<S>(
    json_file: bool,
    writer: NonBlocking,
    filter: EnvFilter,
) -> Box<dyn Layer<S> + Send + Sync>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    if json_file {
        fmt::layer()
            .json()
            .with_target(true)
            .with_thread_ids(true)
            .with_timer(fmt::time::SystemTime)
            .with_writer(writer)
            .with_filter(filter)
            .boxed()
    } else {
        fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_thread_ids(true)
            .with_timer(fmt::time::SystemTime)
            .with_writer(writer)
            .with_filter(filter)
            .boxed()
    }
}
