pub mod oom_guard;
pub mod snapshot;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use hdrhistogram::Histogram;
use parking_lot::Mutex;

pub use snapshot::StatsSnapshot;

pub struct StatsCollector {
    pub sent: AtomicU64,
    pub completed: AtomicU64,
    pub timeouts: AtomicU64,
    pub errors: AtomicU64,
    pub rcode_noerror: AtomicU64,
    pub rcode_nxdomain: AtomicU64,
    pub rcode_servfail: AtomicU64,
    pub rcode_refused: AtomicU64,
    pub rcode_other: AtomicU64,
    histogram:       Mutex<Histogram<u64>>,
    inflight_sum:    AtomicU64,
    inflight_count:  AtomicU64,
    inflight_max:    AtomicU64,
    /// Monotone offset (ns since collector creation) of the last TX completion.
    /// Set via inc_sent_n() using start.elapsed(). Never a UNIX timestamp.
    last_egress_ns:  AtomicU64,
    /// Offset (ns from creation) when the current measurement window started
    /// (set by reset_window() after warm-up); 0 = since creation.
    window_start_ns: AtomicU64,
    /// Monotone clock captured at creation — anchors last_egress_ns.
    start:           Instant,
}

impl StatsCollector {
    pub fn new() -> Self {
        Self {
            sent: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            timeouts: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            rcode_noerror: AtomicU64::new(0),
            rcode_nxdomain: AtomicU64::new(0),
            rcode_servfail: AtomicU64::new(0),
            rcode_refused: AtomicU64::new(0),
            rcode_other: AtomicU64::new(0),
            inflight_sum:    AtomicU64::new(0),
            inflight_count:  AtomicU64::new(0),
            inflight_max:    AtomicU64::new(0),
            last_egress_ns:  AtomicU64::new(0),
            window_start_ns: AtomicU64::new(0),
            start:           Instant::now(),
            histogram: Mutex::new(
                Histogram::new_with_bounds(1, 60_000_000, 3)
                    .expect("create HDR histogram"),
            ),
        }
    }

    pub fn inc_sent(&self) {
        self.sent.fetch_add(1, Ordering::Relaxed);
    }

    /// Count N TX completions (DMA-confirmed egress) and record the timestamp
    /// as a monotone offset from self.start — no UNIX clock, no reconstruction.
    pub fn inc_sent_n(&self, n: usize) {
        self.sent.fetch_add(n as u64, Ordering::Relaxed);
        let elapsed_ns = self.start.elapsed().as_nanos() as u64;
        self.last_egress_ns.store(elapsed_ns, Ordering::Relaxed);
    }

    /// Discard everything counted so far and open a fresh measurement window.
    /// Called after a warm-up period so steady-state numbers exclude XSK bind,
    /// ring fill and NIC ramp.
    pub fn reset_window(&self) {
        self.window_start_ns.store(self.start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        for a in [&self.sent, &self.completed, &self.timeouts, &self.errors,
                  &self.rcode_noerror, &self.rcode_nxdomain, &self.rcode_servfail,
                  &self.rcode_refused, &self.rcode_other,
                  &self.inflight_sum, &self.inflight_count, &self.inflight_max] {
            a.store(0, Ordering::Relaxed);
        }
        self.histogram.lock().clear();
    }

    pub fn inc_timeout(&self) {
        self.timeouts.fetch_add(1, Ordering::Relaxed);
    }

    /// Bulk-increment completed counter (throughput path, no RTT recorded).
    /// Bulk-record completions by parsed rcode (throughput path; no latency sample).
    pub fn record_rcodes(&self, noerror: u64, nxdomain: u64, servfail: u64, refused: u64, other: u64) {
        let total = noerror + nxdomain + servfail + refused + other;
        if total == 0 { return; }
        self.completed.fetch_add(total, Ordering::Relaxed);
        if noerror  > 0 { self.rcode_noerror.fetch_add(noerror,  Ordering::Relaxed); }
        if nxdomain > 0 { self.rcode_nxdomain.fetch_add(nxdomain, Ordering::Relaxed); }
        if servfail > 0 { self.rcode_servfail.fetch_add(servfail, Ordering::Relaxed); }
        if refused  > 0 { self.rcode_refused.fetch_add(refused,  Ordering::Relaxed); }
        if other    > 0 { self.rcode_other.fetch_add(other,      Ordering::Relaxed); }
    }

    pub fn inc_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Called each TX batch to track outstanding query depth.
    #[inline]
    pub fn record_inflight(&self, current: usize) {
        let v = current as u64;
        self.inflight_sum.fetch_add(v, Ordering::Relaxed);
        self.inflight_count.fetch_add(1, Ordering::Relaxed);
        let mut old = self.inflight_max.load(Ordering::Relaxed);
        while v > old {
            match self.inflight_max.compare_exchange_weak(old, v, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(x) => old = x,
            }
        }
    }

    pub fn record_response(&self, rcode: u8, rtt_us: u64) {
        self.completed.fetch_add(1, Ordering::Relaxed);
        match rcode {
            0 => { self.rcode_noerror.fetch_add(1, Ordering::Relaxed); }
            3 => { self.rcode_nxdomain.fetch_add(1, Ordering::Relaxed); }
            2 => { self.rcode_servfail.fetch_add(1, Ordering::Relaxed); }
            5 => { self.rcode_refused.fetch_add(1, Ordering::Relaxed); }
            _ => { self.rcode_other.fetch_add(1, Ordering::Relaxed); }
        }
        let mut h = self.histogram.lock();
        let _ = h.record(rtt_us.max(1));
    }

    /// Per-ramp-step latency window: p50/p95/p99 (microseconds) and sample count for
    /// the RTTs recorded since the last call, then clears the histogram so the next
    /// step measures its own load only. This is what makes `--ramp` emit a
    /// percentiles-vs-load curve (the methodology output).
    pub fn ramp_step_latency(&self) -> (u64, u64, u64, u64) {
        let mut h = self.histogram.lock();
        let out = if h.is_empty() {
            (0, 0, 0, 0)
        } else {
            (h.value_at_quantile(0.50), h.value_at_quantile(0.95),
             h.value_at_quantile(0.99), h.len())
        };
        h.clear();
        out
    }

    pub fn snapshot(&self, elapsed_secs: f64) -> StatsSnapshot {
        let sent      = self.sent.load(Ordering::Relaxed);
        let completed = self.completed.load(Ordering::Relaxed);
        let avg_qps   = if elapsed_secs > 0.0 { completed as f64 / elapsed_secs } else { 0.0 };

        // send_qps uses the real egress window — monotone offset from self.start.
        // last_egress_ns = self.start.elapsed().as_nanos() at the last inc_sent_n().
        // Covers the full TX window including in-flight drain after run end.
        // NO UNIX timestamp, NO now-based reconstruction, NO subtraction.
        let send_qps = {
            let last_ns = self.last_egress_ns.load(Ordering::Relaxed);
            let win0 = self.window_start_ns.load(Ordering::Relaxed);
            if last_ns > win0 {
                let egress_secs = (last_ns - win0) as f64 / 1_000_000_000.0;
                if egress_secs > 0.1 { sent as f64 / egress_secs }
                else                  { sent as f64 / elapsed_secs }
            } else {
                if elapsed_secs > 0.0 { sent as f64 / elapsed_secs } else { 0.0 }
            }
        };

        let h = self.histogram.lock();
        let (min_us, avg_us, p50, p95, p99, p999, max_us) = if !h.is_empty() {
            (
                h.min(),
                h.mean(),
                h.value_at_quantile(0.50),
                h.value_at_quantile(0.95),
                h.value_at_quantile(0.99),
                h.value_at_quantile(0.999),
                h.max(),
            )
        } else {
            (0, 0.0, 0, 0, 0, 0, 0)
        };
        let ifl_count = self.inflight_count.load(Ordering::Relaxed);
        let ifl_mean  = if ifl_count > 0 {
            self.inflight_sum.load(Ordering::Relaxed) as f64 / ifl_count as f64
        } else { 0.0 };
        let ifl_max = self.inflight_max.load(Ordering::Relaxed);

        StatsSnapshot {
            queries_sent:       sent,
            queries_completed:  completed,
            queries_lost:       sent.saturating_sub(completed),
            rcode_noerror:      self.rcode_noerror.load(Ordering::Relaxed),
            rcode_nxdomain:     self.rcode_nxdomain.load(Ordering::Relaxed),
            rcode_servfail:     self.rcode_servfail.load(Ordering::Relaxed),
            rcode_refused:      self.rcode_refused.load(Ordering::Relaxed),
            rcode_other:        self.rcode_other.load(Ordering::Relaxed),
            run_time_s:         elapsed_secs,
            send_qps,
            avg_qps,
            min_us,
            avg_us,
            p50_us:  p50,
            p95_us:  p95,
            p99_us:  p99,
            p999_us: p999,
            max_us,
            inflight_mean: ifl_mean,
            inflight_max:  ifl_max,
            wire_qps:      None,
        }
    }
}
