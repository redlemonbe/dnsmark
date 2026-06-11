const MAX_DOUBLINGS: u32 = 20;
const START_QPS: u64 = 100_000;
/// Median-latency SLO: the ramp's "sustainable" point is the highest load that
/// keeps p50 round-trip latency under this. p50 (not p95/p99) so that a small
/// fraction of forwarded cache-misses can't trip saturation.
const SLO_P50_US: u64 = 1_000;

#[derive(PartialEq)]
enum Phase { Exp, Bisect }

pub struct RampController {
    pub current_qps: u64,
    last_stable_qps: u64,
    doublings: u32,
    phase: Phase,
    lo: u64, // highest sustained target
    hi: u64, // lowest saturated target
}

impl RampController {
    pub fn new() -> Self {
        Self {
            current_qps: START_QPS,
            last_stable_qps: 0,
            doublings: 0,
            phase: Phase::Exp,
            lo: 0,
            hi: 0,
        }
    }

    fn converged(&self) -> bool {
        self.hi.saturating_sub(self.lo) <= (self.lo / 20).max(100_000)
    }

    /// Called once per step. `achieved_qps` = QPS the generator actually sent in the
    /// paced window; `p50_us` = median round-trip latency that window.
    /// Returns (new_target_qps, saturated, max_sustainable_qps).
    ///
    /// A step is "sustained" when the median latency stayed under the SLO. The
    /// criterion is LATENCY-ONLY: `offer_ok` (achieved >= 80% of target) is computed
    /// for context but not gated on. Consequence: the SLO cannot tell generator
    /// saturation from server saturation, and a generator-bound run can keep
    /// "passing" steps it never actually offered — cross-check the achieved q/s
    /// against the target before quoting a maximum (see WHITEPAPER §5b).
    /// Phase 1 doubles from 100k to bracket the max between the last sustained
    /// target and the first failed one; phase 2 bisects inside that bracket
    /// (e.g. 4M ok / 8M fails -> 6M, 7M ...) until within tolerance — no coarse
    /// fallback to the last power of two.
    pub fn advance(&mut self, achieved_qps: u64, p50_us: u64) -> (u64, bool, u64) {
        let offer_ok   = achieved_qps >= (self.current_qps as f64 * 0.80) as u64;
        let latency_ok = p50_us <= SLO_P50_US;
        let ok = latency_ok; let _ = offer_ok;

        match self.phase {
            Phase::Exp => {
                if ok {
                    self.last_stable_qps = self.current_qps;
                    self.doublings += 1;
                    if self.doublings >= MAX_DOUBLINGS {
                        return (self.current_qps, true, self.current_qps);
                    }
                    self.current_qps = self.current_qps.saturating_mul(2);
                    (self.current_qps, false, 0)
                } else {
                    self.lo = self.last_stable_qps;
                    self.hi = self.current_qps;
                    self.phase = Phase::Bisect;
                    if self.converged() {
                        return (self.lo.max(1), true, self.lo);
                    }
                    self.current_qps = self.lo + (self.hi - self.lo) / 2;
                    (self.current_qps, false, 0)
                }
            }
            Phase::Bisect => {
                if ok { self.lo = self.current_qps; } else { self.hi = self.current_qps; }
                if self.converged() {
                    return (self.lo.max(1), true, self.lo);
                }
                self.current_qps = self.lo + (self.hi - self.lo) / 2;
                (self.current_qps, false, 0)
            }
        }
    }
}
