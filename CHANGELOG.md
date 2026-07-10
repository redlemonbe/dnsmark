# Changelog

## [1.0.0] - 2026-07-05

Initial public release. A DNS benchmark tool built for honest measurement and
line-rate generation — a single static binary with no dependencies.

### Transports
- UDP over a kernel socket (default), TCP, and DNS-over-TLS (`--protocol dot`).
- Optional zero-copy **AF_XDP** datapath (`--xdp`): DNS query frames are built
  straight into the NIC's UMEM and transmitted with no per-packet syscall. One
  independent worker per NIC-local physical core, each owning its own queue, UMEM
  and rings. Saturates a 10 GbE link and scales per core.
- Native multi-NIC: repeat `-s`, one AF_XDP stack per card, each pinned to its own
  NIC's NUMA node, so two cards on separate PCIe buses scale independently.

### Load modes
- Auto-ramp by default (no `-Q`/`-l`): **Dichotomic Saturation Discovery (DSD)** —
  logarithmic bracketing followed by bisection to the real saturation knee.
- The `--ramp` p50 SLO is auto-calculated from the measured latency floor
  (`max(3 × floor, floor + 1 ms)`), never hardcoded.
- Fixed load (`-Q`, `-l`) and open-loop firehose (`--max-outstanding 0`).
- `--compare` runs two servers side by side.

### Measurement
- Throughput read at the receiver NIC hardware counters and reported as
  **`Server throughput (NIC rx)`** — the replies the server actually put on the
  wire, recovering ring-overflow drops the host never drained.
- Line-rate awareness: reports `% of line rate` and a **wire-bound** vs
  **link-headroom** verdict (fixed/flood runs only, not in `--ramp`).
- Honest latency: timeouts and end-of-run in-flight queries are counted as losses
  (`sent == completed + lost`, dnsperf-compatible), never as latency samples.
  Built-in HDR histogram: p50 / p95 / p99 / p999.
- Deterministic placement: workers pin to NIC-local physical cores (HT excluded).

### Output & operation
- Live TUI dashboard; `--json` output for CI/CD.
- Built-in 2000-domain corpus, or `-d <file>` for a custom query set.
- Auto warm-up before the measurement window (default 5 s, `DNSMARK_WARMUP`),
  skipped in `--ramp` and for very short runs.
- With `--xdp`, pins every CPU to the `performance` governor for the run and
  restores the previous governor on exit (including Ctrl-C).
- Tagged VLAN generation (experimental, `DNSMARK_VLAN`).
- Static musl binary for x86_64 and aarch64; glibc builds also provided.
