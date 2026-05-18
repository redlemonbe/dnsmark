pub struct RampController {
    pub current_qps: u64,
    last_stable_qps: u64,
}

impl RampController {
    pub fn new() -> Self {
        Self { current_qps: 1_000, last_stable_qps: 0 }
    }

    /// Called every 5s with counts for the window.
    /// Returns (new_target_qps, saturated, max_sustainable_qps).
    pub fn advance(&mut self, sent: u64, timeouts: u64, servfail: u64) -> (u64, bool, u64) {
        if sent == 0 {
            return (self.current_qps, false, 0);
        }
        let timeout_rate = timeouts as f64 / sent as f64;
        let sf_rate = servfail as f64 / sent as f64;

        if timeout_rate > 0.01 || sf_rate > 0.05 {
            // Saturated: last stable level, or current if first window
            let stable = if self.last_stable_qps == 0 {
                // Saturated even at baseline — report 0 (caller will handle messaging)
                0
            } else {
                self.last_stable_qps
            };
            return (stable.max(1), true, stable);
        }

        self.last_stable_qps = self.current_qps;
        self.current_qps = self.current_qps.saturating_mul(2);
        (self.current_qps, false, 0)
    }
}
