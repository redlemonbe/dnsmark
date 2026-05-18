# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.5] - 2026-05-18

### Changed
- **Ramp saturation criterion — burst probe**: each 5-second ramp step now starts
  with a 1-second unlimited burst (sendmmsg) to measure the real maximum achievable
  completions/s. Saturation is declared when `burst_completions < target × 80%`.
  This criterion is topology-independent (loopback, LAN, physical) and immune to
  warm-up and scheduling variance.
  Timeout/SERVFAIL rates are intentionally excluded from ramp: the burst phase
  floods in-flight queries whose timeouts expire during the stabilisation window,
  making those rates unreliable as saturation signals.
  Each step logs the burst result: `Ramp: target QPS -> N (burst: M/s)`.

### Added
- **Parameters section in output**: server, protocol, clients, QPS cap, duration,
  timeout, mode, and source are printed before Statistics for reproducibility.
  Also included in `--json` output under `"parameters"`.

## [0.2.4] - 2026-05-18

### Changed
- **sendmmsg(2) batch sending in unlimited mode**: in unlimited mode (`-Q 0` / ramp
  peak measurement), the UDP worker now sends 64 datagrams per `sendmmsg(2)` syscall
  instead of one per `send()`. This reduces both syscall overhead and tokio yield
  overhead by 64×. Measured improvement: 58k → 96k+ completed QPS on loopback.
  Rate-limited mode (`-Q N`) is unchanged and still uses single sends with the
  `select!`-based receive loop for accurate RTT.

## [0.2.3] - 2026-05-18

### Fixed
- **Ramp never converges when server responds fast (p99 always < 50ms)**:
  removed the unstable effective-QPS criterion entirely. Saturation criteria
  are now p99 > 50ms (primary) and timeout/SERVFAIL rates (secondary).
- **Hard cap added**: ramp stops after 20 doublings regardless of saturation
  criteria, reporting the last stable QPS with reason `hard cap (20 doublings)`.
- **Warm-up split removed**: the 2s+3s split was only needed for the QPS criterion
  and is gone. Each ramp step is a clean 5s window again.
- **Saturation reason display**: now correctly identifies p99 / timeout / SERVFAIL /
  hard-cap and prints the rate or value for each.

## [0.2.2] - 2026-05-18

### Fixed
- **Ramp throughput criterion triggers too early on warm-up**: QPS is now measured
  only on the stable 3-second tail of each 5-second window (first 2 s discarded as
  warm-up for tokio, sockets, jemalloc). Threshold lowered from 85% to 70% to absorb
  residual start-of-window variance.

## [0.2.1] - 2026-05-18

### Fixed
- **Ramp mode no longer converges when server responds fast (REFUSED)**: saturation
  detection was timeout-only. Added two new criteria (OR logic):
  - effective QPS < 85% of target (sender can't keep up)
  - p99 > 50 ms (latency degradation)
  Saturation reason is now printed: `Max sustainable QPS: N (reason)`.
- **Banner version hardcoded at 0.1.0**: now reads `CARGO_PKG_VERSION` at compile time.

## [0.2.0] - 2026-05-18

### Fixed
- **Latency inflation at low QPS**: UDP worker now processes responses during the
  inter-send pause via `tokio::select!`. Previously, at 500 QPS / 16 workers the
  worker slept ~32 ms between sends, leaving responses unread until the next
  iteration and inflating RTT measurements by the full sleep duration.
- **README examples**: Replaced public DNS servers (8.8.8.8, 1.1.1.1) in usage
  examples with private/placeholder addresses, consistent with the ACCEPTABLE_USE
  disclaimer.

### Added
- `CHANGELOG.md` (this file)
- `.github/workflows/ci.yml` — cargo clippy + cargo test on push / PR
- `deny.toml` — supply-chain policy via cargo-deny (licenses, advisories, bans)

## [0.1.0] - 2026-05-18

### Added
- Initial release
- High-performance UDP / TCP / DoT DNS benchmark
- HDR histogram (p50 / p95 / p99 / p999)
- Ramp mode (auto saturation detection, doubles every 5 s)
- Compare mode (two servers side-by-side, diff output)
- Live TUI dashboard (ratatui)
- Random UUID subdomain generator (`--random`, `--random-type`)
- JSON and CSV output
- jemalloc allocator
- AF/XDP opt-in (`--features xdp`)
- Static musl binary (no system dependencies)
- dnsperf CLI compatibility (`-s`, `-p`, `-d`, `-c`, `-Q`, `-l`, `-t`, `-T`, `-q`, `-v`, `-S`)

[0.2.0]: https://github.com/redlemonbe/dnsmark/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/redlemonbe/dnsmark/releases/tag/v0.1.0
