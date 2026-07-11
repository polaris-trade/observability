//! OTLP export target shared by span, metric, and log exporters.
//!
//! One `OtlpConfig` builds three sibling exporters off the same endpoint/timeout/headers.
//! Exporter builders stay `pub(crate)` and otel-gated so no `opentelemetry` type leaks
//! into a public signature; `pipeline.rs`/`metrics.rs` call these to wire providers.
//!
//! Under `--no-default-features` the otel impl is gated out. `otel_resource` folds
//! `service.name` plus the configured `resource` attrs onto every signal provider, so one
//! `OtlpConfig` drives identical resource attributes across span, metric, and log.

use std::time::Duration;

#[cfg(feature = "otel")]
use opentelemetry::KeyValue;
#[cfg(feature = "otel")]
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};

/// Endpoint, transport, headers, resource attrs, and timeout for the OTLP collector.
#[derive(Clone, Debug)]
pub struct OtlpConfig {
    pub endpoint: String,
    pub protocol: OtlpProtocol,
    pub headers: Vec<(String, String)>,
    pub resource: Vec<(String, String)>,
    pub timeout: Duration,
}

/// Wire transport for the OTLP exporters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtlpProtocol {
    Grpc,
    HttpProtobuf,
}

#[cfg(feature = "otel")]
impl OtlpConfig {
    /// Build span exporter for `protocol`. Endpoint + timeout wired both transports.
    // NOTE: headers wired on http only; tonic's MetadataKey isn't re-exported by
    // opentelemetry-otlp (only MetadataMap is), so a dynamic key needs `tonic` as a
    // direct dep, which this crate does not take.
    pub(crate) fn span_exporter(
        &self,
    ) -> Result<opentelemetry_otlp::SpanExporter, opentelemetry_otlp::ExporterBuildError> {
        match self.protocol {
            OtlpProtocol::Grpc => opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(&self.endpoint)
                .with_timeout(self.timeout)
                .build(),
            OtlpProtocol::HttpProtobuf => opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_endpoint(&self.endpoint)
                .with_timeout(self.timeout)
                .with_headers(self.http_headers())
                .build(),
        }
    }

    /// Build metric exporter for `protocol`. Cumulative temporality (SDK default).
    #[cfg(feature = "metrics-otel")]
    pub(crate) fn metric_exporter(
        &self,
    ) -> Result<opentelemetry_otlp::MetricExporter, opentelemetry_otlp::ExporterBuildError> {
        use opentelemetry_sdk::metrics::Temporality;
        match self.protocol {
            OtlpProtocol::Grpc => opentelemetry_otlp::MetricExporter::builder()
                .with_tonic()
                .with_temporality(Temporality::default())
                .with_endpoint(&self.endpoint)
                .with_timeout(self.timeout)
                .build(),
            OtlpProtocol::HttpProtobuf => opentelemetry_otlp::MetricExporter::builder()
                .with_http()
                .with_temporality(Temporality::default())
                .with_endpoint(&self.endpoint)
                .with_timeout(self.timeout)
                .with_headers(self.http_headers())
                .build(),
        }
    }

    /// Build log exporter for `protocol`. Endpoint + timeout wired both transports.
    #[cfg(feature = "otel-logs")]
    pub(crate) fn log_exporter(
        &self,
    ) -> Result<opentelemetry_otlp::LogExporter, opentelemetry_otlp::ExporterBuildError> {
        match self.protocol {
            OtlpProtocol::Grpc => opentelemetry_otlp::LogExporter::builder()
                .with_tonic()
                .with_endpoint(&self.endpoint)
                .with_timeout(self.timeout)
                .build(),
            OtlpProtocol::HttpProtobuf => opentelemetry_otlp::LogExporter::builder()
                .with_http()
                .with_endpoint(&self.endpoint)
                .with_timeout(self.timeout)
                .with_headers(self.http_headers())
                .build(),
        }
    }

    /// Headers as a map for the http transport's `with_headers`.
    fn http_headers(&self) -> std::collections::HashMap<String, String> {
        self.headers.iter().cloned().collect()
    }

    /// Resource shared by all three signal providers: `service.name` plus every configured
    /// `resource` pair. One config, identical attributes on span/metric/log.
    pub(crate) fn otel_resource(&self, service_name: &str) -> opentelemetry_sdk::Resource {
        let mut builder =
            opentelemetry_sdk::Resource::builder().with_service_name(service_name.to_string());
        for (key, value) in &self.resource {
            builder = builder.with_attribute(KeyValue::new(key.clone(), value.clone()));
        }
        builder.build()
    }
}
