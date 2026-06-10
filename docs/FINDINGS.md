# dnsmark — Engineering notes

Status as of **v2.0.0**. User-facing methodology lives in
[benchmarking.md](benchmarking.md) and [WHITEPAPER.md](WHITEPAPER.md); the change
history is in [CHANGELOG.md](../CHANGELOG.md).

## Latency measurement

- **Default transport is the UDP kernel socket**; `--xdp` is opt-in (symmetric
  XDP-vs-XDP, or saturation only). The generator's datapath must match the server's.
- A **unified per-worker loop** (send → `poll` → `recvmmsg` → sweep) measures RTT
  start-to-finish on one thread and one clock, removing the pre-2.0 sender/receiver
  split that added ~34 µs of context-switch latency to every sample.
- The **10 ms timeout sweep** and the **end-of-run drain** record expired and
  still-in-flight queries into the histogram at their real age, so p99/p999 are not
  truncated.
- Latency is **validated against a wire (`tcpdump`) capture**, not against another tool;
  see benchmarking.md §7 for the three-term decomposition (server + network + generator
  overhead) and exact reproduction commands.

## AF_XDP datapath

- One worker per NIC RX queue, pinned to NIC-local **physical** cores (no HT sibling, no
  real-time scheduling). Per-worker local in-flight table; **fixed source port per
  worker**. The generator steers its own RSS indirection table to span exactly the
  bound queues (`ethtool -X equal <queue_count>`) so every response lands on a bound
  worker; without this, the NIC's default RSS (spanning all HW queues) drops responses
  on unbound queues as a false ~100% loss (#8). Queue count and NUMA node are
  auto-detected.
- Requires a physical NIC, `CAP_NET_RAW`/`CAP_BPF` (or root), and flow control disabled
  on the sender to reach line rate.

## Known limitations

- `--clients 8` can plateau below target on a single-RX-queue interface (outstanding
  contention) — use `-c 4` for controlled-load latency, or let AF_XDP size workers to
  queues automatically.
- Synthetic single-record workload: it isolates a server's data plane and is **not** a
  recursive-resolver or cache-miss workload.
- At very high offered rates the generator's AF_XDP RX can concentrate on one
  queue/core and cap the *measured* round-trip rate (a generator limit, not the
  server's). Read served throughput from the **receiver** NIC counters in that
  regime (#8).
