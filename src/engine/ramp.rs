pub struct RampController {
    pub current_qps: u64,
    last_stable_qps: u64,
}

impl RampController {
    pub fn new() -> Self {
        Self { current_qps: 1_000, last_stable_qps: 0 }
    }

    /// Called every 5s with window deltas and current cumulative p99.
    /// Returns (new_target_qps, saturated, max_sustainable_qps).
    ///
    /// Saturation is declared (OR) when:
    ///   - timeout rate > 1%
    ///   - SERVFAIL rate > 5%
    ///   - effective QPS < 85% of target (sender can't keep up)
    ///   - p99 > 50 ms (latency degradation)
    pub fn advance(
        &mut self,
        sent: u64,
        timeouts: u64,
        servfail: u64,
        completed: u64,
        p99_us: u64,
    ) -> (u64, bool, u64) {
        if sent == 0 {
            return (self.current_qps, false, 0);
        }

        let timeout_rate = timeouts as f64 / sent as f64;
        let sf_rate = servfail as f64 / sent as f64;
        // Responses completed per second over the 5-second window
        let effective_qps = completed / 5;
        let throughput_ok = effective_qps >= (self.current_qps as f64 * 0.85) as u64;

        if timeout_rate > 0.01 || sf_rate > 0.05 || !throughput_ok || p99_us > 50_000 {
            let stable = self.last_stable_qps;
            return (stable.max(1), true, stable);
        }

        self.last_stable_qps = self.current_qps;
        self.current_qps = self.current_qps.saturating_mul(2);
        (self.current_qps, false, 0)
    }
}
