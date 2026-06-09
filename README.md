# dnsmark

**A DNS benchmark tool built for honest measurement and line-rate generation. Static binary, no dependencies.**

[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/redlemonbe/dnsmark)](https://github.com/redlemonbe/dnsmark/releases/latest)
[![GitHub Sponsors](https://img.shields.io/github/sponsors/redlemonbe?style=flat&logo=github&label=Sponsor)](https://github.com/sponsors/redlemonbe)

> **Authorized testing only.**  
> Only use dnsmark against DNS servers you own or have explicit written authorization to test.  
> Read [ACCEPTABLE_USE.md](ACCEPTABLE_USE.md) before use.

---

## Why dnsmark for high-performance DNS?

- **Line-rate generation.** An optional zero-copy AF_XDP datapath drives a NIC at line
  rate when you need to saturate a fast server — beyond what a kernel-socket generator
  reaches.
- **Honest latency.** The slow tail is counted (timeouts and end-of-run in-flight go
  into the histogram), and absolute latency is validated against a wire capture rather
  than assumed.
- **Deterministic placement.** Workers pin to NIC-local **physical** cores (no HT, no
  real-time scheduling) for repeatable results.

The trade-offs and exact measurement methodology are written down, not glossed over —
see the [whitepaper](docs/WHITEPAPER.md).

---

## Documentation

- **[Whitepaper](docs/WHITEPAPER.md)** — architecture, the unified worker loop, and how latency is measured.
- **[Benchmarking methodology](docs/benchmarking.md)** — throughput at NIC counters + the wire-validated latency decomposition (§7).

---

## What you get

Established DNS load generators cover UDP, TCP, DoT and DoH; this table lists what
dnsmark adds on top.

| | established generators | dnsmark |
|---|:---:|:---:|
| UDP / TCP / DoT | ✅ | ✅ |
| Built-in auto-ramp to max QPS | a separate companion tool | ✅ `--ramp` (Dichotomic Saturation Discovery) |
| Side-by-side two-server compare | ❌ | ✅ `--compare` |
| Live TUI dashboard | ❌ | ✅ |
| p50 / p95 / p99 / p999 | average + per-query `-v` | ✅ built-in HDR histogram |
| JSON output | ❌ | ✅ `--json` |
| AF_XDP zero-copy datapath | ❌ | ✅ `--xdp` |
| Single static binary, no deps | ❌ | ✅ musl |

> Latency note: a closed-loop generator's absolute latency is never "the server's
> latency" — it includes the tool's own client-side overhead. See
> [Measurement accuracy](#measurement-accuracy--transport) and the
> [whitepaper](docs/WHITEPAPER.md). Anchor on a wire capture.

---

## Measurement accuracy & transport

dnsmark sends over a **UDP kernel socket by default** — the same datapath as a standard kernel UDP client — so latency is directly comparable. `--xdp` is **opt-in** and changes the datapath: only use it when the server under test is itself AF_XDP (a symmetric **XDP-vs-XDP** measurement) or for raw saturation throughput. **Never compare an XDP generator against a kernel server** — that is asymmetric and not publishable. The rule is simple: *the generator's datapath must match the server's.*

A closed-loop generator's reported RTT is **`server processing + network + the tool's own client-side overhead`** — only the first term belongs to the server, and the third differs between any two generators. So **absolute** latency is a property of the *(server, generator, rig)* triple, not of the server alone: validate the server's term against the **wire** (a `tcpdump` capture on the server, paired by DNS transaction id) and compare servers with **one fixed generator**.

dnsmark's client path is deliberately light (batched `recvmmsg`, a tight send/poll loop), so its figure sits close to the wire — but the **wire**, not any tool, is the reference.

See **[docs/benchmarking.md §7](docs/benchmarking.md)** for the full three-term decomposition (the tool's figure against the wire, on two rigs and both generator↔receiver directions) and the exact commands to reproduce it.

> **Concurrency (`-c`).** Each client is a dedicated OS thread. `-c auto` (the default) uses one thread per physical core (HT excluded) -- the sweet spot. Setting `-c` far above the physical core count oversubscribes the CPU and adds scheduling jitter to *both* throughput and latency; raise it only when the bottleneck is clearly elsewhere.

---

## Install

```bash
# x86_64 static (musl — no dependencies)
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-x86_64-linux-musl
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/

# x86_64 glibc (servers with glibc >= 2.17)
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-x86_64-linux-gnu
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/

# aarch64 static (Graviton, Raspberry Pi 4/5 — musl)
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-aarch64-linux-musl
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/

# aarch64 glibc
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-aarch64-linux-gnu
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/
```

> Run dnsmark on a **separate machine** from the DNS server under test.

---

## Quick start

```bash
# Find max sustainable QPS (automatic ramp)
dnsmark -s 192.0.2.1 --random --ramp

# Fixed load — 5 000 QPS for 60 s
dnsmark -s 192.0.2.1 --random -Q 5000 -l 60

# Query file (one `name [type]` per line)
dnsmark -s 192.0.2.1 -d queries.txt -l 30

# Compare two servers side by side
dnsmark -s 192.0.2.1 --compare 192.0.2.2 --random -l 30

# DNS-over-TLS
dnsmark -s 192.0.2.1 --protocol dot --random -l 30

# JSON output (CI/CD)
dnsmark -s 192.0.2.1 --random -l 10 -q --json
```

---

## AF_XDP zero-copy — 10G line-rate generation

With `--xdp`, dnsmark builds DNS query frames straight into the NIC's UMEM and
transmits them zero-copy (no kernel, no per-packet syscall). One independent
worker runs per NIC-local physical core; each owns its own queue, UMEM and rings
— no shared per-packet state. On an Intel X520 (82599) this **saturates a 10 GbE
link (~12 M qps measured — see the [benchmarking guide](docs/benchmarking.md))** and
scales per core, well beyond a kernel-socket generator.

> **`--xdp` is opt-in.** The default transport is the UDP kernel socket (a standard
> kernel UDP client). Use `--xdp` only against a server that is *itself* AF_XDP (symmetric
> measurement) or for raw saturation throughput — never an XDP generator against a
> kernel server. For latency comparisons against unbound/BIND, use the default UDP path.

```bash
# grant capabilities once (or run as root)
sudo setcap cap_net_raw,cap_net_admin,cap_bpf+eip $(which dnsmark)

# zero-copy flood, all NIC-local cores
dnsmark -s 10.0.0.2 -d queries.txt --xdp -c 8 --max-outstanding 0
```

**Requirements to reach line rate (all matter):**

1. **Physical NIC only.** XDP binds a physical interface + queue. It **cannot**
   bind a virtual interface (bond, bridge/`vmbr*`, `veth`, `macvlan`) — dnsmark
   detects these and refuses / retries the physical parent.
2. **Disable flow control on the sender NIC.** Otherwise 802.3x PAUSE frames from
   the receiver throttle TX (we measured a hard ~1.36 M pps cap that vanished once
   flow control was off, jumping to 12 M):
   ```bash
   ethtool -A <nic> rx off tx off
   ```
3. **The server's MAC must be ARP-resolvable.** If dnsmark cannot resolve it, it
   logs a loud warning and **falls back to `sendmmsg`** (kernel path, not
   zero-copy). Pin it if needed:
   ```bash
   ip neigh replace 10.0.0.2 lladdr <server-mac> dev <nic> nud permanent
   ```

### Link bonding is not supported (XDP limitation)

AF_XDP cannot transmit over a Linux **bond** — a bond is a virtual interface, and
the kernel XDP layer binds a physical NIC + queue, so it has no way to spread
frames across bond members (the second member becomes a black hole).

**Workaround — saturate 2×10G with two independent paths:** take each port out of
the bond, give each its own subnet, and run **one dnsmark instance per physical
port**:

```bash
# port A
ethtool -A enp1s0f0 rx off tx off
dnsmark -s 10.0.0.2 -d queries.txt --xdp --max-outstanding 0   # uses enp1s0f0
# port B (second terminal / host)
ethtool -A enp1s0f1 rx off tx off
dnsmark -s 10.1.0.2 -d queries.txt --xdp --max-outstanding 0   # uses enp1s0f1
```

A native multi-NIC mode (one process driving several physical ports) is on the
roadmap — see the issues.

### Benchmarking a DNS server (spread across its cores)

dnsmark varies the UDP **source port** per packet by default so the server's NIC
RSS can spread the load across its RX queues/cores. For that to work the **server's
NIC must hash UDP on L4 ports** — most NICs default to hashing IPs only, which pins
the whole single-source flood to one queue → one core:

```bash
# on the SERVER under test
ethtool -N <nic> rx-flow-hash udp4 sdfn   # hash src/dst IP + src/dst port
ethtool -A <nic> rx off tx off            # no PAUSE-frame throttling
```

Measured impact: an Intel X520 resolver went from **1 core / 448k qps** to **16
cores / 4.77M qps** just by enabling `udp4 sdfn` + the per-packet source-port
variation (the 82599's RSS caps at 16 rings). Use `DNSMARK_FIXED_SPORT=1` to pin
the source port (single-flow / single-core testing).

> 📖 **Full 10G benchmarking methodology** — NIC tuning checklist, how to read NIC
> counters correctly (AF_XDP ZC TX bypasses standard `tx_packets`), CPU-bound vs.
> fill-ring bottleneck diagnosis, gotchas and a reference result (8.83 M qps on a
> 2013 dual-Xeon):  
> **[docs/benchmarking.md](docs/benchmarking.md)**

---

## Ramp mode — Dichotomic Saturation Discovery

```bash
dnsmark -s 192.0.2.1 -d top-10000-domains.txt --xdp --ramp
```

`--ramp` doesn't just climb and stop — it runs a two-phase **Dichotomic Saturation
Discovery (DSD)** to pinpoint the *exact* saturation knee, which a plain doubling ramp
cannot do:

1. **Logarithmic discovery** — start at 100k QPS and double every step until the median
   (p50) round-trip latency breaks the SLO (1 ms). This brackets the maximum between the
   last sustained step and the first broken one in `log₂` steps.
2. **Dichotomic convergence** — binary-search inside that bracket (test the midpoint, move
   the edge toward it, repeat) until within 5 %, converging on the real knee instead of
   falling back to a power of two.

A doubling-only ramp that sees 6.4M work and 12.8M fail can only say "6.4M". DSD reports
the truth — here **11.27M** (real run: Runbound over a single 10 GbE Intel X520, warm
cache):

```
offered      rtt-samples   p50       p95       p99
   50k         21 800      0.032     9.407     9.591    ┐ exponential phase
  100k         41 802      0.038     0.071     9.535    │ (×2 from 100k)
  200k         81 527      0.193     0.256     9.431    │
  400k        161 412      0.198     0.256     8.615    │
  800k        321 243      0.240     0.320     0.364    │
  1.6M        641 088      0.163     0.247     0.282    │
  3.2M      1 279 751      0.076     0.120     0.143    │
  6.4M      2 552 748      0.079     0.108     0.155    │
 12.4M ↑    2 427 966      1.781     2.299     2.499    ┘ SLO broken (p50 > 1 ms)
 ── dichotomic convergence ──
  9.56M     3 672 929      0.129     0.237     0.699    ok
 10.88M     2 925 873      0.558     1.192     7.407    ok
 11.62M     2 652 467      1.150     1.697     1.923    breach
 11.27M     2 876 953      0.944     1.533     1.840    ok ← converged

Max offered load under p50<1ms SLO: 11 266 506 q/s
```

The **median (p50)** is the saturation signal, not p95/p99: a small fraction of forwarded
cache-misses produces large tail outliers — the 9 ms values at low load above — that are a
property of the workload, not server saturation, and would trip a tail-based test
prematurely. Each step measures its own window (the latency histogram is reset per step),
so the percentiles are the load *at that step*, never a cumulative blur.

See **[WHITEPAPER.md §5b](docs/WHITEPAPER.md)** for the full algorithm.

---

## Output

```
DNS Performance Testing Tool — dnsmark 1.0.0
[DISCLAIMER: authorized testing only]

Parameters:

  Server:       192.0.2.1:53
  Protocol:     UDP
  Clients:      10
  QPS cap:      unlimited
  Duration:     30 s
  Timeout:      3000 ms
  Mode:         fixed
  Source:       random (bench.invalid. A)

Statistics:

  Queries sent:         4 890 120
  Queries completed:    4 889 011     (99.98%)
  Queries lost:             1 109     (0.02%)

  Response codes:
    NOERROR:                    0     (0.00%)
    NXDOMAIN:           4 889 011     (100.00%)
    SERVFAIL:                   0     (0.00%)
    REFUSED:                    0     (0.00%)

  Average QPS:            163 000
  Throughput:             163 000 qps

  Latency:
    min:       0.148 ms
    avg:       1.361 ms
    p50:       1.292 ms
    p95:       2.443 ms
    p99:       3.193 ms
    p999:      3.999 ms
    max:       7.979 ms

  Run time: 30.001 s
```

## Support the project

[![GitHub Sponsors](https://img.shields.io/github/sponsors/redlemonbe?style=flat&logo=github&label=Sponsor%20on%20GitHub)](https://github.com/sponsors/redlemonbe)

**Bitcoin** — `3FP8hkkiu4kwCD1PDFgAv2oq1ZTyXwy3yy`  
**Ethereum** — `0xB5eEAf89edA4204Aa9305B068b37A93439cBb680`

---


## License

AGPL v3 — see [LICENSE](LICENSE).

---

## Part of RunASM

dnsmark is part of **[RunASM](https://www.runasm.com)** — high-performance infrastructure in Rust, with benchmarks measured at the NIC hardware counters, not asserted.
