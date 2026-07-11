//! Runtime metrics gate plus hot-path telemetry helpers.
//!
//! Dependency leaf: `metrics` facade + `hdrhistogram` (+ optional `tokio` flusher) only.
//! No `opentelemetry`, no `tracing-subscriber`, so a feed recv loop can pull it without
//! dragging in backend crates. Backend wiring lives in the `observability` crate.

use std::{
    cell::{Cell, RefCell},
    sync::{
        Arc, LazyLock, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

use hdrhistogram::Histogram;

// ---------------------------------------------------------------------
// Runtime gate: process-wide atomic, per-thread mirror to skip the
// cross-core hop on the hot path once primed.
// ---------------------------------------------------------------------

/// Process-wide metrics gate. Starts OFF; flip with [`set_metrics_enabled`].
pub static METRICS_ON: AtomicBool = AtomicBool::new(false);

thread_local! {
    static GATE_MIRROR: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Flip process-wide gate. Relaxed: gate is advisory, not ordering-sensitive vs other state.
pub fn set_metrics_enabled(on: bool) {
    METRICS_ON.store(on, Ordering::Relaxed);
}

/// Sync this thread's cached gate value from the process-wide flag.
/// Call once per thread at startup, and again after any runtime toggle a thread cares about.
pub fn refresh_thread_gate() {
    GATE_MIRROR.set(Some(METRICS_ON.load(Ordering::Relaxed)));
}

/// Gate check for hot path. Reads primed thread-local mirror; falls to shared atomic if unset.
#[inline]
pub fn metrics_enabled() -> bool {
    GATE_MIRROR
        .with(|mirror| mirror.get())
        .unwrap_or_else(|| METRICS_ON.load(Ordering::Relaxed))
}

// ---------------------------------------------------------------------
// Gated RAII timer: clock read only pays when gate is on.
// ---------------------------------------------------------------------

/// RAII latency timer. Records elapsed seconds into `metric`'s histogram on drop.
/// `None` variant means gate was off at start: no clock read, no record on drop.
pub struct Timer(Option<(Instant, &'static str)>);

/// Start a timer for `metric` iff [`metrics_enabled`], else skip the clock read entirely.
#[inline]
pub fn timer(metric: &'static str) -> Timer {
    if metrics_enabled() {
        Timer(Some((Instant::now(), metric)))
    } else {
        Timer(None)
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some((start, metric)) = self.0 {
            metrics::histogram!(metric).record(start.elapsed().as_secs_f64());
        }
    }
}

// ---------------------------------------------------------------------
// Thread-local bounded histogram + cross-thread drain.
// Bounds 1ns..60s, 3 sig figs. saturating_record clamps tail spikes
// instead of dropping the sample.
// ---------------------------------------------------------------------

const HIST_LOW_NS: u64 = 1;
const HIST_HIGH_NS: u64 = 60_000_000_000;
const HIST_SIGFIGS: u8 = 3;

fn new_bounded_histogram() -> Histogram<u64> {
    Histogram::new_with_bounds(HIST_LOW_NS, HIST_HIGH_NS, HIST_SIGFIGS)
        .expect("fixed bounds 1ns..60s at 3 sigfigs are always valid")
}

/// Recover a poisoned mutex instead of panicking. Telemetry must survive a panicked holder.
fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// One thread's histogram, shared so the flusher thread can drain it.
type SharedHistogram = Arc<Mutex<Histogram<u64>>>;

/// Merge targets, one slot per thread that has called [`merge_local`] at least once.
static REGISTRY: LazyLock<Mutex<Vec<SharedHistogram>>> = LazyLock::new(|| Mutex::new(Vec::new()));

thread_local! {
    static LOCAL: RefCell<Histogram<u64>> = RefCell::new(new_bounded_histogram());
    static LOCAL_SHARED: RefCell<Option<SharedHistogram>> = const { RefCell::new(None) };
}

/// Record one latency sample (ns) into this thread's local histogram. No-op if gate off.
#[inline]
pub fn record_latency_ns(ns: u64) {
    if !metrics_enabled() {
        return;
    }
    LOCAL.with_borrow_mut(|h| h.saturating_record(ns));
}

/// Merge this thread's local histogram into its registry slot, then clear local.
/// First call per thread registers a fresh shared histogram into [`REGISTRY`].
pub fn merge_local() {
    let shared = LOCAL_SHARED.with_borrow_mut(|slot| {
        slot.get_or_insert_with(|| {
            let hist = Arc::new(Mutex::new(new_bounded_histogram()));
            lock_recover(&REGISTRY).push(Arc::clone(&hist));
            hist
        })
        .clone()
    });
    LOCAL.with_borrow_mut(|local| {
        lock_recover(&shared).add(&*local).ok();
        local.reset();
    });

    // hand off message count into this thread's shared slot, same tick
    let count = COUNT.with(|c| c.replace(0));
    let shared_count = COUNT_SHARED.with_borrow_mut(|slot| {
        slot.get_or_insert_with(|| {
            let counter = Arc::new(AtomicU64::new(0));
            lock_recover(&COUNT_REGISTRY).push(Arc::clone(&counter));
            counter
        })
        .clone()
    });
    shared_count.fetch_add(count, Ordering::Relaxed);
}

/// Drain every registered thread histogram into one snapshot, clearing each in place.
/// Only sees data already handed off via [`merge_local`]; a thread that never merges
/// never contributes here.
pub fn drain_all() -> Histogram<u64> {
    let mut merged = new_bounded_histogram();
    for hist in lock_recover(&REGISTRY).iter() {
        let mut guard = lock_recover(hist);
        merged.add(&*guard).ok();
        guard.reset();
    }
    merged
}

// ---------------------------------------------------------------------
// Per-thread count + sampler. Hot path (count_msg) is a plain Cell, never
// a shared atomic. Cross-thread aggregation rides merge_local into a shared
// AtomicU64 slot per thread; drain_count sums them, mirroring the histogram
// REGISTRY hand-off so a flusher on another thread sees the real total.
// ---------------------------------------------------------------------

/// Message-count slots, one per thread that has merged at least once.
static COUNT_REGISTRY: LazyLock<Mutex<Vec<Arc<AtomicU64>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

thread_local! {
    static COUNT: Cell<u64> = const { Cell::new(0) };
    static COUNT_SHARED: RefCell<Option<Arc<AtomicU64>>> = const { RefCell::new(None) };
    static SAMPLE_CTR: Cell<u64> = const { Cell::new(0) };
}

/// Increment this thread's message counter. Wrapping: overflow wraps, never panics.
#[inline]
pub fn count_msg() {
    COUNT.with(|c| c.set(c.get().wrapping_add(1)));
}

/// Read this thread's counter, then reset it to zero. Per-thread primitive;
/// for cross-thread totals use merge_local on the worker tick plus drain_count.
#[inline]
pub fn take_count() -> u64 {
    COUNT.with(|c| c.replace(0))
}

/// Drain every thread's merged message count into one sum, zeroing each slot.
/// Only sees counts handed off via merge_local, never a thread's live local count.
pub fn drain_count() -> u64 {
    lock_recover(&COUNT_REGISTRY)
        .iter()
        .map(|slot| slot.swap(0, Ordering::Relaxed))
        .sum()
}

/// True exactly once per `mask + 1` calls. `mask` must be `2^n - 1` (e.g. [`SAMPLE_1_IN_8192`]).
/// Checks the pre-increment counter value, so the very first call on a thread always samples.
#[inline]
pub fn should_sample(mask: u64) -> bool {
    SAMPLE_CTR.with(|c| {
        let v = c.get();
        c.set(v.wrapping_add(1));
        v & mask == 0
    })
}

/// Mask for 1-in-8192 sampling: pass to [`should_sample`].
pub const SAMPLE_1_IN_8192: u64 = 8192 - 1;

// ---------------------------------------------------------------------
// Optional tokio flusher.
// ---------------------------------------------------------------------

/// Spawn a 100ms tokio loop draining histograms + counters into `metrics` gauges/counters.
///
/// Gauge quantiles do not re-aggregate across process instances: averaging p99 across
/// processes is wrong math. Fine for single-process feed handler. Worker threads must
/// call `merge_local` on their own tick, since `drain_all`/`drain_count` only see data
/// already handed off to the shared registries, never a thread's live local state.
#[cfg(feature = "flush-tokio")]
pub fn spawn_flusher(latency_prefix: &'static str, count_metric: &'static str) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            tick.tick().await;
            refresh_thread_gate();
            let h = drain_all();
            metrics::gauge!(format!("{latency_prefix}.p50")).set(h.value_at_quantile(0.50) as f64);
            metrics::gauge!(format!("{latency_prefix}.p99")).set(h.value_at_quantile(0.99) as f64);
            metrics::gauge!(format!("{latency_prefix}.p999"))
                .set(h.value_at_quantile(0.999) as f64);
            metrics::counter!(count_metric).increment(drain_count());
        }
    });
}

// ---------------------------------------------------------------------
// OTel-standard field keys. No project-invented `myapp.*` keys.
// ---------------------------------------------------------------------

/// OTel-standard attribute key constants for structured log/span/metric fields.
pub mod field {
    pub const HTTP_REQUEST_METHOD: &str = "http.request.method";
    pub const HTTP_ROUTE: &str = "http.route";
    pub const HTTP_RESPONSE_STATUS_CODE: &str = "http.response.status_code";
    pub const RPC_METHOD: &str = "rpc.method";
    pub const RPC_SERVICE: &str = "rpc.service";
    pub const ERROR_TYPE: &str = "error.type";
    pub const ENDUSER_ID: &str = "enduser.id";
    pub const SERVER_ADDRESS: &str = "server.address";
    pub const SERVER_PORT: &str = "server.port";
    pub const NETWORK_PROTOCOL_NAME: &str = "network.protocol.name";
    pub const MESSAGING_SYSTEM: &str = "messaging.system";
    pub const MESSAGING_DESTINATION_NAME: &str = "messaging.destination.name";
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: flush-tokio flusher (spawn_flusher) not covered here. It only proves
    // itself through the metrics facade global recorder, and installing one would
    // race every other test sharing this process. Skipped.

    /// Gate starts off; set + refresh flips the thread mirror.
    // @lat: [[tests#Runtime gate#Gate defaults off then flips via refresh]]
    #[test]
    fn gate_defaults_off_then_flips_via_refresh() {
        assert!(
            !metrics_enabled(),
            "gate must default off before any enable call"
        );
        set_metrics_enabled(true);
        refresh_thread_gate();
        assert!(metrics_enabled());
        // leave gate clean for any test sharing this process
        set_metrics_enabled(false);
        refresh_thread_gate();
    }

    /// Timer skips clock read when gate off, captures start + metric name when on.
    // @lat: [[tests#Runtime gate#Timer skips clock when gate off, captures when on]]
    #[test]
    fn timer_skips_clock_off_gate_captures_on_gate() {
        set_metrics_enabled(false);
        refresh_thread_gate();
        assert!(
            timer("x").0.is_none(),
            "off gate: no clock read, no capture"
        );

        set_metrics_enabled(true);
        refresh_thread_gate();
        assert!(
            timer("x").0.is_some(),
            "on gate: start instant + metric name captured"
        );

        set_metrics_enabled(false);
        refresh_thread_gate();
    }

    /// should_sample: fires on fresh thread's first call, again one period later,
    /// exactly once per period-length run of consecutive calls, whatever the start offset.
    // @lat: [[tests#Sampling#Fires on first call and every period after]]
    #[test]
    fn should_sample_fires_first_call_and_every_period_after() {
        const MASK: u64 = SAMPLE_1_IN_8192;
        const PERIOD: u64 = MASK + 1;

        std::thread::spawn(|| {
            assert!(
                should_sample(MASK),
                "first call on fresh thread must sample"
            );

            let stray = (0..PERIOD - 1).filter(|_| should_sample(MASK)).count();
            assert_eq!(stray, 0, "no sample strictly between period boundaries");

            assert!(
                should_sample(MASK),
                "sample fires again exactly one period later"
            );

            // any PERIOD consecutive calls cross exactly one boundary
            let hits = (0..PERIOD).filter(|_| should_sample(MASK)).count();
            assert_eq!(hits, 1, "exactly one sample per PERIOD consecutive calls");
        })
        .join()
        .unwrap();
    }

    /// take_count reads then resets this thread's counter; second read is zero.
    // @lat: [[tests#Per-thread message counting#take_count reads then resets]]
    #[test]
    fn take_count_reads_then_resets() {
        std::thread::spawn(|| {
            const K: u64 = 5;
            for _ in 0..K {
                count_msg();
            }
            assert_eq!(take_count(), K);
            assert_eq!(take_count(), 0, "second read after take must be zero");
        })
        .join()
        .unwrap();
    }

    /// Cross-thread merge/drain plus saturating clamp. Sole owner of REGISTRY and
    /// COUNT_REGISTRY in this test binary: no other test calls merge_local, drain_all,
    /// or drain_count, else counts here would double up against process-wide state.
    // @lat: [[tests#Cross-thread histogram and count aggregation#merge_local and drain_all aggregate across threads with clamp]]
    #[test]
    fn merge_local_and_drain_all_aggregate_across_threads_with_clamp() {
        const KNOWN: [u64; 5] = [10, 1_000, 50_000, 2_000_000, 999_999_999];
        const OVER_BOUND_NS: u64 = 70_000_000_000; // above HIST_HIGH_NS, must clamp not panic
        const MSG_COUNT: u64 = 7;

        std::thread::spawn(|| {
            set_metrics_enabled(true);
            refresh_thread_gate();
            for &ns in &KNOWN {
                record_latency_ns(ns);
            }
            record_latency_ns(OVER_BOUND_NS);
            for _ in 0..MSG_COUNT {
                count_msg();
            }
            merge_local();
        })
        .join()
        .unwrap();

        let merged = drain_all();
        assert_eq!(merged.len(), KNOWN.len() as u64 + 1);
        let p50 = merged.value_at_quantile(0.5);
        assert!((HIST_LOW_NS..=HIST_HIGH_NS).contains(&p50));
        // hdrhistogram buckets: max() reports the occupied bucket's representative value,
        // which can round up past the raw high bound. Compare against that bucket's own
        // upper edge instead, still proving the clamp landed in-range, not at raw 70e9.
        let clamp_bound = merged.highest_equivalent(HIST_HIGH_NS);
        assert!(
            merged.max() <= clamp_bound,
            "saturating_record must clamp tail spike into bound, not panic or overrun"
        );

        assert_eq!(drain_count(), MSG_COUNT);
    }

    /// field keys stay dotted OTel names, never project-invented myapp.* prefixes.
    // @lat: [[tests#OTel-standard field keys#Field keys are dotted OTel names without a project prefix]]
    #[test]
    fn field_keys_are_dotted_otel_names_without_project_prefix() {
        assert_eq!(field::HTTP_REQUEST_METHOD, "http.request.method");
        assert_eq!(field::HTTP_ROUTE, "http.route");
        assert_eq!(field::RPC_SERVICE, "rpc.service");
        assert_eq!(field::ERROR_TYPE, "error.type");

        let keys = [
            field::HTTP_REQUEST_METHOD,
            field::HTTP_ROUTE,
            field::HTTP_RESPONSE_STATUS_CODE,
            field::RPC_METHOD,
            field::RPC_SERVICE,
            field::ERROR_TYPE,
            field::ENDUSER_ID,
            field::SERVER_ADDRESS,
            field::SERVER_PORT,
            field::NETWORK_PROTOCOL_NAME,
            field::MESSAGING_SYSTEM,
            field::MESSAGING_DESTINATION_NAME,
        ];
        for key in keys {
            assert!(
                !key.contains("myapp"),
                "field key leaked project prefix: {key}"
            );
        }
    }
}
