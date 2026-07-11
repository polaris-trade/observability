//! Tracing + metrics backend wiring over the `observability-core` gate.
//!
//! Two independent subsystems that share nothing at runtime except one `OtlpConfig`:
//!
//! - subscriber pipeline: one `tracing_subscriber::Registry`, logging fan-out over N sinks
//!   plus a single tracing OTLP sink (`pipeline`).
//! - metrics recorder: a separate `metrics` global recorder, Prometheus pull or OTLP bridge,
//!   flipping the `observability-core` gate on install (`metrics`).
//!
//! `init` is the thin convenience that runs both. See module docs for each subsystem.

mod config;
mod error;
mod metrics;
mod otlp;
mod pipeline;

pub use config::{
    LogFormat, LogSink, LogSinkKind, LoggingConfig, MetricsConfig, MetricsExporter, PipelineConfig,
    Rotation, TraceExporter, TracingConfig,
};
pub use error::{MetricsError, PipelineError};
pub use metrics::{MetricsGuard, init_metrics};
// hot-path gate lives in the leaf crate; re-export so a binary drives it through `observability`.
pub use observability_core;
pub use observability_core::{
    METRICS_ON, metrics_enabled, refresh_thread_gate, set_metrics_enabled,
};
pub use otlp::{OtlpConfig, OtlpProtocol};
pub use pipeline::{PipelineGuard, init_pipeline};

/// Both subsystems in one value. `metrics` is optional so a build can run logging + tracing
/// without a recorder.
pub struct ObsConfig {
    pub pipeline: PipelineConfig,
    pub metrics: Option<MetricsConfig>,
}

/// Held for the process lifetime. Drop flushes the pipeline's OTel batches and file writers,
/// then flips the metrics gate off.
pub struct ObsGuard {
    pub pipeline: PipelineGuard,
    pub metrics: Option<MetricsGuard>,
}

/// Bring up both subsystems: pipeline first (so a metrics warning is itself logged), then
/// metrics sharing the pipeline's `OtlpConfig` and service name.
///
/// A pipeline failure is fatal and returned. A metrics failure is downgraded to a
/// `tracing::warn!` and `metrics: None`, never taking down logging or tracing.
pub fn init(cfg: ObsConfig) -> Result<ObsGuard, PipelineError> {
    // clone the two values metrics needs before the pipeline config is moved into init.
    let service_name = cfg.pipeline.service_name.clone();
    let otlp = cfg.pipeline.otlp.clone();

    let pipeline = init_pipeline(cfg.pipeline)?;

    let metrics = match cfg.metrics {
        Some(mcfg) => match init_metrics(mcfg, otlp.as_ref(), &service_name) {
            Ok(guard) => Some(guard),
            Err(e) => {
                tracing::warn!(error = %e, "metrics init failed, running without metrics");
                None
            }
        },
        None => None,
    };

    Ok(ObsGuard { pipeline, metrics })
}
