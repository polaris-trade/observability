# observability-core

The runtime metrics gate plus hot-path telemetry helpers. A dependency leaf: `metrics`,
`hdrhistogram`, and an optional `tokio` flusher, nothing else. No `opentelemetry`, no
`tracing-subscriber`, so a feed receive loop can pull it without dragging in backend crates. All
backend wiring lives one crate up, in [`observability`](../observability).

## Why a separate crate

The hot path never touches a shared atomic once a thread has primed its gate mirror. Everything
sampled per-message lives in thread-local state and merges into shared registries only on a
periodic tick, so a flusher on another thread only ever sees data a worker already handed off,
never a worker's live local state.

## Runtime gate

A process-wide `AtomicBool` (`METRICS_ON`, starts OFF), mirrored per-thread into a
`Cell<Option<bool>>`.

```rust
pub static METRICS_ON: AtomicBool;
pub fn set_metrics_enabled(on: bool);   // flip the process-wide gate (Relaxed; advisory)
pub fn refresh_thread_gate();           // sync this thread's mirror from the flag
pub fn metrics_enabled() -> bool;       // hot-path check: reads primed mirror, else the atomic
```

Call `refresh_thread_gate` once per thread at startup, and again after any toggle a thread cares
about. `observability::init_metrics` flips `METRICS_ON` on a successful recorder install, so a
binary rarely calls `set_metrics_enabled` directly.

## Latency, counts, sampling

```rust
pub struct Timer(/* ... */);
pub fn timer(metric: &'static str) -> Timer;   // RAII; records elapsed secs on drop
pub fn record_latency_ns(ns: u64);             // thread-local histogram; no-op if gate off
pub fn count_msg();                            // per-thread Cell<u64>, wrapping
pub fn take_count() -> u64;                    // read + reset this thread's counter
pub fn should_sample(mask: u64) -> bool;       // 1-in-(mask+1) per-thread sampler
pub const SAMPLE_1_IN_8192: u64;               // mask = 8192 - 1
```

- `timer` reads the clock only when the gate is on; the `None` variant records nothing on drop.
- `record_latency_ns` lands in a thread-local `hdrhistogram::Histogram` bounded 1ns..60s at 3
  significant figures. It uses `saturating_record`, so a tail spike clamps into range instead of
  panicking.
- `should_sample` requires `mask == 2^n - 1`. It fires on a fresh thread's very first call, then
  exactly once per `mask + 1` consecutive calls.

```rust
use observability_core::{metrics_enabled, record_latency_ns, count_msg, should_sample, SAMPLE_1_IN_8192};

let t0 = std::time::Instant::now();
// decode ...
count_msg();
record_latency_ns(t0.elapsed().as_nanos() as u64);

if should_sample(SAMPLE_1_IN_8192) {
    // periodic, not every message
}
```

## Cross-thread aggregation

Per-thread state merges into shared registries on a worker tick, then a flusher drains them.

```rust
pub fn merge_local();                 // fold this thread's histogram + count into its shared slots
pub fn drain_all() -> Histogram<u64>; // fold every registered histogram into one snapshot, clearing each
pub fn drain_count() -> u64;          // sum every merged count, zeroing each slot
```

The first `merge_local` on a thread registers a fresh shared histogram and count slot. `drain_*`
only see data already handed off via `merge_local`; a thread that never merges never contributes.
Poisoned mutexes are recovered rather than propagated, so telemetry survives a panicked holder.
Each worker thread must call `merge_local` on its own tick.

## Optional tokio flusher

```toml
observability-core = { git = "...", tag = "observability-core-v0.2.0", features = ["flush-tokio"] }
```

```rust
// from an async context
observability_core::spawn_flusher("feed.decode_latency", "feed.msgs");
```

`spawn_flusher` runs a 100ms tokio loop that drains the histograms into `p50` / `p99` / `p999`
gauges and the counts into a counter, all via the `metrics` facade. Gauge quantiles do not
re-aggregate across process instances (averaging p99 across processes is wrong math); it targets
a single-process feed handler. This is the only feature and the only thing that pulls in `tokio`.

## Field keys

`field::*` exposes OTel-standard dotted attribute keys (`http.request.method`, `rpc.service`,
`error.type`, and so on) for structured log, span, and metric fields. No project-invented
`myapp.*` prefixes.

## License

MIT OR Apache-2.0.
