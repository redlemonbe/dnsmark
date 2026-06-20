# Changelog

## [2.4.0] - 2026-06-18

### Fixed
- **`--ramp` self-cap on kernel-UDP — the offered load now reaches the server's real knee (#14).**
  The 2.3.0 ramp work let the throughput worker rate-pace and sample RTT, but the worker still
  drained RX **in the TX thread**: at a high offered rate the per-iteration `recvmmsg` + in-flight
  bookkeeping capped TX at ~440k, so the dichotomy concluded "max sustainable ~440k" and never
  reached a fast server's knee (the same generator floods 5.9M kernel-UDP).

  Fix: the kernel-UDP throughput path (flood **and** ramp) is now split into a **TX thread** that
  floods/paces and records send times into a lock-free `SharedInFlight` (one `AtomicU64` slot per
  16-bit DNS id — the id is the index, so no lock and no collision), and a **dedicated RX thread**
  that drains via `poll()`+`recvmmsg`, matches ids and records RTT + completions. A single shared
  clock keeps `RTT = recv - send` exact.

  Verified on X710 / i40e vs a kernel-slow-path server: `--ramp` NIC tx peaked at **2.37M** (was
  442k) and found a real **p50<1ms knee at 1.3M**; p50 rises correctly with load and the SLO breaks
  at saturation. Flood egress unchanged at 5.82M; the closed-loop latency path
  (`--max-outstanding > 0`) is bit-for-bit unchanged.

### Notes
- The AF_XDP `--ramp` was already correct and is unchanged — verified end-to-end to a **9.36M
  sub-ms knee** against a fast XDP server. Its ring-based RX drain is cheap enough to run in the
  unified worker, so the TX/RX split is not needed there.
- The AF_XDP **flood** self-report under-count is **by design** (#5): the unified TX+RX-per-queue
  worker with single-queue (q0) RSS concentration is exactly what lets the XDP `--ramp` reach the
  real knee at sub-ms p50. In flood, read served throughput from the receiver NIC counters (the
  methodology already does); `--ramp` self-report is accurate.

## [2.3.0] - 2026-06-13

### Fixed
- **`--ramp` now works reliably in BOTH transports.** The saturation ramp (Dichotomic
  Saturation Discovery) was effectively unusable:
  - **kernel-UDP**: `--ramp` ran on the throughput worker, which has no per-query latency,
    so the p50 SLO never tripped and the ramp just flooded (no knee).
  - **AF_XDP**: the RSS RETA was steered across all bound queues (`equal queue_count`), but the
    unified RX+TX workers — busy on TX — polled each thinly-filled RX queue rarely, so matched
    replies sat ~10 ms (a measurement artefact) and the dichotomy never found the real knee.

  Fixes:
  - kernel-UDP: the throughput worker now rate-paces to the ramp target **and** samples RTT
    into the histogram (per-worker InFlight keyed by DNS id) — it drives load on the fast batched
    path while measuring latency, so the dichotomy finds the server's real knee.
  - AF_XDP: RSS is steered to a **single** RX queue (`ethtool -X equal 1`) so it is drained
    continuously; TX still spreads across all bound queues.

  Verified end-to-end on a dual-Xeon-v2 / X710 bench: kernel-UDP ramp ~3.8 M qps, AF_XDP ramp
  ~11.0 M qps (the server's real saturation knee), vs 0 / a 10 ms artefact before.

### Notes
- `--ramp` stays an opt-in flag; the default remains the closed-loop latency probe
  (`--max-outstanding 100`). Throughput (flood) and closed-loop latency are unchanged.

## [2.2.3] - 2026-06-13

### Fixed
- **No-xdp latency mode — the default invocation — was ~512× too slow.** The closed-loop
  outstanding gate on the kernel-UDP datapath checked a single **shared** `global_in_flight`
  atomic, so `--max-outstanding 100` (the default) was split across all *N* workers: only
  ~`100/N` queries in flight per worker (~5 on a 20-worker host). `dnsmark -s <server> -d
  <queries>` with no flags therefore read **1 845 qps** against a server serving ~940 k in the
  same mode — a starved generator misreported as a slow server. The gate is now **per-worker**
  (a local counter, matching dnsperf's per-client `-q` and the AF_XDP path); the shared atomic
  is kept only as a reported statistic, never as the hot-path gate. Measured: **1 845 → 944 k
  qps**. Flood mode (`--max-outstanding 0`) and `--xdp` are unaffected.

### Changed
- WHITEPAPER §5 / §3b updated to match reality: the outstanding gate is per-worker on **both**
  datapaths; documented the kernel-UDP throughput ceiling (~5 M qps, CPU-bound on the physical
  cores — XPS/`mq` and NUMA are already optimal, and HyperThreading *lowers* the rate) and that
  AF_XDP (~13 M, near 10 GbE line rate) is the only path past the per-skb cost.
- Removed dead `InFlight::sweep` (superseded by `sweep_with_ages`).

## [2.2.2] - 2026-06-13

### Fixed
- **--xdp emitted only ~nworkers (~12) distinct flows, capping the server under test.**
  v2.2.1 routed each worker's responses correctly but still stamped one fixed UDP source port
  per worker, so query traffic carried only ~12 distinct 5-tuples - the server's RSS collapsed
  onto ~6 receive queues and the measured throughput was a generator artefact, not the server's
  ceiling. Each worker now cycles its source port over a wide range (SPORT_SPREAD=2048), fanning
  queries across the receiver's full RSS. Response matching stays per-worker (lock-free, no
  cross-core contention); in --xdp flood mode the per-query latency stats become approximate
  (responses scatter across the bound RX queues) while served throughput stays exact. Measured:
  a kernel-UDP server that read 2.4 Mqps under the 12-flow generator reads 6.5 Mqps with the
  spread; an AF_XDP server reaches the generator's ~11 Mqps single-fibre ceiling.

## [2.2.1] - 2026-06-10

### Fixed
- **`--xdp` reported ~100% loss against a healthy AF_XDP server (#8).** Query frames use a fixed UDP source port (12345), so every response shares one 5-tuple and hashes to a single RSS queue. The generator binds its AF_XDP RX on a capped subset of queues (q0..N-1), but the NIC's default RSS indirection spans all HW queues, so the single response queue was frequently outside the bound set: responses landed on an unbound queue and were dropped before the XSK (false 100% loss), which also stalled the closed-loop sender. The generator now steers the RSS indirection table to span exactly the bound queues (`ethtool -X <if> equal <queue_count>` - RETA only, no channel reconfig, safe around an active zero-copy bind). Verified: round-trip completion 99.7-99.9% with the NIC's default multi-queue RSS. Above ~9 M qps the single active RX queue (one core) saturates and depresses the reported completion rate - a generator-side limit; read served throughput from the receiver NIC counters.

## [2.2.0] - 2026-06-10

### Added
- **802.1Q VLAN support for AF_XDP (`DNSMARK_VLAN=<vid>`)** *(experimental — see
  Validation)*. With `--xdp`, one 802.1Q tag is baked into the frame template (no
  per-frame shift; the hot path stays copy+patch), an optional tag is skipped on
  RX, and the AF_XDP socket **binds the physical parent** of a VLAN sub-interface
  while reading src IP/MAC from the sub-interface. Rationale: AF_XDP zero-copy is
  unsupported on a VLAN sub-interface (`bind … : errno 95`), so generation must
  use the physical NIC and inject the tag itself. Needed for providers that
  deliver the private network tagged (e.g. Latitude). Companion to Runbound #188.
- Wire-truth PHY tx counter resolves to the physical parent when a VLAN is used.

### Validation
- Frame layout **unit-tested against the 802.1Q wire spec** (TPID/VID, inner
  EtherType, L3 shifted +4, IPv4 checksum — the offset guard). Physical-parent
  bind confirmed (XSK `ifindex` = physical NIC). The
  resolver-side round trip is proven end-to-end (`dig` over a tagged VLAN →
  `NOERROR`). **The dnsmark tagged `--xdp` data path is not yet validated at line
  rate**: the only available 100G NIC (Broadcom BCM57508, `bnxt_en`) has **no
  AF_XDP zero-copy in any Linux kernel** (verified against mainline `bnxt_xdp.c` —
  no `xsk_pool`/`XDP_ZEROCOPY`), so generation fell back to copy mode and could not
  be rate-tested. Full validation pends a zero-copy-capable NIC (Intel
  `ice`/`i40e`/`ixgbe`, Mellanox `mlx5`). I cannot confirm tagged generation at rate.

## [2.1.0] - 2026-06-07

### Added
- **Reliable throughput counter**: report SUBMITTED TX descriptors (what actually
  reaches the wire), not completion-ring entries which under-report at multi-Mpps.
- **Auto warm-up** (`DNSMARK_WARMUP`, default 3 s): reset the measurement window
  after XSK bind / ring fill / NIC ramp so reported rates are steady-state.
- **GovernorGuard**: pin every CPU to `performance` for the run, restore on exit
  (DVFS is the #1 benchmark confounder).
- **Huge-page (2 MiB) UMEM** with 4 KiB fallback — fewer dTLB misses at multi-Mpps.

### Changed / Fixed
- **AF_XDP generator no longer self-throttles** (the non-reproducible "peak then
  collapse" behaviour): cap unified workers to NIC-local PHYSICAL cores (never HT)
  instead of one-per-HW-queue, which oversubscribed the few NIC-local cores and
  overdrove the ixgbe ZC datapath. Core budget by topology: 2-socket Intel = 10
  local + 6 cross-NUMA (QPI-bound); many-node AMD = 12 per port (Infinity Fabric).
- Global cross-NIC core cursor so dual-fibre spreads across distinct cores.
- Incremental IPv4 header checksum (RFC 1071) on the hot path.

### Notes
- Frame size dominates pps: realistic short query names (corpus) reach far higher
  rates than 32-hex `--random` names (~104 B). Bench with a realistic dataset.


All notable changes to this project will be documented in this file.  
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) — [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [2.0.6] — 2026-06-05

### Fixed

- **`physical_cores()` CPU enumeration.** It now walks the real
  `/sys/devices/system/cpu/cpuN` entries — correct for any SMT width (2/4/8) and for
  sparse/high CPU ids (e.g. a cgroup cpuset) — and dedups by **(package, core)** rather
  than `core_id` alone, so on a multi-socket host the second socket's cores are no longer
  collapsed (a dual-socket box previously reported ~half its physical cores). Still
  returns exactly **one logical CPU per physical core — HT siblings are excluded** (we pin
  to real cores, never HyperThreads). Falls back to `num_cpus::get_physical()` if `/sys`
  is absent.

---

## [2.0.5] — 2026-06-05

### Added

- **Host environment capture (#6).** `--json` output now includes a `host` object — CPU
  model, physical cores / logical threads, NUMA nodes, total memory, and the egress NIC
  toward the target (interface, driver, link speed, NUMA node) — and a one-line host
  banner is printed at startup. A `notes` field flags a result that may be bounded by the
  **receiver's** NIC/bus rather than the server software (high loss → read the receiver's
  NIC counters; see docs/benchmarking.md §3). Generator-side capture; receiver-side is out
  of scope (no remote hook).

---

## [2.0.4] — 2026-06-05

Hardening and measurement-correctness follow-ups to 2.0.0, each backed by a bench or a
microbench and cross-checked against a `tcpdump` wire capture (see docs/WHITEPAPER.md).

### Fixed

- **A timeout is a loss, not a completion** (2.0.3). Timed-out / evicted / end-of-run
  in-flight queries count toward `queries_lost`, never `queries_completed` or the latency
  histogram. So `queries_completed` = real responses, `queries_lost` = timeouts + send
  errors, `sent == completed + lost`, and the latency tail is pure response latencies.
- **Explicit in-flight eviction accounting** (2.0.1). In flood mode, a hash collision in
  a per-worker in-flight table is detected and counted as a loss, so the accounting
  identity holds exactly even when the table overflows.

### Changed

- **Hot-path query copy uses `copy_from_slice`** (2.0.2). The hand-rolled AVX2/SSE2
  memcpy was measured no faster than the standard copy at 30–60 B and was removed —
  simpler, `unsafe`-free. No SIMD speedup is claimed (see WHITEPAPER §10).
- **Multi-NIC aggregate percentiles report the worst NIC's value**, not a weighted
  average — averaging percentiles is invalid and could hide a slow NIC; a max surfaces
  it. `--nic-stats` gives per-NIC percentiles. Mean / min / max / throughput are exact.

### Added

- IPv6 targets log a one-time warning that NUMA-local pinning is skipped (the route
  lookup is IPv4-only) instead of degrading silently.
- **Technical whitepaper** (docs/WHITEPAPER.md) and a **wire-validated latency methodology**
  (docs/benchmarking.md §7): a generator's reported RTT = server + network + the tool's own
  client-side overhead, so absolute latency is anchored on a `tcpdump` capture, not on
  another tool. Default transport is UDP (comparable to dnsperf); `--xdp` is opt-in and
  symmetric (XDP-vs-XDP only).

---

## [2.0.0] — 2026-06-05

### Changed

- **UDP kernel socket is now the default transport; AF_XDP is opt-in via `--xdp`.** dnsmark no longer auto-enables XDP. The generator's datapath must match the server's: use the default UDP path against kernel servers (unbound, BIND, Runbound kernel path) for latency comparable to dnsperf, and `--xdp` only against AF_XDP servers (symmetric XDP-vs-XDP) or for saturation throughput. Mixing transports across a comparison is rejected as non-publishable.
- **Unified UDP worker** — send and receive now run in the *same* OS thread per worker (loop modelled on dnsperf: send → `poll` until next-send-or-response → `recvmmsg` drain → timeout sweep). Removes the previous sender/receiver thread split that added ~34 µs of context-switch latency to every measured RTT.

### Fixed

- **Honest latency tail** — queries expired by the timeout sweep, and queries still in flight at end-of-run, are now recorded in the latency histogram (at their real age) instead of being silently dropped. p99/p999 no longer hide the slowest responses.
- **Per-worker rate calibration** — target QPS is divided by the number of *actually spawned* workers (one per NIC RX queue), not by `--clients`. Previously a low-queue NIC under-drove the target (e.g. 50k → 6.25k on a single-queue interface).
- Removed `SCHED_FIFO` real-time scheduling on worker threads — on bare metal it starved per-core kernel softirqs and could take down host networking. Workers run `SCHED_OTHER`, pinned to NIC-local physical cores.

### Validation

- Latency cross-checked against `tcpdump` wire capture on two servers (Unbound, BIND): dnsmark's generator overhead is ~constant (~45 µs) and ~25 µs lower than dnsperf's, i.e. closer to the wire truth. Both tools rank servers identically.

---

## [1.3.0] — 2026-06-03

### Added

- **Multi-NIC AF_XDP flood** — generate across N independent NICs in parallel to aggregate beyond a single 10 GbE link. AF_XDP does not support bonding, so each NIC is an **independent XSK interface** with its own rings, workers, and target. Specify multiple targets with repeated `-s` (e.g. `-s 10.10.10.2 -s 10.10.20.2`), each on a distinct subnet routed via a distinct NIC; workers are split across NICs. `--nic-stats` prints a per-NIC throughput breakdown alongside the aggregate. A single `-s` is unchanged (mono-NIC). A NIC that fails XDP attach warns and is skipped without taking down the others.
  - Measured: **19.4M qps aggregate** across two X520 10 GbE fibre links (8.6M + 10.8M) — past the single-link ~11.3M line-rate.
- **Benchmarking methodology guide** (`docs/benchmarking.md`) — how to measure a receiver's true throughput at its NIC counters (not the generator's round-trip, which under-counts when the receiver out-paces the generator's RX), NIC/host tuning (flow control, RSS, governor), the 10 GbE line-rate ceiling, and gotchas (silent TX fallback < 1.2.1, corrupted-NIC reset, the poll-model myth).

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
