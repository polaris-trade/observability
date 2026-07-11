//! Error types for pipeline init and metrics recorder install.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("global tracing subscriber already initialized")]
    AlreadyInit,
}

#[derive(Debug, Error)]
pub enum MetricsError {
    #[error("failed to bind metrics exporter socket")]
    Bind(#[source] std::io::Error),
    // boxed so error.rs stays decoupled from prometheus/otel recorder error types;
    // metrics.rs picks the concrete source at the install site
    #[error("failed to install metrics recorder")]
    Install(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
}
