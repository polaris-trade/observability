//! Plain-data pipeline config. No `opentelemetry` type in any field; `otlp.rs` is the
//! only module allowed to know about the SDK.

use std::{net::SocketAddr, path::PathBuf};

use crate::otlp::OtlpConfig;

/// Top-level pipeline config: service identity, default log level, per-signal wiring.
#[derive(Clone, Debug)]
pub struct PipelineConfig {
    pub service_name: String,
    pub level: String,
    pub otlp: Option<OtlpConfig>,
    pub logging: Option<LoggingConfig>,
    pub tracing: Option<TracingConfig>,
}

/// Log fan-out across N sinks.
#[derive(Clone, Debug)]
pub struct LoggingConfig {
    pub sinks: Vec<LogSink>,
}

/// One logging sink: destination, wire format, optional per-sink level override.
#[derive(Clone, Debug)]
pub struct LogSink {
    pub kind: LogSinkKind,
    pub format: LogFormat,
    pub level: Option<String>,
}

/// Sink destination.
#[derive(Clone, Debug)]
pub enum LogSinkKind {
    Stdout,
    RollingFile {
        dir: PathBuf,
        prefix: String,
        rotation: Rotation,
    },
    Otlp,
}

/// Log line wire format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Bunyan,
    Pretty,
}

/// Rolling-file rotation cadence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    Minutely,
    Hourly,
    Daily,
    Never,
}

/// Tracing subsystem config: which exporter backs the span pipeline.
#[derive(Clone, Debug)]
pub struct TracingConfig {
    pub exporter: TraceExporter,
}

/// Trace exporter choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceExporter {
    Otlp,
}

/// Metrics subsystem config: which recorder backs the `metrics` facade.
#[derive(Clone, Debug)]
pub struct MetricsConfig {
    pub exporter: MetricsExporter,
}

/// Metrics exporter choice: Prometheus pull endpoint or OTLP push bridge.
#[derive(Clone, Debug)]
pub enum MetricsExporter {
    Prometheus { bind: SocketAddr },
    Otlp,
}
