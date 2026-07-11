# observability

Two-crate observability lib, reusable by any Rust project. [[crates/observability-core/src/lib.rs]] is the hot-path metrics core; [[crates/observability/src/lib.rs]] wires tracing and metrics backends on top of it.

`observability-core` depends only on `metrics` + `hdrhistogram` (+ optional `tokio` flusher), so a hot-path recv loop can pull it without dragging in `opentelemetry` or `tracing-subscriber`. All backend wiring lives one crate up, in `observability`.

## Two-subsystem split

[[crates/observability/src/lib.rs#init]] brings up two subsystems that share nothing at runtime except one `OtlpConfig`.

[[crates/observability/src/pipeline.rs#init_pipeline]] builds a single `tracing_subscriber::Registry`: a logging fan-out over every configured sink (stdout, rolling file, OTLP-log bridge), each with its own format and level filter, fused with exactly one tracing OTLP span sink. Tracing is never fanned out and never written to disk, by design; only logging fans out.

[[crates/observability/src/metrics.rs#init_metrics]] installs the `metrics`-facade global recorder (Prometheus pull or OTLP push) and flips the `observability-core` gate on success only; any error path leaves the gate untouched. It runs independently of the pipeline: `init` calls `init_pipeline` first (so a metrics failure is itself logged), and a metrics install failure downgrades to a `tracing::warn!` plus `metrics: None`, never taking down logging or tracing.

The only runtime coupling between the two subsystems is the `OtlpConfig` value `init` clones out of `PipelineConfig` before calling `init_pipeline`, then hands to `init_metrics` alongside the service name.

## Hot-path memory model (observability-core)

`observability-core` never touches a shared atomic on the hot path once a thread has primed its gate mirror. Everything sampled per-message lives in thread-local state, merged into shared registries only on a periodic tick.

The runtime gate is a process-wide [[crates/observability-core/src/lib.rs#METRICS_ON]] `AtomicBool`, mirrored per-thread into a `Cell<Option<bool>>` refreshed via `refresh_thread_gate`. The hot-path check reads the thread-local mirror first and only falls back to the shared atomic before that mirror has ever been primed on the calling thread.

Latency samples land in a thread-local bounded `hdrhistogram::Histogram` (1ns..60s, 3 sig figs, `saturating_record` so a tail spike clamps into range instead of panicking) via [[crates/observability-core/src/lib.rs#record_latency_ns]]. [[crates/observability-core/src/lib.rs#merge_local]] hands that thread's histogram, plus its message count, into a registry-backed shared slot: one slot per thread that has ever merged. [[crates/observability-core/src/lib.rs#drain_all]] folds every registered slot into one snapshot and clears each slot in place, so a flusher on another thread only ever sees data a worker thread has already merged, never a worker's live local state.

Message counting mirrors the same shape: a plain per-thread `Cell<u64>` on the hot path (`count_msg`/`take_count`), never a shared atomic, drained into the shared registry alongside the histogram on the same `merge_local` tick.

[[crates/observability-core/src/lib.rs#should_sample]] is a per-thread wraparound sampler (`mask` must be `2^n - 1`, e.g. `SAMPLE_1_IN_8192`) for hot-path call sites that need periodic-not-every-message sampling. It samples true on a fresh thread's very first call, then exactly once per `mask + 1` consecutive calls after that.

## Shared OTLP, three sibling exporters

[[crates/observability/src/otlp.rs#OtlpConfig]] is the single source of endpoint, protocol, headers, resource attrs, and timeout for every OTLP signal.

`pipeline.rs` and `metrics.rs` each call its exporter builders directly: `span_exporter` for tracing, `metric_exporter` (under `metrics-otel`) for the metrics bridge, `log_exporter` (under `otel-logs`) for the log bridge layer, all off the same endpoint/timeout/headers pair. No `opentelemetry` type crosses into a public `observability` signature; the builders stay `pub(crate)` and otel-gated.

`otel_resource` folds `service.name` plus every configured `resource` attribute onto whichever provider calls it, so span, metric, and log all carry identical resource attributes from one config, with no per-signal drift.

## Feature model

`otel` is the grouping feature. Per-signal features layer on top of it.

`otel` pulls in `opentelemetry`/`opentelemetry_sdk`/`opentelemetry-otlp`/`tracing-opentelemetry` and gates every `#[cfg(feature = "otel")]` block. `otel-traces` is a plain alias for `otel`. `otel-logs` adds the OTLP log bridge (`opentelemetry-appender-tracing`). `metrics-prom` adds the Prometheus pull exporter. `metrics-otel` adds the OTLP metrics bridge and implies `otel`. `bunyan` adds the bunyan-format log sink.

No feature is named `tracing` or `metrics`; those names stay reserved for the extern facade crate deps themselves, so a feature flag never shadows `tracing::` or `metrics::` path resolution.

The whole otel stack pins to opentelemetry 0.31: `metrics-exporter-otel` 0.3.1 pins `opentelemetry ^0.31`, and all three OTLP signal exporters plus the metrics bridge must resolve to one `opentelemetry` version, so that crate is the version bottleneck for the entire stack.

## See also

Related sections outside this file:

- [[tests]] documents test specifications for the runtime gate, sampler, per-thread counters, histogram merge, and OTel field keys.
