# Changelog

All notable changes to this project will be documented in this file.  
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) — [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [1.0.0] — 2026-05-26

First stable release.

### Added
- **SIMD memcpy dispatch** — `AVX2` (32 B/iter) on Haswell+ / Threadripper, `SSE2` (16 B/iter) on Xeon E5 v2 baseline. Detected once at boot via CPUID, cached in `OnceLock`. Logged at startup: `[dnsmark] CPU SIMD: SSE4.2 | sse4.2=true avx2=false avx512f=false`.
- **Zero-allocation hot path** — pre-built wire-format query pool (`WireQueryPool`), stack-allocated `iovecs[256]` + `mmsghdr[256]` in `sendmmsg_pre_alloc()`, stack response buffer in receiver thread. No heap allocation on the send/receive hot path.
- **Static binaries** — `x86_64-linux-musl` and `aarch64-linux-musl` in GitHub releases. Drop-anywhere, no runtime dependencies.

### Changed
- `BATCH_SIZE` 64 → 256 — 4× fewer `sendmmsg(2)` syscalls at peak QPS.
- `RECV_BATCH` 16 → 64 — matches larger send batch size.
- `SO_SNDBUF` / `SO_RCVBUF` tuned to 8 MB per socket (requires `net.core.wmem_max` / `rmem_max` ≥ 8 MB on the OS).

---

## [0.4.5] — 2026-05-19

### Fixed
- XDP interface selected via `getifaddrs()` on server subnet — eliminates wrong-interface selection on Proxmox hosts where a bridge and a veth share the same `/24`.
- Virtual interface detection with automatic parent resolution — if the selected interface is a bridge / veth / ipvlan / macvlan, XDP retries on the physical parent; falls back to recvmmsg if no parent is found.

---

## [0.4.4] — 2026-05-18

### Added
- AF/XDP receive path (`--features xdp`, enabled by default) — DNS responses captured at NIC driver level via eBPF, bypassing the kernel network stack. Automatic fallback to recvmmsg on unsupported hardware.
- `--no-xdp` — disable AF/XDP at runtime.
- `--xdp` — force AF/XDP, error if unavailable.

---

## [0.4.3] — 2026-05-18

### Fixed
- `--max-outstanding` no longer stalls the sender: skips the slot instead of sleeping 500 µs when the global in-flight cap is reached.

---

## [0.4.2] — 2026-05-18

### Changed
- `--max-outstanding` is now a global limit across all workers (matches `dnsperf -q` semantics).

---

## [0.4.1] — 2026-05-18

### Added
- `--max-outstanding <N>` (default 100) — caps total in-flight queries across all workers.

---

## [0.4.0] — 2026-05-18

### Added
- Dedicated sender + receiver OS threads per UDP worker — sender and receiver are fully decoupled, eliminating cross-thread contention on the hot path.

---

## [0.3.x] — 2026-05-17

- `-c auto` minimum 8 workers, drift-compensating rate limiter.
- Global shared in-flight counter (`Arc<AtomicUsize>`).
- `--max-outstanding` initial implementation (per-worker).
- Non-blocking sender — removes 500 µs sleep on cap hit.
