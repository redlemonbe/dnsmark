# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.3] - 2026-05-18

### Fixed
- **`--max-outstanding` no longer stalls the sender**: when the global in-flight cap is reached, the sender now skips the slot and loops back immediately instead of sleeping 500 µs. At equivalent QPS and completion rate, this eliminates the artificial latency that the sleep introduced.

## [0.4.2] - 2026-05-18

### Changed
- **`--max-outstanding` is now a global limit** across all workers, not per worker. With the default of 100 and 32 workers, total in-flight queries are capped at 100 instead of 3 200 — matching the semantics of `dnsperf -q 100` exactly.

## [0.4.1] - 2026-05-18

### Added
- **`--max-outstanding <N>`** (default 100): limits the total number of in-flight queries across all workers. Mirrors `dnsperf -q`. Set to `0` to disable. Prevents unbounded memory growth when the server is slow to respond.

## [0.4.0] - 2026-05-18

### Fixed
- **p999 no longer spikes to 3 s under load**: the previous async implementation produced 3-second tail latency spikes when the server was saturated. The UDP hot path now uses dedicated OS sender and receiver threads — p999 is in the sub-millisecond range for a responsive server.

### Changed
- UDP workers use dedicated OS threads (sender + receiver) instead of async tasks. The tokio runtime is now used only for orchestration and the TUI.

## [0.3.2] - 2026-05-18

### Fixed
- **`-c auto` guarantees at least 8 workers** on VMs and containers with fewer than 8 physical cores.
- **Rate-limited mode now delivers the requested QPS accurately**: drift-compensating absolute deadlines replace fixed-duration sleeps. `-Q 15000` now achieves ~15 000 QPS instead of ~9 700.

## [0.3.1] - 2026-05-18

### Changed
- **CPU affinity skips HyperThreading siblings**: workers are pinned to one logical CPU per physical core. On a 20-core/40-thread machine, 20 workers are used instead of 40.
- **`-c auto` defaults to physical core count** (was `num_cpus × 4`).

## [0.3.0] - 2026-05-18

### Added
- **CPU affinity per worker**: each worker is pinned to a physical core at startup, reducing cross-core cache migrations at high QPS.

## [0.2.5] - 2026-05-18

### Changed
- **Ramp saturation detection uses a burst probe**: each ramp step starts with a 1-second unlimited burst to measure actual server capacity. Results are more reliable across different network topologies (loopback, LAN, physical).

### Added
- **Parameters section in output**: server, protocol, clients, QPS cap, duration, timeout, mode, and source are printed before statistics for reproducibility.

## [0.2.4] - 2026-05-18

### Changed
- **Unlimited mode uses `sendmmsg(2)` batch sending** (64 datagrams per syscall). Significantly increases peak throughput in unlimited and ramp modes.

## [0.2.3] - 2026-05-18

### Fixed
- **Ramp mode converges correctly when the server responds fast**: removed an unstable effective-QPS criterion that caused ramp to never stop on low-latency servers. Ramp now stops after 20 doublings at most.

## [0.2.2] - 2026-05-18

### Fixed
- **Ramp throughput criterion no longer triggers during warm-up**: QPS is measured on the stable tail of each 5-second window, not the full window.

## [0.2.1] - 2026-05-18

### Fixed
- **Ramp mode stops correctly on fast servers**: saturation detection now includes p99 > 50 ms as a criterion, not just timeouts.
- **Version string in banner**: now reads from `CARGO_PKG_VERSION` instead of being hardcoded.

## [0.2.0] - 2026-05-18

### Fixed
- **Latency no longer inflated at low QPS**: responses are now processed during the inter-send pause. Previously, at 500 QPS with 16 workers, RTT measurements were inflated by the full sleep duration between sends.

### Added
- `CHANGELOG.md`
- CI workflow: `cargo clippy` + `cargo test` on push and PR.
- `deny.toml`: supply-chain policy (licenses, advisories, dependency bans).

## [0.1.0] - 2026-05-18

### Added
- Initial release.
- High-performance UDP / TCP / DoT DNS benchmark.
- HDR histogram: p50 / p95 / p99 / p999.
- Ramp mode: automatic saturation detection.
- Compare mode: two servers side-by-side with diff output.
- Live TUI dashboard.
- Random UUID subdomain generator (`--random`, `--random-type`).
- JSON and CSV output.
- jemalloc allocator.
- AF/XDP opt-in (`--features xdp`).
- Static musl binary — no system dependencies.
- dnsperf CLI compatibility (`-s`, `-p`, `-d`, `-c`, `-Q`, `-l`, `-t`, `-T`, `-q`, `-v`, `-S`).

[0.4.3]: https://github.com/redlemonbe/dnsmark/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/redlemonbe/dnsmark/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/redlemonbe/dnsmark/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/redlemonbe/dnsmark/compare/v0.3.2...v0.4.0
[0.3.2]: https://github.com/redlemonbe/dnsmark/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/redlemonbe/dnsmark/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/redlemonbe/dnsmark/compare/v0.2.5...v0.3.0
[0.2.5]: https://github.com/redlemonbe/dnsmark/compare/v0.2.4...v0.2.5
[0.2.4]: https://github.com/redlemonbe/dnsmark/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/redlemonbe/dnsmark/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/redlemonbe/dnsmark/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/redlemonbe/dnsmark/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/redlemonbe/dnsmark/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/redlemonbe/dnsmark/releases/tag/v0.1.0
