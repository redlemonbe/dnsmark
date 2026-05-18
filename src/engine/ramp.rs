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

    /// Called every 5s with window deltas and cumulative p99.
    /// Returns (new_target_qps, saturated, max_sustainable_qps).
    ///
    /// Saturation is declared (OR) when:
    ///   - p99 > 50 ms  (primary — latency degradation, immune to warm-up variance)
    ///   - timeout rate > 1%  (secondary)
    ///   - SERVFAIL rate > 5%  (secondary)
    ///   - 20 doublings reached without saturation (hard cap)
    pub fn advance(
        &mut self,
        sent: u64,
        timeouts: u64,
        servfail: u64,
        p99_us: u64,
    ) -> (u64, bool, u64) {
        if sent == 0 {
            return (self.current_qps, false, 0);
        }

        let timeout_rate = timeouts as f64 / sent as f64;
        let sf_rate = servfail as f64 / sent as f64;

        if p99_us > 50_000 || timeout_rate > 0.01 || sf_rate > 0.05 {
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
