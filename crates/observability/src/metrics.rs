//! Metrics recorder subsystem.
//!
//! Installs the global `metrics`-facade recorder: Prometheus pull endpoint or OTLP push
//! bridge. Flips the `observability-core` gate on successful install only; any error path
//! returns before touching it. See [`crate::config::MetricsExporter`] for the choice.
//!
//! Module named `metrics`, same as the extern facade crate. Reach the facade with a
//! leading `::metrics::`; bare `metrics::` resolves here instead.

use std::net::SocketAddr;

#[cfg(feature = "metrics-otel")]
use opentelemetry::metrics::MeterProvider as _;
#[cfg(feature = "metrics-otel")]
use opentelemetry_sdk::metrics::SdkMeterProvider;

use crate::{
    config::{MetricsConfig, MetricsExporter},
    error::MetricsError,
    otlp::OtlpConfig,
};

/// OTLP meter provider slot. `Option<SdkMeterProvider>` when the OTLP metrics backend
/// is compiled in, unit otherwise, so [`MetricsGuard`] carries no per-feature field list.
#[cfg(feature = "metrics-otel")]
type OtelMeterProvider = Option<SdkMeterProvider>;
#[cfg(not(feature = "metrics-otel"))]
type OtelMeterProvider = ();

/// Handle for an installed metrics recorder.
///
/// Drop flips the `observability-core` gate off and, for the OTLP backend, shuts down
/// the meter provider so its final export batch flushes before the process exits.
pub struct MetricsGuard {
    // read only by `shutdown_otel` under `metrics-otel`; unit type otherwise.
    #[allow(dead_code)]
    otel_provider: OtelMeterProvider,
}

impl MetricsGuard {
    /// Flip gate on, return guard holding whatever provider state must outlive install.
    // no exporter feature -> init_metrics only ever returns Err, so `new` has no caller.
    #[cfg_attr(
        not(any(feature = "metrics-prom", feature = "metrics-otel")),
        allow(dead_code)
    )]
    fn new(otel_provider: OtelMeterProvider) -> Self {
        observability_core::set_metrics_enabled(true);
        Self { otel_provider }
    }

    #[cfg(feature = "metrics-otel")]
    fn shutdown_otel(&self) {
        if let Some(provider) = &self.otel_provider {
            let _ = provider.shutdown();
        }
    }

    #[cfg(not(feature = "metrics-otel"))]
    fn shutdown_otel(&self) {}
}

impl Drop for MetricsGuard {
    fn drop(&mut self) {
        observability_core::set_metrics_enabled(false);
        self.shutdown_otel();
    }
}

/// Install the recorder chosen by `cfg.exporter`.
///
/// `otlp` only matters for the OTLP arm; pass `None` for a Prometheus-only setup.
/// Gate flips true only on success; any error path leaves it off.
pub fn init_metrics(
    cfg: MetricsConfig,
    otlp: Option<&OtlpConfig>,
    service_name: &str,
) -> Result<MetricsGuard, MetricsError> {
    match cfg.exporter {
        MetricsExporter::Prometheus { bind } => install_prometheus(bind),
        MetricsExporter::Otlp => install_otlp(otlp, service_name),
    }
}

/// Box a plain message as the install-error source, for feature-disabled arms.
#[cfg(any(not(feature = "metrics-prom"), not(feature = "metrics-otel")))]
fn disabled(feature: &str, exporter: &str) -> MetricsError {
    MetricsError::Install(
        format!("{exporter} exporter requested but crate built without feature \"{feature}\"")
            .into(),
    )
}

#[cfg(feature = "metrics-prom")]
fn install_prometheus(bind: SocketAddr) -> Result<MetricsGuard, MetricsError> {
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .with_http_listener(bind)
        .install()
        .map_err(map_prometheus_build_error)?;
    // prometheus path holds no otel provider; the empty state is `()` without `metrics-otel`.
    #[allow(clippy::unit_arg)]
    Ok(MetricsGuard::new(Default::default()))
}

/// `FailedToCreateHTTPListener` is the only bind-shaped variant; the crate stringifies
/// the source `io::Error` before handing it back, so rewrap the message as `io::Error`
/// rather than lose the `Bind` classification.
#[cfg(feature = "metrics-prom")]
fn map_prometheus_build_error(err: metrics_exporter_prometheus::BuildError) -> MetricsError {
    match err {
        metrics_exporter_prometheus::BuildError::FailedToCreateHTTPListener(msg) => {
            MetricsError::Bind(std::io::Error::other(msg))
        }
        other => MetricsError::Install(Box::new(other)),
    }
}

#[cfg(not(feature = "metrics-prom"))]
fn install_prometheus(_bind: SocketAddr) -> Result<MetricsGuard, MetricsError> {
    Err(disabled("metrics-prom", "prometheus"))
}

#[cfg(feature = "metrics-otel")]
fn install_otlp(
    otlp: Option<&OtlpConfig>,
    service_name: &str,
) -> Result<MetricsGuard, MetricsError> {
    let otlp = otlp.ok_or_else(|| {
        MetricsError::Install("otlp exporter requested but no OtlpConfig was provided".into())
    })?;
    let exporter = otlp
        .metric_exporter()
        .map_err(|e| MetricsError::Install(Box::new(e)))?;
    let provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter)
        .with_resource(otlp.otel_resource(service_name))
        .build();
    let recorder =
        metrics_exporter_otel::OpenTelemetryRecorder::new(provider.meter("observability"));
    ::metrics::set_global_recorder(recorder).map_err(|e| MetricsError::Install(Box::new(e)))?;
    Ok(MetricsGuard::new(Some(provider)))
}

#[cfg(not(feature = "metrics-otel"))]
fn install_otlp(
    _otlp: Option<&OtlpConfig>,
    _service_name: &str,
) -> Result<MetricsGuard, MetricsError> {
    Err(disabled("metrics-otel", "otlp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // @lat: [[tests#Metrics recorder guard drop behavior#Drop flips the gate off]]
    #[test]
    fn drop_flips_gate_off() {
        let guard = MetricsGuard::new(Default::default());
        assert!(observability_core::metrics_enabled());
        drop(guard);
        assert!(!observability_core::metrics_enabled());
    }
}
