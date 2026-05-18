const MAX_DOUBLINGS: u32 = 20;

pub struct RampController {
    pub current_qps: u64,
    last_stable_qps: u64,
    doublings: u32,
}

impl RampController {
    pub fn new() -> Self {
        Self { current_qps: 1_000, last_stable_qps: 0, doublings: 0 }
    }

    /// Called every 5s (1s burst + 4s stabilisation).
    /// Returns (new_target_qps, saturated, max_sustainable_qps).
    ///
    /// Saturation criteria (OR):
    ///   - burst_completions < target × 80% — sender/server physically cannot
    ///     reach the target at full speed; topology-independent
    ///   - 20 doublings without saturation (hard cap)
    ///
    /// Timeout / latency / SERVFAIL are intentionally excluded: the burst
    /// phase sends far more packets than the server can answer, leaving many
    /// queries in-flight that expire as timeouts during the stabilisation
    /// window, making those rates meaningless as saturation signals.
    pub fn advance(&mut self, burst_completions: u64) -> (u64, bool, u64) {
        let burst_ok = burst_completions >= (self.current_qps as f64 * 0.80) as u64;

        if !burst_ok {
            let stable = self.last_stable_qps;
            return (stable.max(1), true, stable);
        }

        self.last_stable_qps = self.current_qps;
        self.doublings += 1;
        if self.doublings >= MAX_DOUBLINGS {
            return (self.current_qps, true, self.current_qps);
        }
        self.current_qps = self.current_qps.saturating_mul(2);
        (self.current_qps, false, 0)
    }
}
