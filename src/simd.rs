// CPU feature detection and SIMD dispatch for the dnsmark hot path.
//
// Baseline: Xeon E5 v2 (Ivy Bridge, 2013) — SSE2 + SSE4.2, no AVX2.
// Upgrade:  Xeon E5 v3 / Threadripper (Haswell+, 2013+) — AVX2.
//
// Detected once at process start, cached in OnceLock — zero CPUID overhead
// after the first call. No SIGILL risk: paths are guarded by target_feature.

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

/// Copy `src` into `dst` using the best available SIMD path.
/// `dst` must have at least `src.len()` bytes available.
/// Runtime dispatch: AVX2 (32 bytes/iter) → SSE2 (16 bytes/iter) → scalar.
#[inline]
pub fn memcpy_dispatch(dst: &mut [u8], src: &[u8]) {
    debug_assert!(dst.len() >= src.len());
    let len = src.len();
    #[cfg(target_arch = "x86_64")]
    {
        match simd_level() {
            SimdLevel::Avx2 | SimdLevel::Avx512 => {
                return unsafe { memcpy_avx2(dst.as_mut_ptr(), src.as_ptr(), len) };
            }
            _ => {
                return unsafe { memcpy_sse2(dst.as_mut_ptr(), src.as_ptr(), len) };
            }
        }
    }
    #[allow(unreachable_code)]
    dst[..len].copy_from_slice(src);
}

/// SSE2 copy: 16 bytes/iteration. Works on any x86_64 (Xeon E5 v2 baseline).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn memcpy_sse2(mut dst: *mut u8, mut src: *const u8, mut len: usize) {
    while len >= 16 {
        core::arch::asm!(
            "movdqu {v}, [{s}]",
            "movdqu [{d}], {v}",
            s = in(reg)      src,
            d = in(reg)      dst,
            v = out(xmm_reg) _,
            options(nostack),
        );
        src = src.add(16);
        dst = dst.add(16);
        len -= 16;
    }
    // scalar tail
    for i in 0..len {
        *dst.add(i) = *src.add(i);
    }
}

/// AVX2 copy: 32 bytes/iteration. Haswell / Xeon E5 v3 / Threadripper+.
/// VEX-encoded 16-byte tail avoids AVX→SSE transition penalty.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn memcpy_avx2(mut dst: *mut u8, mut src: *const u8, mut len: usize) {
    while len >= 32 {
        core::arch::asm!(
            "vmovdqu {v}, [{s}]",
            "vmovdqu [{d}], {v}",
            s = in(reg)      src,
            d = in(reg)      dst,
            v = out(ymm_reg) _,
            options(nostack),
        );
        src = src.add(32);
        dst = dst.add(32);
        len -= 32;
    }
    // 16-byte tail — VEX-encoded to avoid AVX→SSE transition penalty
    if len >= 16 {
        core::arch::asm!(
            "vmovdqu {v}, xmmword ptr [{s}]",
            "vmovdqu xmmword ptr [{d}], {v}",
            s = in(reg)      src,
            d = in(reg)      dst,
            v = out(xmm_reg) _,
            options(nostack),
        );
        src = src.add(16);
        dst = dst.add(16);
        len -= 16;
    }
    // scalar tail <16 bytes
    for i in 0..len {
        *dst.add(i) = *src.add(i);
    }
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

    #[test]
    fn memcpy_dispatch_all_lengths() {
        for len in 0..=128usize {
            let src: Vec<u8> = (0..len as u8).collect();
            let mut dst = vec![0u8; len + 4]; // +4 to detect overrun
            memcpy_dispatch(&mut dst[..len], &src);
            assert_eq!(&dst[..len], src.as_slice(), "len={len}");
            assert_eq!(&dst[len..], &[0u8; 4], "overrun at len={len}");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn sse2_explicit() {
        for len in 0..=64usize {
            let src: Vec<u8> = (0..len as u8).collect();
            let mut dst = vec![0u8; len];
            unsafe { memcpy_sse2(dst.as_mut_ptr(), src.as_ptr(), len); }
            assert_eq!(dst, src, "sse2 failed at len={len}");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn avx2_explicit() {
        if !std::is_x86_feature_detected!("avx2") { return; }
        for len in 0..=128usize {
            let src: Vec<u8> = (0..len as u8).collect();
            let mut dst = vec![0u8; len];
            unsafe { memcpy_avx2(dst.as_mut_ptr(), src.as_ptr(), len); }
            assert_eq!(dst, src, "avx2 failed at len={len}");
        }
    }
}
