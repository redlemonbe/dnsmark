// CPU feature detection for the startup banner.
//
// Baseline: Xeon E5 v2 (Ivy Bridge, 2013) — SSE2 + SSE4.2, no AVX2.
// Upgrade:  Xeon E5 v3 / Threadripper (Haswell+) — AVX2.
//
// Detected once at process start, cached in OnceLock — zero CPUID overhead
// after the first call.
//
// NOTE: dnsmark does **not** use hand-rolled SIMD for the hot-path query copy.
// At 30–60 byte templates, `copy_from_slice` under `-O3` is as fast as (or faster
// than) a hand-written AVX2/SSE2 loop — measured, see docs/WHITEPAPER.md §10. This
// module is kept only for the CPU-tier startup banner.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SimdLevel {
    Scalar,  // non-x86_64 or very old CPU
    Sse2,    // x86_64 ABI baseline — Xeon E5 v2 and all x86_64
    Sse42,   // Nehalem / Westmere+
    Avx2,    // Haswell / Xeon E5 v3+ / Threadripper
    Avx512,  // Skylake-X+
}

impl SimdLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Scalar => "Scalar",
            Self::Sse2   => "SSE2",
            Self::Sse42  => "SSE4.2",
            Self::Avx2   => "AVX2",
            Self::Avx512 => "AVX-512",
        }
    }
}

/// Returns the highest SIMD tier available on this CPU.
/// Detected once, cached for all subsequent calls (lock-free after first).
#[inline]
pub fn simd_level() -> SimdLevel {
    static LEVEL: OnceLock<SimdLevel> = OnceLock::new();
    *LEVEL.get_or_init(detect)
}

fn detect() -> SimdLevel {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx512f") { return SimdLevel::Avx512; }
        if std::is_x86_feature_detected!("avx2")    { return SimdLevel::Avx2; }
        if std::is_x86_feature_detected!("sse4.2")  { return SimdLevel::Sse42; }
        return SimdLevel::Sse2; // x86_64 ABI guarantee
    }
    #[allow(unreachable_code)]
    SimdLevel::Scalar
}

/// Log detected SIMD tier at startup.
pub fn log_simd_info() {
    let level = simd_level();
    #[cfg(target_arch = "x86_64")]
    {
        eprintln!(
            "[dnsmark] CPU SIMD: {} | sse4.2={} avx2={} avx512f={}",
            level.as_str(),
            std::is_x86_feature_detected!("sse4.2"),
            std::is_x86_feature_detected!("avx2"),
            std::is_x86_feature_detected!("avx512f"),
        );
    }
    #[cfg(not(target_arch = "x86_64"))]
    eprintln!("[dnsmark] CPU SIMD: {}", level.as_str());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_ordering() {
        assert!(SimdLevel::Scalar < SimdLevel::Sse2);
        assert!(SimdLevel::Sse2   < SimdLevel::Sse42);
        assert!(SimdLevel::Sse42  < SimdLevel::Avx2);
        assert!(SimdLevel::Avx2   < SimdLevel::Avx512);
    }

    #[test]
    fn simd_level_cached() {
        assert_eq!(simd_level(), simd_level());
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn sse2_always_available() {
        assert!(simd_level() >= SimdLevel::Sse2);
    }
}
