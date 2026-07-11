//! Subscriber subsystem: one `tracing_subscriber::Registry` fusing a logging fan-out
//! (N sinks at once) with a single tracing OTLP sink.
//!
//! Logging fans out to every configured sink (stdout, rolling file, OTLP-log bridge),
//! each with its own format and level filter; every non-blocking file sink hands back a
//! `WorkerGuard` retained in `PipelineGuard` so tail lines flush on shutdown. Tracing is a
//! single OTLP span sink by design, never fanned out, never on disk. Both read transport
//! from the shared `OtlpConfig`; that is the only runtime coupling with the metrics recorder.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter, Layer, Registry, layer::SubscriberExt, util::SubscriberInitExt,
};

#[cfg(feature = "otel")]
use crate::otlp::OtlpConfig;
use crate::{
    config::{LogFormat, LogSinkKind, PipelineConfig, Rotation},
    error::PipelineError,
};

/// Holds everything whose drop must outlive the process: non-blocking file writer guards
/// and the OTel providers (dropping a provider flushes its pending batch).
pub struct PipelineGuard {
    // held only so the non-blocking writer threads flush their tail on drop; never read.
    #[allow(dead_code)]
    workers: Vec<WorkerGuard>,
    #[cfg(feature = "otel")]
    tracer_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
    #[cfg(feature = "otel-logs")]
    logger_provider: Option<opentelemetry_sdk::logs::SdkLoggerProvider>,
}

impl Drop for PipelineGuard {
    fn drop(&mut self) {
        // final flush before worker guards drop; provider drop also shuts down.
        #[cfg(feature = "otel")]
        if let Some(p) = &self.tracer_provider {
            let _ = p.force_flush();
        }
        #[cfg(feature = "otel-logs")]
        if let Some(p) = &self.logger_provider {
            let _ = p.force_flush();
        }
    }
}

/// Build the one Registry and install it as the global default via `try_init`.
///
/// A second call returns `PipelineError::AlreadyInit` instead of panicking. An OTLP
/// exporter that fails to build degrades to logging-only (warn on stderr, still `Ok`).
pub fn init_pipeline(cfg: PipelineConfig) -> Result<PipelineGuard, PipelineError> {
    let mut workers: Vec<WorkerGuard> = Vec::new();
    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = Vec::new();

    // bunyan needs one JsonStorageLayer in the registry, added once ahead of any bunyan sink.
    #[cfg(feature = "bunyan")]
    if cfg.logging.as_ref().is_some_and(|l| {
        l.sinks
            .iter()
            .any(|s| matches!(s.format, LogFormat::Bunyan))
    }) {
        layers.push(tracing_bunyan_formatter::JsonStorageLayer.boxed());
    }

    #[cfg(feature = "otel-logs")]
    let mut logger_provider = None;

    if let Some(logging) = &cfg.logging {
        for sink in &logging.sinks {
            let level = sink.level.as_deref().unwrap_or(&cfg.level);
            match &sink.kind {
                LogSinkKind::Stdout => {
                    let (writer, guard) = tracing_appender::non_blocking(std::io::stdout());
                    workers.push(guard);
                    layers.push(log_layer(&sink.format, &cfg.service_name, writer, level));
                }
                LogSinkKind::RollingFile {
                    dir,
                    prefix,
                    rotation,
                } => {
                    let appender = rolling_appender(dir, prefix, rotation);
                    let (writer, guard) = tracing_appender::non_blocking(appender);
                    workers.push(guard);
                    layers.push(log_layer(&sink.format, &cfg.service_name, writer, level));
                }
                LogSinkKind::Otlp => {
                    #[cfg(feature = "otel-logs")]
                    match cfg.otlp.as_ref() {
                        Some(otlp) => match build_logger_provider(otlp, &cfg.service_name) {
                            Ok(provider) => {
                                let bridge =
                                    opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(
                                        &provider,
                                    );
                                layers.push(bridge.with_filter(env_filter(level)).boxed());
                                logger_provider = Some(provider);
                            }
                            Err(e) => eprintln!(
                                "observability: otlp log exporter build failed, log sink skipped: {e}"
                            ),
                        },
                        None => {
                            eprintln!("observability: otlp log sink needs an otlp config, skipping")
                        }
                    }
                    #[cfg(not(feature = "otel-logs"))]
                    eprintln!(
                        "observability: otlp log sink configured but `otel-logs` feature off, skipping"
                    );
                }
            }
        }
    }

    // tracing: a single OTLP span sink, never fanned out.
    #[cfg(feature = "otel")]
    let mut tracer_provider = None;
    #[cfg(feature = "otel")]
    if let (Some(_tracing), Some(otlp)) = (&cfg.tracing, &cfg.otlp) {
        match build_tracer_provider(otlp, &cfg.service_name) {
            Ok(provider) => {
                use opentelemetry::trace::TracerProvider as _;
                let tracer = provider.tracer("observability");
                layers.push(tracing_opentelemetry::layer().with_tracer(tracer).boxed());
                tracer_provider = Some(provider);
            }
            Err(e) => {
                eprintln!("observability: otlp trace exporter build failed, tracing disabled: {e}")
            }
        }
    }

    Registry::default()
        .with(layers)
        .try_init()
        .map_err(|_| PipelineError::AlreadyInit)?;

    Ok(PipelineGuard {
        workers,
        #[cfg(feature = "otel")]
        tracer_provider,
        #[cfg(feature = "otel-logs")]
        logger_provider,
    })
}

fn env_filter(level: &str) -> EnvFilter {
    EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"))
}

fn rolling_appender(
    dir: &std::path::Path,
    prefix: &str,
    rotation: &Rotation,
) -> tracing_appender::rolling::RollingFileAppender {
    let rot = match rotation {
        Rotation::Minutely => tracing_appender::rolling::Rotation::MINUTELY,
        Rotation::Hourly => tracing_appender::rolling::Rotation::HOURLY,
        Rotation::Daily => tracing_appender::rolling::Rotation::DAILY,
        Rotation::Never => tracing_appender::rolling::Rotation::NEVER,
    };
    tracing_appender::rolling::RollingFileAppender::new(rot, dir, prefix)
}

// `service` feeds the bunyan formatter's app-name field; unused when that feature is off.
#[cfg_attr(not(feature = "bunyan"), allow(unused_variables))]
fn log_layer<W>(
    format: &LogFormat,
    service: &str,
    writer: W,
    level: &str,
) -> Box<dyn Layer<Registry> + Send + Sync>
where
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
{
    match format {
        LogFormat::Json => tracing_subscriber::fmt::layer()
            .json()
            .with_writer(writer)
            .with_filter(env_filter(level))
            .boxed(),
        LogFormat::Pretty => tracing_subscriber::fmt::layer()
            .pretty()
            .with_writer(writer)
            .with_filter(env_filter(level))
            .boxed(),
        LogFormat::Bunyan => {
            #[cfg(feature = "bunyan")]
            {
                tracing_bunyan_formatter::BunyanFormattingLayer::new(service.to_string(), writer)
                    .with_filter(env_filter(level))
                    .boxed()
            }
            #[cfg(not(feature = "bunyan"))]
            {
                eprintln!("observability: bunyan format needs `bunyan` feature, using json");
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(writer)
                    .with_filter(env_filter(level))
                    .boxed()
            }
        }
    }
}

#[cfg(feature = "otel")]
fn build_tracer_provider(
    otlp: &OtlpConfig,
    service: &str,
) -> Result<opentelemetry_sdk::trace::SdkTracerProvider, opentelemetry_otlp::ExporterBuildError> {
    let exporter = otlp.span_exporter()?;
    Ok(opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(otlp.otel_resource(service))
        .build())
}

#[cfg(feature = "otel-logs")]
fn build_logger_provider(
    otlp: &OtlpConfig,
    service: &str,
) -> Result<opentelemetry_sdk::logs::SdkLoggerProvider, opentelemetry_otlp::ExporterBuildError> {
    let exporter = otlp.log_exporter()?;
    Ok(opentelemetry_sdk::logs::SdkLoggerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(otlp.otel_resource(service))
        .build())
}
