---
lat:
  require-code-mention: true
---
# Tests

Behavioral test specifications for the hot-path gate, sampler, counters, histogram merge, and OTel field keys in `observability-core`, plus the metrics recorder guard in `observability`.

## Runtime gate

The process-wide gate mirrors into a per-thread `Cell` so the hot path never re-reads the shared atomic once primed.

### Gate defaults off then flips via refresh

Gate starts `false` on a fresh process; calling `set_metrics_enabled(true)` then `refresh_thread_gate` flips the calling thread's cached mirror to match.

### Timer skips clock when gate off, captures when on

Off gate: `timer()` must not read the clock or capture a metric name, no `Instant`, no capture. On gate: `timer()` must capture the start instant and metric name for its drop-time record.

## Sampling

`should_sample` is a wraparound periodic sampler for hot-path call sites that must not fire on every message but must never miss a fresh thread's first call.

### Fires on first call and every period after

`should_sample(mask)` samples true on a thread's very first call regardless of the sampler's internal counter start value, then again exactly once per `mask + 1` consecutive calls, never twice within one period.

## Per-thread message counting

`count_msg`/`take_count` track a per-thread `Cell<u64>`, never a shared atomic, so the hot path never pays a cross-core cache line bounce.

### take_count reads then resets

Reading via `take_count` returns the accumulated count and resets the thread-local counter to zero in the same call; a second read immediately after returns zero.

## Cross-thread histogram and count aggregation

`merge_local` hands a thread's local `hdrhistogram` and message count into shared per-thread registry slots; `drain_all`/`drain_count` fold every registered slot into one snapshot, clearing each slot in place.

### merge_local and drain_all aggregate across threads with clamp

A worker's recorded latencies, plus one sample above the 60s bound, merge via `merge_local`; `drain_all` on another thread sums them with the tail sample clamped into range.

`drain_count` returns the matching message-count sum for the same worker, proving both registries hand off together on one `merge_local` tick.

## OTel-standard field keys

The `field` module ships OTel-standard attribute key constants only; no project-invented prefix is allowed to leak into the exported key set.

### Field keys are dotted OTel names without a project prefix

Every constant in the `field` module equals its documented OTel dotted name (for example `http.request.method`), and none contain a project-prefix marker.

## Metrics recorder guard drop behavior

`MetricsGuard` is the RAII handle for an installed metrics recorder; its `Drop` impl must flip the `observability-core` gate off so a torn-down recorder never leaves the hot path believing metrics are still live.

### Drop flips the gate off

Constructing a `MetricsGuard` flips the `observability-core` gate on; dropping that guard flips it back off, proving the recorder lifecycle and the hot-path gate stay in sync.
