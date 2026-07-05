const MAX_DOUBLINGS: u32 = 20;
/// Hard cap on bisection steps. With a clean oracle the 5 % tolerance converges in ~5-6 halvings;
/// the cap only bounds the pathological case where a noisy served-throughput signal (kernel-UDP
/// generator jitter) keeps flipping the bracket bound so it never tightens. On hit we stop and
/// report the tightest bracket seen — the run always terminates.
const MAX_BISECT: u32 = 14;
const START_QPS: u64 = 100_000;
/// The p50 SLO is **auto-calculated from the measured latency floor**, never hardcoded —
/// a fixed 1 ms is wrong the moment the baseline RTT exceeds it (a kernel/VM resolver, or a
/// real path with switches + a router: the kernel-UDP generator floor alone is ~2-3 ms, so a
/// 1 ms SLO yields a useless knee of 0). The ramp records the lowest p50 it sees (the floor)
/// and a step is "sustained" while p50 ≤ `max(SLO_FACTOR × floor, floor + SLO_MARGIN_US)` —
/// relative, so it reduces to ~1 ms on a sub-ms AF_XDP floor (the proven value) and scales up
/// on its own for a slower floor. The criterion stays p50-based and SENT/offered-driven
/// (completed is unreliable under XDP zero-copy when the RX can't be fully drained).
const SLO_FACTOR: f64 = 3.0;
const SLO_MARGIN_US: u64 = 1_000;

#[derive(PartialEq)]
enum Phase { Exp, Bisect }

pub struct RampController {
    pub current_qps: u64,
    last_stable_qps: u64,
    doublings: u32,
    phase: Phase,
    lo: u64, // highest sustained target
    hi: u64, // lowest saturated target
    bisect_steps: u32, // bisection iterations taken (termination guard)
    use_xdp: bool, // XDP firehose ⇒ latency-only gate; kernel closed loop ⇒ also gate on offer
    pub baseline_us: u64, // measured latency floor (min p50); 0 = unset
}

impl RampController {
    pub fn new(use_xdp: bool) -> Self {
        Self {
            current_qps: START_QPS,
            last_stable_qps: 0,
            doublings: 0,
            phase: Phase::Exp,
            lo: 0,
            hi: 0,
            bisect_steps: 0,
            use_xdp,
            baseline_us: 0,
        }
    }

    /// Auto p50 SLO threshold (µs), relative to the measured latency floor. Valid after the
    /// first step set `baseline_us`.
    pub fn threshold_us(&self) -> u64 {
        let b = self.baseline_us.max(1);
        ((b as f64 * SLO_FACTOR) as u64).max(b + SLO_MARGIN_US)
    }

    /// Final saturation bracket (lo = highest sustained target, hi = lowest saturated target).
    /// After convergence the true knee lies between these — report them for transparency.
    pub fn bracket(&self) -> (u64, u64) {
        (self.lo, self.hi)
    }

    fn converged(&self) -> bool {
        // Stop bisecting once the bracket is within 5 % of the sustained bound. The small
        // absolute floor (5 k) only prevents endless halving at very low knees; it must NOT
        // dominate, or the bisection quits with a huge bracket. (The old 100 k floor did exactly
        // that — at a ~300 k knee it declared "converged" with a 100 k-wide bracket, ~33 % error,
        // so the bisection effectively did nothing.)
        self.hi.saturating_sub(self.lo) <= (self.lo / 20).max(5_000)
    }

    /// Called once per step. `achieved_qps` = QPS the generator actually sent in the
    /// paced window; `p50_us` = median round-trip latency that window.
    /// Returns (new_target_qps, saturated, max_sustainable_qps).
    ///
    /// A step is "sustained" when the median latency stayed under the auto SLO — and, in kernel-UDP
    /// mode, ALSO when the datapath delivered ≥80 % of the target (`offer_ok`). Gating on the
    /// offered/served throughput is what lets the DSD find the ceiling on the kernel closed loop,
    /// where p50 stays at the floor even past saturation; XDP stays latency-only (its firehose
    /// offered rate lags the target by design, so offer-gating would false-saturate). See the
    /// `ok` comment below and WHITEPAPER §5b.
    /// Phase 1 doubles from 100k to bracket the max between the last sustained
    /// target and the first failed one; phase 2 bisects inside that bracket
    /// (e.g. 4M ok / 8M fails -> 6M, 7M ...) until within tolerance — no coarse
    /// fallback to the last power of two. Phase 2 is also hard-capped at
    /// `MAX_BISECT` steps so a noisy oracle can never hunt forever; it then
    /// returns the tightest bracket found.
    pub fn advance(&mut self, achieved_qps: u64, p50_us: u64) -> (u64, bool, u64) {
        // Track the latency floor → the auto SLO is relative to it.
        if p50_us > 0 && (self.baseline_us == 0 || p50_us < self.baseline_us) {
            self.baseline_us = p50_us;
        }
        let offer_ok   = achieved_qps >= (self.current_qps as f64 * 0.80) as u64;
        let latency_ok = p50_us <= self.threshold_us();
        // Kernel-UDP is a GATED CLOSED LOOP: the offered rate tracks the target until the
        // generator's kernel-recv (or the server) can't keep up, and the shallow in-flight keeps
        // p50 pinned at the floor even past that point — so latency alone never trips and the Exp
        // phase used to double all the way to MAX_DOUBLINGS (~100 s of wasted steps, a phantom
        // capacity it never served). Gating on delivered throughput (`offer_ok`, served ≥ 80 % of
        // target) brackets saturation at the real ceiling and converges.
        // XDP is an OPEN-LOOP FIREHOSE: the offered rate legitimately lags the (huge) target while
        // the XSK ramps, so offer-gating there would false-saturate on the very first low step and
        // spiral the bisection down to noise. Keep XDP latency-only — its SLO genuinely trips at
        // the wire ceiling (that is the historical, correct XDP behaviour).
        let ok = if self.use_xdp { latency_ok } else { latency_ok && offer_ok };

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
                self.bisect_steps += 1;
                if self.converged() || self.bisect_steps >= MAX_BISECT {
                    return (self.lo.max(1), true, self.lo);
                }
                self.current_qps = self.lo + (self.hi - self.lo) / 2;
                (self.current_qps, false, 0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the controller with an oracle `target -> (achieved_qps, p50_us)` until it reports
    /// saturation. Returns (steps, lo, hi). Panics past `guard` steps — a runaway (never
    /// saturating) is exactly the bug this guards against, so it must fail the test, not hang.
    fn run_to_saturation(
        ctrl: &mut RampController,
        guard: u32,
        mut oracle: impl FnMut(u64) -> (u64, u64),
    ) -> (u32, u64, u64) {
        let mut steps = 0u32;
        loop {
            let target = ctrl.current_qps;
            let (achieved, p50) = oracle(target);
            let (_next, saturated, _max) = ctrl.advance(achieved, p50);
            steps += 1;
            if saturated {
                let (lo, hi) = ctrl.bracket();
                return (steps, lo, hi);
            }
            assert!(steps < guard, "DSD never saturated within {guard} steps (runaway)");
        }
    }

    /// The kernel-UDP / generator-bound case: latency stays pinned at the floor forever, but the
    /// datapath can only serve ~500k q/s regardless of the target. Latency-only (the old criterion)
    /// never trips here and doubled to MAX_DOUBLINGS (~100 s of steps, a phantom multi-M capacity).
    /// The offer gate must bracket saturation near the real served ceiling in a handful of steps.
    #[test]
    fn generator_bound_saturates_near_the_served_ceiling() {
        const CEIL: u64 = 500_000;
        let mut ctrl = RampController::new(false); // kernel mode: offer-gated
        let (steps, lo, hi) = run_to_saturation(&mut ctrl, 30, |target| (target.min(CEIL), 40));
        assert!(steps < 15, "should terminate fast, took {steps} steps");
        // Not a runaway: the knee sits around the ceiling (the 80 % offer gate tolerates up to
        // ~1.25× before failing), never a multi-ceiling phantom target.
        assert!((CEIL..CEIL * 2).contains(&lo), "knee lo={lo} off vs ceiling {CEIL}");
        assert!(hi < CEIL * 2, "hi={hi} runaway above ceiling {CEIL}");
    }

    /// Clean latency cliff: the server offers any target but p50 explodes past a knee. The
    /// bisection must converge to a tight bracket (within the 5 % tolerance) around that knee.
    #[test]
    fn clean_latency_knee_converges_within_tolerance() {
        const KNEE: u64 = 3_000_000;
        let mut ctrl = RampController::new(false);
        let (_steps, lo, hi) = run_to_saturation(&mut ctrl, 40, |target| {
            (target, if target <= KNEE { 50 } else { 5_000 })
        });
        assert!((KNEE * 9 / 10..=KNEE).contains(&lo), "knee lo={lo} not near {KNEE}");
        assert!(hi > lo, "empty bracket lo={lo} hi={hi}");
        assert!(hi - lo <= lo / 20 + 5_000, "bracket not within tolerance: [{lo};{hi}]");
    }

    /// A noisy served-throughput oracle (generator jitter flips the offer bound every step) must
    /// still terminate — the MAX_BISECT cap guarantees it instead of hunting forever.
    #[test]
    fn noisy_oracle_still_terminates() {
        let mut ctrl = RampController::new(false);
        let mut n = 0u32;
        let (steps, _lo, _hi) = run_to_saturation(&mut ctrl, MAX_DOUBLINGS + MAX_BISECT + 4, |target| {
            n += 1;
            // Alternate between fully served and half-served → offer_ok flips each call.
            (if n % 2 == 0 { target } else { target / 2 }, 40)
        });
        assert!(steps <= MAX_DOUBLINGS + MAX_BISECT + 2, "did not terminate promptly: {steps}");
    }

    /// XDP mode is latency-only: an open-loop firehose under-offers the target while it ramps, and
    /// the offer gate MUST NOT fire (that was the regression — it spiralled the XDP bisection down
    /// to noise). With latency always under SLO here, the Exp phase climbs to MAX_DOUBLINGS rather
    /// than false-saturating on the low offered rate; the run still terminates via that cap.
    #[test]
    fn xdp_mode_is_latency_only_no_offer_saturation() {
        let mut ctrl = RampController::new(true); // XDP mode
        // Offered is a tiny fraction of the (doubling) target, but latency stays at the floor —
        // exactly the XDP-ramp shape. Kernel gating would saturate immediately; XDP must not.
        let (steps, _lo, _hi) = run_to_saturation(&mut ctrl, MAX_DOUBLINGS + 2, |target| {
            (target / 10, 40)
        });
        // It climbed the full exponential run (only the MAX_DOUBLINGS cap stops it) instead of
        // false-saturating on the low offered rate at the first step, as kernel gating would.
        assert_eq!(steps, MAX_DOUBLINGS, "XDP ramp did not climb (offer gate leaked into XDP)");
    }
}
