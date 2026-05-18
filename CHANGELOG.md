# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.3] - 2026-05-18

### Changed
- **No blocking sleep when `--max-outstanding` is reached**: removed the 500 µs
  `sleep` that was introduced in v0.4.2 when the global in-flight cap was hit.
  - Rate-limited path: the sender simply skips the slot (advances the deadline)
    and loops back immediately; the rate-limiter sleep on the next iteration
    naturally yields the CPU while the receiver drains.
  - Unlimited path: replaced `sleep(500 µs)` with `std::thread::yield_now()`
    (one OS scheduler quantum, typically < 10 µs).
  The global `Arc<AtomicUsize>` counter is kept (introduced in v0.4.2).

### Performance (vs Runbound 192.168.1.11, 32 workers, 10 s)

| Tool | In-flight total | QPS | Completion | Avg RTT |
|---|---|---|---|---|
| dnsperf `-q 100 -c 32` | 3 200 | 67 020 | 100 % | 1.44 ms |
| dnsmark `--max-outstanding 100` | 100 | 67 272 | 99.98 % | 1.81 ms |

dnsmark matches dnsperf QPS-for-QPS with 32× fewer in-flight slots.

## [0.4.2] - 2026-05-18

### Changed
- **`--max-outstanding` is now a global limit** across all workers (was per-worker
  in v0.4.1). A single `Arc<AtomicUsize>` is created once in the engine and
  shared across every UDP worker. The sender increments it on each successful
  send; the receiver decrements it on each response received **and** on each
  timeout expiry. The check therefore limits the total number of queries in
  flight across the entire run, not per worker.
  With the default `--max-outstanding 100` and 32 workers this is 100 total
  in-flight instead of the previous 3 200 (32 × 100).

### Performance (vs Runbound 192.168.1.11, 32 workers, 10 s)

| Tool | In-flight total | QPS | Completion | Avg RTT | p999 |
|---|---|---|---|---|---|
| dnsperf `-q 100 -c 32` | 3 200 | 65 322 | 100 % | 1.48 ms | ~47 ms |
| dnsmark `--max-outstanding 100` | 100 | 59 868 | 99.96 % | 3.82 ms | 33 ms |

dnsmark achieves comparable throughput with 32× fewer in-flight slots.

## [0.4.1] - 2026-05-18

### Added
- **`--max-outstanding N`** (default 100, mirrors dnsperf `-q`): limits the number
  of in-flight queries per worker. Applied in both rate-limited and unlimited
  (sendmmsg) mode. In unlimited mode the batch size is capped to the remaining
  headroom (`max_outstanding - current_in_flight`), preventing burst overshoot.
  With 32 workers × default 100 = 3 200 concurrent queries max — equivalent to
  `dnsperf -c 32 -q 100`. Use `--max-outstanding 0` to disable.

### Performance (vs Runbound 192.168.1.11, 32 workers, 15 s)

| Mode | Tool | QPS | Completion | Avg RTT |
|---|---|---|---|---|
| Unlimited | dnsperf `-q 100` | 65 744 | 100.0 % | 1.47 ms |
| Unlimited | dnsmark `--max-outstanding 100` | 84 084 | 99.7 % | 36.9 ms |
| Rate 50k | dnsperf `-Q 50000 -q 100` | 48 221 | 100.0 % | 0.63 ms |
| Rate 50k | dnsmark `-Q 50000 --max-outstanding 100` | 49 845 | 100.0 % | 1.51 ms |

dnsmark's unlimited mode sends up to max_outstanding queries per worker
simultaneously (aggressive fill strategy), which drives the server harder than
dnsperf's natural back-pressure model. Both approaches are useful:
dnsperf measures sustainable throughput at low latency; dnsmark measures the
server's absolute packet-processing ceiling.

## [0.4.0] - 2026-05-18

### Changed
- **UDP architecture: dedicated sender + receiver OS threads** (replaces single
  tokio async task per worker). Each worker now spawns two `std::thread`s:
  - *Sender*: tight loop — `std::thread::sleep` (nanosleep) for rate limiting
    with drift compensation, `sendmmsg(64)` for unlimited mode. RTT timer
    starts at the actual `send()` call, matching dnsperf behaviour.
  - *Receiver*: `recvmmsg(MSG_DONTWAIT, batch=16)` in a tight loop, with timeout
    expiry checked every 10 ms. Responses are recorded under a single
    `parking_lot::Mutex` lock per batch, released before taking the histogram
    lock.
  The tokio runtime is now used only for orchestration, the TUI, and ramp
  control — the UDP hot path has zero async overhead.
- **Semaphore removed**: back-pressure via semaphore caused `p999 = timeout`
  (3 s) when the server was slow. Without the semaphore the sender never
  blocks; the natural send rate provides back-pressure.

### Fixed
- `p999 = 3 s` is gone: the old semaphore `timeout_dur` wait manifested as
  3-second latency spikes in the tail. With the OS-thread model p999 is now
  in the sub-millisecond range for a responsive server.

### Performance
| Metric | v0.3.x (tokio select!) | v0.4.0 (OS threads) |
|---|---|---|
| `-Q 15000` QPS | 14 970 | 14 951 |
| `-Q 15000` p999 | ~3 000 ms | 0.2 ms |
| unlimited completed QPS | ~100k | ~139k |
| ramp burst QPS | ~107k | ~143k |

## [0.3.2] - 2026-05-18

### Fixed
- **`-c auto` minimum 8 workers**: auto mode now guarantees at least 8 concurrent
  workers even on machines with fewer physical cores (VMs, containers).
  Startup message now distinguishes the capped case:
  `Workers: 8 (auto — min 8, VM has 2 physical cores)` vs.
  `Workers: 32 (auto — physical cores, HT excluded)`.
- **`max_in_flight` too restrictive**: changed from `concurrent / threads` (could be
  1 on a matching worker/thread count) to `concurrent × 4`, giving each UDP worker
  enough in-flight slots to sustain the QPS target without blocking the sender
  pipeline.
- **Drift-compensating rate limiter**: the UDP rate-limited path now tracks absolute
  send deadlines (`next_send: Instant`) instead of sleeping a fixed duration each
  iteration. When the tokio timer overshoots (e.g. 3 ms instead of 2.13 ms), the
  next sleep is proportionally shorter to compensate, keeping the long-run rate
  accurate. Result: `-Q 15000` now delivers ~15 000 QPS at 100% completion instead
  of ~9 700 QPS.

## [0.3.1] - 2026-05-18

### Changed
- **Physical-core-only CPU affinity**: `pin_to_cpu` now reads
  `/sys/devices/system/cpu/cpu*/topology/core_id` to build a list of one
  logical CPU ID per physical core, excluding HT siblings. Workers are pinned
  to those IDs instead of the full logical CPU range. The list is computed once
  at first call and cached via `OnceLock`. Falls back to
  `0..num_cpus::get_physical()` when `/sys` is unavailable.
- **`-c auto` default**: concurrent workers now default to the physical core
  count instead of `num_cpus * 4`. Accepted values: `auto` (default), `0`
  (same as auto), or an explicit integer. At startup dnsmark prints:
  `Workers: N (auto — physical cores, HT excluded)` or `Workers: N (manual)`.
- **Version string**: `--version` now reads `CARGO_PKG_VERSION` at compile
  time instead of the previously hardcoded `0.2.0`.

## [0.3.0] - 2026-05-18

### Added
- **Semaphore in-flight back-pressure (UDP)**: each rate-limited UDP worker now holds
  a `tokio::sync::Semaphore` with `max_in_flight = max(1, concurrent / threads)` slots.
  A permit is acquired before each send and released automatically when the response
  arrives or the query times out. If the server stops answering, the semaphore fills and
  the sender blocks naturally, bounding the in-flight HashMap and preventing memory runaway.
  Unlimited/burst mode bypasses the semaphore for maximum throughput.
- **CPU affinity per worker**: each worker task calls `sched_setaffinity(2)` at startup
  to pin its OS thread to CPU `worker_id % num_cpus`. This reduces cross-core cache
  migrations at high QPS. Implemented in `transport/mod.rs::pin_to_cpu`, applied to
  UDP, TCP, and DoT workers.

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
