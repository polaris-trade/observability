# observability

Tracing and metrics backend wiring over [`observability-core`](../observability-core): a
subscriber pipeline, a metrics recorder, and one shared OTLP config. This is where every heavy
backend dependency lives, all feature-gated. Depend on this crate in a binary or service crate;
depend on `observability-core` alone in a hot-path library.

## Two subsystems, one coupling

`init` brings up two subsystems that share nothing at runtime except a single `OtlpConfig`.

### Subscriber pipeline (`init_pipeline`)

Builds one `tracing_subscriber::Registry` and installs it as the global default via `try_init`
(a second call returns `PipelineError::AlreadyInit` instead of panicking).

- **Logging fans out** across every configured `LogSink`: stdout, rolling file, or the OTLP-log
  bridge, each with its own `LogFormat` (`Json`, `Bunyan`, `Pretty`) and its own level filter.
  Every non-blocking file sink hands back a `WorkerGuard` retained in `PipelineGuard`, so tail
  lines flush on shutdown.
- **Tracing is a single OTLP span sink** by design: never fanned out, never written to disk.

An OTLP exporter that fails to build degrades to logging-only (a warning on stderr, still `Ok`).
Dropping `PipelineGuard` force-flushes the tracer and logger providers before the file worker
guards drop.

### Metrics recorder (`init_metrics`)

Installs the `metrics`-facade global recorder chosen by `MetricsExporter`:

- `Prometheus { bind }`: a pull endpoint on the given socket.
- `Otlp`: an OTLP push bridge over the shared `OtlpConfig`.

The `observability-core` gate flips true only on a successful install; any error path returns
before touching it. Dropping `MetricsGuard` flips the gate off and, for the OTLP backend, shuts
down the meter provider so its final export batch flushes.

### `init`: both at once

```rust
pub fn init(cfg: ObsConfig) -> Result<ObsGuard, PipelineError>;
```

`ObsConfig { pipeline, metrics }` where `metrics` is `Option`, so a build can run logging plus
tracing with no recorder. The pipeline comes up first (so a metrics warning is itself logged),
then metrics reuse the pipeline's cloned `OtlpConfig` and service name. A pipeline failure is
fatal and returned; a metrics failure downgrades to `tracing::warn!` plus `metrics: None`, never
taking down logging or tracing. Hold the returned `ObsGuard` for the process lifetime.

## Shared OTLP

`OtlpConfig` is the single source of endpoint, protocol (`Grpc` or `HttpProtobuf`), headers,
resource attributes, and timeout for every OTLP signal. It builds three sibling exporters,
`span_exporter`, `metric_exporter` (under `metrics-otel`), and `log_exporter` (under
`otel-logs`), off the same transport pair. No `opentelemetry` type crosses a public
`observability` signature: the builders stay `pub(crate)` and otel-gated. `otel_resource` folds
`service.name` plus every configured `resource` pair onto whichever provider calls it, so span,
metric, and log all carry identical resource attributes with no per-signal drift.

> Headers are wired on the http transport only. `tonic`'s `MetadataKey` is not re-exported by
> `opentelemetry-otlp` (only `MetadataMap` is), and this crate does not take a direct `tonic`
> dep, so a dynamic gRPC header key has no wiring path here.

## Feature model

`otel` is the grouping feature; per-signal features layer on top of it. No feature is named
`tracing` or `metrics`, so a flag never shadows `tracing::` or `metrics::` path resolution.

| Feature | Default | Pulls in | Effect |
| --- | :---: | --- | --- |
| `logging` | yes | none | stdout / rolling-file log sinks |
| `otel` | yes | `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `tracing-opentelemetry` | gates every OTLP block; backs the tracing span sink |
| `otel-traces` | no | `otel` | plain alias for `otel` |
| `otel-logs` | no | `otel` + `opentelemetry-appender-tracing` | OTLP log-bridge sink |
| `metrics-prom` | yes | `metrics-exporter-prometheus` | Prometheus pull recorder |
| `metrics-otel` | no | `otel` + `metrics-exporter-otel` | OTLP metrics push bridge |
| `bunyan` | no | `tracing-bunyan-formatter` | bunyan-format log sink |

`default = ["logging", "otel", "metrics-prom"]`.

The whole otel stack pins to opentelemetry 0.31: `metrics-exporter-otel` 0.3.1 pins
`opentelemetry ^0.31`, and all OTLP signal exporters plus the metrics bridge must resolve to one
`opentelemetry` version, so that crate is the version bottleneck. Do not bump one otel crate
alone.

## Errors

```rust
pub enum PipelineError { AlreadyInit }
pub enum MetricsError  { Bind(io::Error), Install(Box<dyn Error + Send + Sync>) }
```

`MetricsError::Install` is boxed so this crate stays decoupled from the concrete
Prometheus/OTLP recorder error types; the install site picks the source.

## Config surface

`PipelineConfig` and its parts are plain data with no `opentelemetry` type in any field:
`LoggingConfig` / `LogSink` / `LogSinkKind` / `LogFormat` / `Rotation`, `TracingConfig` /
`TraceExporter`, `MetricsConfig` / `MetricsExporter`, and `OtlpConfig` / `OtlpProtocol`. The
runtime gate helpers (`metrics_enabled`, `refresh_thread_gate`, `set_metrics_enabled`,
`METRICS_ON`) and the whole `observability_core` crate are re-exported, so a binary drives the
hot-path gate through `observability` without a second dependency line.

## Log level precedence

Each sink's level filter is resolved as: a valid `RUST_LOG` value first, then the sink's own
`level` (or the pipeline `level` fallback), then `"info"`. Setting `RUST_LOG` therefore overrides
**every** sink's level uniformly, since each layer builds its filter from the same env var; the
per-sink `level` applies only while `RUST_LOG` is unset. An empty `RUST_LOG=""` counts as set and
yields `EnvFilter`'s empty default, not the config level.

## Building without backends

Under `--no-default-features` the otel and recorder wiring is gated out; sinks that need a
disabled feature print a one-line stderr notice and are skipped (an OTLP log sink without
`otel-logs`, a bunyan sink without `bunyan`, which falls back to json). The
`--feature-powerset --depth 2` CI job builds the combinations.

## License

MIT OR Apache-2.0.
