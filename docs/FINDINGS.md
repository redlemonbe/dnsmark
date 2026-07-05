# dnsmark — Engineering notes

Status as of **v2.7.5**. User-facing methodology lives in
[benchmarking.md](benchmarking.md) and [WHITEPAPER.md](WHITEPAPER.md); the change
history is in [CHANGELOG.md](../CHANGELOG.md).

## Latency measurement

- **Default transport is the UDP kernel socket**; `--xdp` is opt-in (symmetric
  XDP-vs-XDP, or saturation only). The generator's datapath must match the server's.
- A **unified per-worker loop** (send → `poll` → `recvmmsg` → sweep) measures RTT
  start-to-finish on one thread and one clock, removing the pre-2.0 sender/receiver
  split that added ~34 µs of context-switch latency to every sample.
- The **10 ms timeout sweep** and the **end-of-run drain** count expired and
  still-in-flight queries as **losses** (`queries_lost`, v2.0.3) — never as completions
  and never into the latency histogram. The histogram holds real response latencies
  only (slow responses within the timeout still count), and
  `sent == completed + lost` holds exactly.
- Latency is **validated against a wire (`tcpdump`) capture**, not against another tool;
  see benchmarking.md §7 for the three-term decomposition (server + network + generator
  overhead) and exact reproduction commands.

## AF_XDP datapath

- One worker per **bound** RX queue, capped to the NIC-local physical-core budget
  (v2.1.0 — one busy-poll worker per physical core is the stable point; one XSK per HW
  queue oversubscribed the NIC-local cores and collapsed throughput), pinned to
  NIC-local **physical** cores (no HT sibling, no real-time scheduling). A single
  **shared** in-flight table (matched by global DNS id, not partitioned per worker);
  each worker **cycles its UDP source port** over an internal
  spread (`10000 + (counter mod 2048)`, per-worker phase offset) — a fixed one-port-per-worker
  scheme was dropped because too few flows collapsed the receiver's RSS onto a few queues. The generator steers its
  own RSS indirection table to span the bound queues
  (`equal 1` in closed loop, `equal <queue_count>` in firehose — cf. WHITEPAPER §6) so
  every response lands on a bound
  worker; without this, the NIC's default RSS (spanning all HW queues) drops responses
  on unbound queues as a false ~100% loss (#8). Queue count and NUMA node are
  auto-detected.
- Requires a physical NIC, `CAP_NET_RAW`/`CAP_BPF` (or root), and flow control disabled
  on the sender to reach line rate.

## Line-rate awareness (2026-07-02, v2.7.1)

Built directly on `server_rx_qps` — the authoritative rate read from the egress NIC's
hardware rx counter (`rx_packets` + ring-overflow drops), which stays the reference.
dnsmark now divides that authoritative rate by a line-rate ceiling computed from its own
hardware observations — the average on-wire reply size (`rx_bytes / rx_packets`) and the
egress-NIC link speed — and reports "% of line rate" plus a verdict:
`wire-bound` (>= 90% of line rate → the link is the limit) or `link-headroom` (< 90% →
the server or generator is the limit, not the wire). No receiver-side reading is needed;
it works in both AF_XDP and kernel-UDP.

- **Scope (v2.7.2): the line-rate verdict is emitted only for fixed/flood runs, never in
  `--ramp`.** In `--ramp`, `server_rx_qps` spans the whole ramp-up, so its average sits far
  below the peak; a line-rate % computed from it would contradict the DSD's own "Capacity"
  summary. In `--ramp` the DSD Capacity / Within SLO / Knee bracket is the throughput answer;
  the line-rate line appears only in fixed/flood mode, where `server_rx_qps` reflects one
  steady window.

- **DNS at multi-Mpps on 10 GbE is line-rate bound.** Measured on an X710 + X520
  dual-link rig with 103 B replies — both directions pinned to the wire. Single-link
  verified 100% wire-bound (`server_rx_qps` ~9.85 M/s, ceiling ~9.85 M/s); dual-link
  ~97% wire-bound (`server_rx_qps` ~19 M/s).
- **Ceiling** is ~9.85 M replies/s per 10 G NIC for ~100 B replies; ~19 M/s across a dual
  link.
- **A single core saturates a 10 G NIC in both directions** (TX + RX-count). A 1-core
  unified worker and a 2-core TX/RX split give identical throughput (~9.85 M/s per NIC):
  the wire is the wall, not the CPU and not PCIe (PCIe x8 Gen3 ~63 Gbps, 6× the 10 G
  link).
- Auto warm-up default is now **5 s** (was 3 s) so the reported rate is steady-state.

## Kernel-UDP DSD capacity is a closed-loop knee (2026-07-02, v2.7.2/v2.7.3)

Two related honesty fixes, both from the X710 + X520 rig (generator = dual Xeon E5-2690 v2).

- **Line-rate gated to fixed/flood (v2.7.2).** The line-rate verdict was contradicting the
  `--ramp` Capacity summary: `server_rx_qps` averaged over the ramp-up is far below the peak,
  so its line-rate % undershot the DSD knee. The verdict is now printed only for fixed/flood
  runs (one steady window); in `--ramp` the DSD Capacity / Within SLO / Knee bracket is the
  throughput answer. See the line-rate section above.
- **Kernel-UDP DSD is the closed-loop SLO knee, not the raw ceiling (v2.7.3).** In kernel-UDP
  the ramp is a **gated closed loop** (dnsperf-comparable, latency-honest). The generator's own
  kernel receive path drops replies under load; those drops clog the outstanding slots and cap
  the *offered* rate well below the server's real capacity. So the kernel-UDP DSD figure is the
  closed-loop knee — **generator-recv bound**, not the server's raw max. It is now labelled
  `Capacity: … (closed-loop knee — kernel-recv bound, NOT the server's raw max)`, and the ramp
  prints a pointer to the open-loop command for the raw ceiling:
  `dnsmark -s <ip> -Q 0 --max-outstanding 0`, which reports "Server throughput (NIC rx)" =
  `server_rx_qps`, the authoritative reply rate.
- **Numbers.** kernel-UDP DSD (closed loop) knees at ~1.08 M/s (generator-recv bound). The same
  kernel-UDP server under an **open-loop flood** serves ~5.4 M/s (`server_rx_qps`, NIC-verified);
  the generator sends ~5.3 M/s but its kernel recv drops ~50%, so the userspace round-trip counts
  only ~2.5 M/s. Kernel-UDP is generator-recv bound; XDP exists to remove that (lossless RX).
- **The `--xdp` ramp is unaffected.** It is an open-loop firehose with a lossless zero-copy RX,
  so its Capacity genuinely is the max replies/s on the wire (NIC-verified): ~9.85 M/s on a
  single 10 G NIC (line rate, wire-bound), ~19 M/s across a dual link (~97% wire-bound).

## Known limitations

- `--clients 8` can plateau below target on a single-RX-queue interface (outstanding
  contention) — use `-c 4` for controlled-load latency, or let AF_XDP size workers
  automatically (one per bound queue, capped to NIC-local physical cores).
- Synthetic single-record workload: it isolates a server's data plane and is **not** a
  recursive-resolver or cache-miss workload.
- At very high offered rates the generator's userspace round-trip can lag the true
  served rate (a generator-recv limit, not the server's). `server_rx_qps` — the
  authoritative rate read from the egress NIC's hardware rx counter — is reported
  directly ("Server throughput (NIC rx)"), so no manual receiver-counter reading is
  needed (#8).
