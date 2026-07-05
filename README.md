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
- **Line-rate awareness (v2.7.1).** dnsmark states, from its own hardware observations,
  whether a run is limited by the Ethernet link or by the server. It divides the
  authoritative rate (`server_rx_qps`) by the line-rate ceiling computed from the average
  on-wire reply size and the egress-NIC link speed, and reports a **`% of line rate`** with
  a verdict: **wire-bound** (≥ 90 % — the link is the wall) or **link-headroom** (< 90 % —
  the server or the generator is the limit, not the wire). Self-contained: no receiver-side
  reading, works in AF_XDP and kernel-UDP. Since v2.7.2 the line-rate verdict is emitted
  **only for fixed/flood runs, not in `--ramp`** — under `--ramp` the DSD Capacity / Within
  SLO / Knee bracket is the throughput answer.
- **Honest latency.** Timeouts and end-of-run in-flight queries are counted as **losses**
  (`sent == completed + lost`, dnsperf-compatible), never as latency samples — the histogram
  holds real responses only, so the tail is not polluted by fabricated RTTs. Absolute latency
  is validated against a wire capture rather than assumed.
- **Deterministic placement.** Workers pin to NIC-local **physical** cores (no HT, no
  real-time scheduling) for repeatable results.
- **Read at the wire.** Throughput is read at the NIC hardware counters and reported by
  dnsmark itself as **`Server throughput (NIC rx)`** (`rx_packets + rx_missed_errors +
  rx_fifo_errors + rx_over_errors` — the ring-overflow terms recover replies the host never
  drained) — no manual `ethtool -S` step. In multi-NIC mode (repeat `-s`, one AF_XDP stack per card) a
  single dnsmark instance drives one AF_XDP stack per card, each pinned to its own NIC's
  NUMA node so two cards on separate PCIe buses scale independently. Cross-validated against
  `dnsperf` (DNS-OARC) — see [docs/cross-validation-dnsperf.md](docs/cross-validation-dnsperf.md).

The trade-offs and exact measurement methodology are written down, not glossed over —
see the [whitepaper](docs/WHITEPAPER.md).

---

## Measured — server ceilings (2026-07-03, v2.7.5)

Four resolvers in strict parity (forward+cache, DNSSEC off, 100k-domain corpus warmed to
all-cache-hit), single 10 GbE Intel X710 (i40e), driven by four generators. **Truth =
receiver NIC tx_packets ÷ 20 s** — the replies the server actually put on the wire, the
one measure that makes cross-tool numbers comparable. Peak qps over all four generators:

| Server | Peak (NIC tx) | vs BIND9 |
|---|---:|---:|
| Runbound (xdp:yes) | **12.5 M** | ~6.6× |
| Runbound (xdp:no)  | 6.8 M | ~3.6× |
| Unbound 1.22       | 3.1 M | ~1.6× |
| BIND9 9.x          | 1.9 M | 1× |

Runbound's AF_XDP fast path roughly doubles its own kernel-path ceiling (~6.8 M → ~12.5 M).

> **Generator effect (read this before quoting a headline).** The number a benchmark
> reports depends on the *generator's* load discipline as much as the server. Open-loop
> AF_XDP firehoses (dnsmark `--xdp`, kxdpgun) push a robust server hardest — but BIND9
> **collapses** under the 13 M XDP firehose (872 k / 1.03 M qps) versus 1.89 M under the
> gentler kernel-UDP load: classic receiver livelock (all CPU burned in softirq, drops rise,
> useful work falls as offered rate climbs). The receiver-NIC-tx counter captures the truth
> in every case, which is exactly why it is the reference here.

Caveats, honestly: single 20 s runs (±10–15 % run-to-run, not averaged). The 12.5 M figure
exceeds the ~9.85 M/s line rate for 103 B replies because this warm produced smaller average
replies (line rate is reply-size dependent). Full method, per-generator table, and analysis:
**[docs/benchmarking.md](docs/benchmarking.md)**.

---

## Documentation

- **[Whitepaper](docs/WHITEPAPER.md)** — architecture, the unified worker loop, how latency is measured, and the generator effect (§7a).
- **[Benchmarking methodology](docs/benchmarking.md)** — throughput at NIC counters + the wire-validated latency decomposition; the 2026-07-03 four-server reference result (§6).
- **[Cross-validation vs dnsperf](docs/cross-validation-dnsperf.md)** — dnsmark vs dnsperf across BIND9, Unbound, Runbound; the four-server campaign (§4).
- **[dnsmark vs kxdpgun](docs/dnsmark-vs-kxdpgun.md)** — the two open-loop AF_XDP generators, measured at the receiver NIC.
- **[unbound vs BIND9 methodology](docs/server-comparison-methodology.md)** — the closed-loop DSD / latency comparison method for kernel resolvers (procedure only).
- **[Empirical validation](docs/empirical-validation.md)** — how dnsmark's figures are cross-validated (NIC counters, latency vs dnsperf) — procedure only.
- **[Engineering notes](docs/FINDINGS.md)** — datapath findings and known limitations.

---

## What you get

The canonical closed-loop DNS load tool is **dnsperf** (DNS-OARC); this table lists what
dnsmark adds on top of it.

| | dnsperf | dnsmark |
|---|:---:|:---:|
| UDP / TCP / DoT | ✅ | ✅ |
| Built-in auto-ramp to max QPS | a separate companion tool | ✅ `--ramp` (Dichotomic Saturation Discovery) |
| Side-by-side two-server compare | ❌ | ✅ `--compare` |
| Live TUI dashboard | ❌ | ✅ |
| p50 / p95 / p99 / p999 | average + per-query `-v` | ✅ built-in HDR histogram |
| Wire-bound vs. server-bound verdict | ❌ | ✅ line-rate awareness (`% of line rate`) |
| JSON output | ❌ | ✅ `--json` |
| AF_XDP zero-copy datapath | ❌ | ✅ `--xdp` |
| Single static binary, no deps | ❌ | ✅ musl |

> **kxdpgun** (the AF_XDP DNS generator shipped with Knot DNS) also does zero-copy XDP **and**
> JSON output — it is the open-loop peer dnsmark is benchmarked against separately in
> [docs/dnsmark-vs-kxdpgun.md](docs/dnsmark-vs-kxdpgun.md). The table above is versus the
> mainstream closed-loop tool, not versus kxdpgun.

> Latency note: a closed-loop generator's absolute latency is never "the server's
> latency" — it includes the tool's own client-side overhead. See
> [Measurement accuracy](#measurement-accuracy--transport) and the
> [whitepaper](docs/WHITEPAPER.md). Anchor on a wire capture.

---

## Measurement accuracy & transport

dnsmark sends over a **UDP kernel socket by default** — the same datapath as a standard kernel UDP client — so latency is directly comparable. `--xdp` is **opt-in** and changes the datapath: only use it when the server under test is itself AF_XDP (a symmetric **XDP-vs-XDP** measurement) or for raw saturation throughput. **Never compare an XDP generator against a kernel server** — that is an asymmetric datapath comparison and results are not directly comparable. The rule is simple: *the generator's datapath must match the server's.*

A closed-loop generator's reported RTT is **`server processing + network + the tool's own client-side overhead`** — only the first term belongs to the server, and the third differs between any two generators. So **absolute** latency is a property of the *(server, generator, rig)* triple, not of the server alone: validate the server's term against the **wire** (a `tcpdump` capture on the server, paired by DNS transaction id) and compare servers with **one fixed generator**.

dnsmark's client path is deliberately light (batched `recvmmsg`, a tight send/poll loop), so its figure sits close to the wire — but the **wire**, not any tool, is the reference.

**Line-rate awareness (v2.7.1).** Building directly on the authoritative `server_rx_qps`,
dnsmark reports whether a run is limited by the Ethernet link or by the server. It divides
`server_rx_qps` by a line-rate ceiling computed from the **average on-wire reply size**
(`rx_bytes / rx_packets` from the same egress-NIC hardware counters) and the NIC's link
speed. The result is a `% of line rate` and a verdict: **wire-bound** at ≥ 90 % (the link is
the limit) or **link-headroom** below (the server or the generator is, not the wire). It
needs no receiver-side reading and works on both the AF_XDP and kernel-UDP paths. Since
v2.7.2 the verdict is printed **only for fixed/flood runs, not in `--ramp`**: in `--ramp`,
`server_rx_qps` averages the whole ramp-up and sits far below the peak, so a line-rate % there
would contradict the DSD's own Capacity summary — under `--ramp` the DSD Capacity / Within
SLO / Knee bracket is the throughput answer, and the line-rate line appears only in fixed/flood
mode (where `server_rx_qps` reflects one steady window). At multi-Mpps, DNS on a 10 GbE link is
line-rate bound: the ceiling is reply-size dependent (for ~100 B replies, roughly the link speed
divided by the on-wire frame size), and a **single core** can saturate a 10 G NIC in both
directions (TX + RX-count) — a 1-core unified worker and a 2-core TX/RX split give the same
throughput; the wire is the wall, not the CPU or PCIe (x8 Gen3 ~63 Gbps, 6× the 10 G link).

See **[docs/benchmarking.md §7](docs/benchmarking.md)** for the full three-term decomposition (the tool's figure against the wire, and both generator↔receiver directions) and the exact commands to reproduce it.

> **Concurrency (`-c`).** Each client is a dedicated OS thread. `-c auto` (the default) uses one thread per physical core (HT excluded), with a **floor of 8** — so on a 2-core VM `auto` is still 8 workers (a 4× oversubscription there), on purpose, to keep small hosts busy. Setting `-c` far above the physical core count oversubscribes the CPU and adds scheduling jitter to *both* throughput and latency; raise it only when the bottleneck is clearly elsewhere.

---

## Install

```bash
# x86_64 static (musl — no dependencies)
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-x86_64-unknown-linux-musl
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/

# x86_64 glibc (servers with glibc >= 2.17)
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-x86_64-unknown-linux-gnu
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/

# aarch64 static (Graviton, Raspberry Pi 4/5 — musl)
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-aarch64-unknown-linux-musl
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/

# aarch64 glibc
curl -Lo dnsmark https://github.com/redlemonbe/dnsmark/releases/latest/download/dnsmark-aarch64-unknown-linux-gnu
chmod +x dnsmark && sudo mv dnsmark /usr/local/bin/
```

> Run dnsmark on a **separate machine** from the DNS server under test.

---

## Quick start

```bash
# Find max sustainable QPS (auto-ramp — no -Q or -l needed, builtin corpus)
dnsmark -s 192.0.2.1

# Fixed load — 5 000 QPS for 60 s
dnsmark -s 192.0.2.1 -Q 5000 -l 60

# Query file (one `name [type]` per line; omit -d to use the builtin 2000-domain corpus)
dnsmark -s 192.0.2.1 -d queries.txt -l 30

# Compare two servers side by side
dnsmark -s 192.0.2.1 --compare 192.0.2.2 -l 30

# DNS-over-TLS
dnsmark -s 192.0.2.1 --protocol dot -l 30

# JSON output (CI/CD)
dnsmark -s 192.0.2.1 -l 10 -q --json
```

---

## AF_XDP zero-copy — 10G line-rate generation

With `--xdp`, dnsmark builds DNS query frames straight into the NIC's UMEM and
transmits them zero-copy (no kernel, no per-packet syscall). One independent
worker runs per NIC-local physical core; each owns its own queue, UMEM and rings
— no shared per-packet state. On a 10 GbE NIC this **saturates the link** (see the
[benchmarking guide](docs/benchmarking.md)) and scales per core, well beyond a
kernel-socket generator.

> **`--xdp` is opt-in.** The default transport is the UDP kernel socket (a standard
> kernel UDP client). Use `--xdp` only against a server that is *itself* AF_XDP (symmetric
> measurement) or for raw saturation throughput — never an XDP generator against a
> kernel server. For latency comparisons against unbound/BIND, use the default UDP path.

```bash
# grant capabilities once (or run as root)
sudo setcap cap_net_raw,cap_net_admin,cap_bpf+eip $(which dnsmark)

# zero-copy flood, all NIC-local cores
# --xdp defaults to firehose (--max-outstanding 0) and auto-confines to the NIC's NUMA
# node — no numactl, no DNSMARK_SPORT_SPREAD, no ethtool tuning needed (since v2.5.0).
dnsmark -s 10.0.0.2 -d queries.txt --xdp
```

**Requirements to reach line rate (all matter):**

1. **Physical NIC only.** XDP binds a physical interface + queue. It **cannot**
   bind a virtual interface (bond, bridge/`vmbr*`, `veth`, `macvlan`) — dnsmark
   detects these and refuses / retries the physical parent.
2. **Disable flow control on the sender NIC.** Otherwise 802.3x PAUSE frames from
   the receiver throttle TX to a hard cap that vanishes once flow control is off:
   ```bash
   ethtool -A <nic> rx off tx off
   ```
3. **The server's MAC must be ARP-resolvable.** If dnsmark cannot resolve it, it
   logs a loud warning and **falls back to `sendmmsg`** (kernel path, not
   zero-copy). Pin it if needed:
   ```bash
   ip neigh replace 10.0.0.2 lladdr <server-mac> dev <nic> nud permanent
   ```

**CPU governor.** With `--xdp`, dnsmark pins every CPU to the `performance` governor
for the run and restores the previous governor on exit, including Ctrl-C. After a
hard kill or a crash (the binary is built with `panic = "abort"`) the restore cannot
run — if a crashed run left cores pinned, reset them:
`echo schedutil | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor`.

### Tagged VLAN (experimental)

`DNSMARK_VLAN=<vid>` injects one 802.1Q tag into the generated frames (needed when
the provider delivers the private network tagged). AF_XDP zero-copy cannot bind a
VLAN sub-interface, so dnsmark binds the **physical parent** and bakes the tag into
the frame template. **Experimental**: the frame layout is unit-tested and a tagged
resolver round trip is proven end-to-end, but tagged generation has not been
validated at line rate (no zero-copy-capable tagged NIC was available — see
CHANGELOG 2.2.0).

### Link bonding is not supported (XDP limitation)

AF_XDP cannot transmit over a Linux **bond** — a bond is a virtual interface, and
the kernel XDP layer binds a physical NIC + queue, so it has no way to spread
frames across bond members (the second member becomes a black hole).

**Native multi-NIC — saturate 2×10G from one process.** Repeat `-s`, one target per
physical port (each on its own subnet routed via its own NIC). A single dnsmark instance
drives one AF_XDP stack per card; since v2.5.0 each stack pins to **its own NIC's NUMA
node** (per-node core cursor), so two cards on separate PCIe buses / sockets scale
independently:

```bash
# one instance, two cards (X710 on node 1 + X520 on node 0) — no numactl, no per-NIC tuning
dnsmark -s 10.71.10.1 -s 10.51.10.1 -d queries.txt --xdp --nic-stats
```

Each target must route via a distinct NIC (dnsmark refuses two targets on the same
interface). Link **bonding** is still unsupported (a bond is virtual; XDP binds a physical
NIC+queue) — use the per-port targets above instead of a bond.

### Benchmarking a DNS server (spread across its cores)

dnsmark spreads load across the server's RX queues/cores through **source-port
diversity**: on the default path each `-c` worker owns a kernel socket with its own
source port; with `--xdp` each worker *cycles* its UDP source port over a wide range
(`10000 + (counter mod 2048)`, with a per-worker phase offset) — a fixed one-port-per-worker
scheme was dropped because a handful of flows collapsed the receiver's RSS onto a few queues.
The result is thousands of distinct flows fanned across the server's NIC RSS. Use enough
workers to light up the server's queues. For RSS
to use ports at all, the **server's NIC must hash UDP on L4 ports** — most NICs
default to hashing IPs only, which pins the whole single-source flood to one
queue → one core:

```bash
# on the SERVER under test
ethtool -N <nic> rx-flow-hash udp4 sdfn   # hash src/dst IP + src/dst port
ethtool -A <nic> rx off tx off            # no PAUSE-frame throttling
```

Enabling `udp4 sdfn` lets the source-port diversity reach all of the server NIC's RX
rings instead of pinning the whole single-source flood to one queue → one core (the
82599's RSS caps at 16 rings). For a single-flow / single-core test, run a single
worker (`-c 1`).

**Generator-side AF_XDP RX (responses) — accurate since v2.5.0.** With `--xdp`, dnsmark
also *receives* responses over AF_XDP. Earlier versions funnelled all responses onto a
single queue (`ethtool -X equal 1`) and matched them per-worker, so one TX-busy worker
drained only a fraction of the arriving replies → the headline round-trip under-counted a
fast server (#15). Since v2.5.0 the firehose path spreads the RETA across **all** worker
queues, and each worker counts the responses on its own queue (no cross-core match), so the
round-trip tracks the server's real reply rate. No manual tuning is required.

In addition, dnsmark reports an **authoritative `Server throughput (NIC rx)`** =
`rx_packets + rx_missed_errors` on the return NIC(s): this counts every reply that reached
the wire, even those the kernel later drops at the socket, so it equals the server's real
reply rate without reading the remote host. When the userspace round-trip is lower (kernel
socket / NIC-ring overflow, or a non-NUMA-local stack), dnsmark prints a NOTE and the NIC-rx
figure is the truth.

> 📖 **Full 10G benchmarking methodology** — NIC tuning checklist, how to read NIC
> counters correctly (AF_XDP ZC TX bypasses standard `tx_packets`), CPU-bound vs.
> fill-ring bottleneck diagnosis, and gotchas:  
> **[docs/benchmarking.md](docs/benchmarking.md)**

---

## Ramp mode — Dichotomic Saturation Discovery

```bash
# auto-DSD (default when no -Q or -l)
dnsmark -s 192.0.2.1

# explicit ramp with a custom corpus
dnsmark -s 192.0.2.1 -d top-10000-domains.txt --xdp --ramp
```

`--ramp` doesn't just climb and stop — it runs a two-phase **Dichotomic Saturation
Discovery (DSD)** to pinpoint the *exact* saturation knee, which a plain doubling ramp
cannot do:

1. **Logarithmic discovery** — start at 100k QPS and double every step until the median
   (p50) round-trip latency breaks the SLO (auto-computed as `max(3 × floor, floor + 1 ms)`,
   where floor is the minimum p50 observed at low load — so once the floor exceeds ~500 µs the
   `3×` term dominates, not the `+1 ms`). This brackets the maximum in `log₂` steps.
2. **Dichotomic convergence** — binary-search inside that bracket (test the midpoint, move
   the edge toward it, repeat) until within 5 %, converging on the real knee instead of
   falling back to a power of two.

A doubling-only ramp that sees a rate work and its double fail can only report the lower
power of two. DSD bisects the gap and reports the real knee. At the end it prints the idle
latency floor, the NIC-verified Capacity, the Within-SLO rate, and the DSD knee bracket:

```
  Idle latency:  <floor> ms   (floor — minimum p50 observed)
  Capacity:      <max>        (NIC-verified — max replies/s on the wire)
  Within SLO:    <rate>       (p50 stays under the SLO at this rate)
Knee bracket (DSD bisection): [<low> ; <high>] q/s  (±%)
```

The **median (p50)** is the saturation signal, not p95/p99: a small fraction of forwarded
cache-misses produces large tail outliers that are a property of the workload, not server
saturation, and would trip a tail-based test prematurely. Each step measures its own window
(the latency histogram is reset per step), so the percentiles are the load *at that step*,
never a cumulative blur.

**What the DSD Capacity means depends on the datapath (v2.7.3).** The `--xdp` ramp is an
**open-loop firehose with a lossless zero-copy RX**, so its Capacity genuinely *is* the max
replies/s on the wire (labelled *NIC-verified*). The kernel-UDP ramp (default transport /
auto-DSD) is a different animal: it is a **gated closed loop** (dnsperf-comparable,
latency-honest), and the generator's own kernel receive path drops replies under load, which
clogs the outstanding slots and caps the *offered* rate well below the server's real capacity.
So the kernel-UDP DSD figure is the **closed-loop SLO knee, not the server's raw ceiling**, and
is labelled accordingly:

```
  Capacity:      <knee>  (closed-loop knee — kernel-recv bound, NOT the server's raw max)
  -> for the raw ceiling, run open-loop:  dnsmark -s <ip> -Q 0 --max-outstanding 0
     (reports "Server throughput (NIC rx)" = server_rx_qps, the authoritative reply rate)
```

kernel-UDP is **generator-recv bound** — the generator's own kernel recv drops replies under
load, so the userspace round-trip counts fewer than reach the wire — and `--xdp` exists to
remove that with a lossless RX. The `--xdp` ramp is unaffected by this closed-loop cap; its
Capacity is the real wire ceiling.

See **[WHITEPAPER.md §5b](docs/WHITEPAPER.md)** for the full algorithm.

---

## Output

A real `--xdp` firehose run against a warmed resolver on a single 10 GbE X710 (latency reads
`0.000 ms` — an open-loop firehose does not sample RTT; use a fixed-load or `--ramp` run for
latency):

```
DNS Performance Testing Tool — dnsmark 2.7.5
[DISCLAIMER: authorized testing only]

Parameters:

  Server:       192.0.2.1:53
  Protocol:     UDP
  Clients:      20
  QPS cap:      13000000
  Duration:     12 s
  Timeout:      3000 ms
  Mode:         fixed
  Source:       file (corpus-100k.txt)

Statistics:

  Queries sent:         145251413
  Queries completed:    118209415   (81.38%)
  Queries lost:         27041998    (18.62%)

  Response codes:
    NOERROR:            118190692   (99.98%)
    NXDOMAIN:           13055       (0.01%)
    SERVFAIL:           5668        (0.00%)
    REFUSED:            0           (0.00%)

  Send throughput (egress):  12099464 qps  ← match this to rx_packets on receiver NIC
  Wire egress (NIC PHY):     12098517 qps  (confirmed transmitted)
  Round-trip completed:      9846874 qps  (81.4% of egress, userspace)
  Server throughput (NIC rx):  9850308 qps  (authoritative — replies on the wire)
  Line rate:                 100% of 10 Gb/s wire  (103 B replies, ceiling 9.85 M/s)
    → WIRE-BOUND: the Ethernet link is saturated — this IS the max for this
      reply size on this link. Use faster/more NICs (25/40/100G) to push higher.

  Latency:
    min:       0.000 ms
    avg:       0.000 ms
    p50:       0.000 ms
    p95:       0.000 ms
    p99:       0.000 ms
    p999:      0.000 ms
    max:       0.000 ms

  Run time: 12.005 s
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
